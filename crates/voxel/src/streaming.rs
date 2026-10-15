//! Bookkeeping for background chunk generation and meshing.
//!
//! The pipeline decides *what* to generate/mesh and validates results; it never runs
//! work itself. [`GenJob`]/[`MeshJob`] own immutable inputs (a generator handle, a chunk
//! snapshot), so the caller can run them on any thread — the client uses Bevy's
//! `AsyncComputeTaskPool`, tests run them inline in any order. Results come back through
//! [`ChunkPipeline::finish_generation`] / [`ChunkPipeline::finish_meshing`] on the thread
//! that owns the [`VoxelWorld`], where obsolete results are discarded:
//!
//! - every job carries a ticket; a result is only accepted while its ticket is the one
//!   recorded for that position (cancelled, unloaded or reset positions drop it);
//! - mesh jobs also carry the [`MeshKey`] (revisions of the chunk and its 6 face
//!   neighbours) they were built from; revisions are never reused, so any edit,
//!   neighbour change or unload/reload since the snapshot makes the result stale.
//!
//! Chunk lifecycle (derived from the maps below, not stored separately):
//! requested (in range, missing) → generating (`generating`) → generated (loaded, dirty)
//! → meshing (`meshing`) → ready (`applied` key == current key) → unloaded.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::chunk::Chunk;
use crate::coords::ChunkPos;
use crate::generation::WorldGenerator;
use crate::meshing::{ChunkMesh, ChunkSnapshot, mesh_snapshot};
use crate::registry::BlockRegistry;
use crate::world::{NEIGHBOURS, VoxelWorld};

/// Streaming limits. All counts are per [`ChunkPipeline`] call ("tick").
#[derive(Debug, Clone)]
pub struct StreamConfig {
    /// Horizontal chunk radius kept loaded around the centre.
    pub radius: i32,
    /// Vertical chunk radius kept loaded around the centre.
    pub radius_y: i32,
    /// Maximum generation jobs in flight.
    pub max_generating: usize,
    /// Maximum mesh jobs in flight.
    pub max_meshing: usize,
    /// Maximum new jobs of each kind admitted per tick.
    pub max_admit_per_tick: usize,
    /// Maximum completed meshes the caller should apply per tick.
    pub max_apply_per_tick: usize,
}

impl Default for StreamConfig {
    fn default() -> Self {
        Self {
            radius: 6,
            radius_y: 2,
            max_generating: 8,
            max_meshing: 8,
            max_admit_per_tick: 8,
            max_apply_per_tick: 12,
        }
    }
}

/// Identity of one job; unique for the lifetime of a pipeline.
pub type Ticket = u64;

/// Revisions of a chunk and its 6 face neighbours (`None` = not loaded) at snapshot time.
pub type MeshKey = [Option<u64>; 7];

/// Current [`MeshKey`] of a loaded chunk.
pub fn mesh_key(world: &VoxelWorld, pos: ChunkPos) -> Option<MeshKey> {
    let mut key = [world.revision(pos)?; 7].map(Some);
    for (slot, d) in key[1..].iter_mut().zip(NEIGHBOURS) {
        *slot = world.revision(pos.offset(d.0, d.1, d.2));
    }
    Some(key)
}

/// Background generation of one chunk.
pub struct GenJob {
    pub pos: ChunkPos,
    pub ticket: Ticket,
    generator: Arc<dyn WorldGenerator>,
}

impl GenJob {
    pub fn run(self) -> GenResult {
        let start = Instant::now();
        let chunk = self.generator.generate(self.pos);
        GenResult {
            pos: self.pos,
            ticket: self.ticket,
            chunk,
            duration: start.elapsed(),
        }
    }
}

pub struct GenResult {
    pub pos: ChunkPos,
    pub ticket: Ticket,
    chunk: Chunk,
    pub duration: Duration,
}

/// Background meshing of one chunk snapshot.
pub struct MeshJob {
    pub pos: ChunkPos,
    pub ticket: Ticket,
    key: MeshKey,
    snapshot: ChunkSnapshot,
    registry: Arc<BlockRegistry>,
}

impl MeshJob {
    pub fn run(self) -> MeshResult {
        let start = Instant::now();
        let mut mesh = ChunkMesh::default();
        mesh_snapshot(&self.snapshot, &self.registry, &mut mesh);
        MeshResult {
            pos: self.pos,
            ticket: self.ticket,
            key: self.key,
            mesh,
            duration: start.elapsed(),
        }
    }
}

pub struct MeshResult {
    pub pos: ChunkPos,
    pub ticket: Ticket,
    key: MeshKey,
    mesh: ChunkMesh,
    pub duration: Duration,
}

/// Rolling window of recent durations, for averages and tail latency.
#[derive(Debug, Default, Clone)]
pub struct LatencyStats {
    samples: VecDeque<Duration>,
}

impl LatencyStats {
    const WINDOW: usize = 256;

    pub fn record(&mut self, d: Duration) {
        if self.samples.len() == Self::WINDOW {
            self.samples.pop_front();
        }
        self.samples.push_back(d);
    }

    pub fn len(&self) -> usize {
        self.samples.len()
    }

    pub fn is_empty(&self) -> bool {
        self.samples.is_empty()
    }

    pub fn average(&self) -> Duration {
        match self.samples.len() {
            0 => Duration::ZERO,
            n => self.samples.iter().sum::<Duration>() / n as u32,
        }
    }

    /// Nearest-rank percentile, `p` in `0.0..=1.0`.
    pub fn percentile(&self, p: f64) -> Duration {
        let mut sorted: Vec<Duration> = self.samples.iter().copied().collect();
        sorted.sort_unstable();
        let Some(last) = sorted.len().checked_sub(1) else {
            return Duration::ZERO;
        };
        sorted[((last as f64) * p.clamp(0.0, 1.0)).round() as usize]
    }

    pub fn max(&self) -> Duration {
        self.samples.iter().copied().max().unwrap_or_default()
    }
}

/// Counters for the debug overlay.
#[derive(Debug, Default, Clone)]
pub struct PipelineStats {
    /// In range and missing, waiting for a generation slot.
    pub pending_generation: usize,
    pub active_generation: usize,
    /// Dirty chunks waiting for a meshing slot.
    pub pending_meshing: usize,
    pub active_meshing: usize,
    /// Generation jobs dropped because their chunk left the range before completion.
    pub cancelled_generation: u64,
    /// Results rejected as obsolete (wrong ticket, already loaded, or changed source).
    pub stale_generation: u64,
    pub stale_meshes: u64,
    pub generation: LatencyStats,
    pub meshing: LatencyStats,
    /// Main-thread time spent applying results (recorded by the caller).
    pub apply: LatencyStats,
}

/// Schedules and validates background chunk work. See the module docs.
pub struct ChunkPipeline {
    pub config: StreamConfig,
    pub stats: PipelineStats,
    next_ticket: Ticket,
    tick: u64,
    generating: HashMap<ChunkPos, Ticket>,
    meshing: HashMap<ChunkPos, (Ticket, MeshKey)>,
    /// Key of the mesh currently shown for each chunk.
    applied: HashMap<ChunkPos, MeshKey>,
    /// Tick at which each dirty chunk was first seen, for starvation-free ordering.
    dirty_since: HashMap<ChunkPos, u64>,
    scratch: Vec<ChunkPos>,
}

fn dist2(a: ChunkPos, b: ChunkPos) -> i64 {
    let (dx, dy, dz) = (
        i64::from(a.x - b.x),
        i64::from(a.y - b.y),
        i64::from(a.z - b.z),
    );
    dx * dx + dy * dy + dz * dz
}

impl ChunkPipeline {
    pub fn new(config: StreamConfig) -> Self {
        Self {
            config,
            stats: PipelineStats::default(),
            next_ticket: 0,
            tick: 0,
            generating: HashMap::new(),
            meshing: HashMap::new(),
            applied: HashMap::new(),
            dirty_since: HashMap::new(),
            scratch: Vec::new(),
        }
    }

    fn ticket(&mut self) -> Ticket {
        self.next_ticket += 1;
        self.next_ticket
    }

    /// Whether `ticket` is still the live generation job for `pos` (else drop its task).
    pub fn is_current_generation(&self, pos: ChunkPos, ticket: Ticket) -> bool {
        self.generating.get(&pos) == Some(&ticket)
    }

    /// Whether `ticket` is still the live mesh job for `pos` (else drop its task).
    pub fn is_current_mesh(&self, pos: ChunkPos, ticket: Ticket) -> bool {
        self.meshing.get(&pos).is_some_and(|(t, _)| *t == ticket)
    }

    /// Forgets all in-flight work, e.g. after the world was replaced by a reload. Late
    /// results are then rejected by ticket.
    pub fn reset(&mut self) {
        self.generating.clear();
        self.meshing.clear();
        self.applied.clear();
        self.dirty_since.clear();
    }

    /// Unloads chunks beyond the range (+1 chunk hysteresis), cancels generation that is
    /// no longer needed, loads stashed edited chunks directly, and admits new generation
    /// jobs nearest `center` first within the concurrency and admission limits.
    pub fn update(&mut self, world: &mut VoxelWorld, center: ChunkPos) -> Vec<GenJob> {
        self.tick += 1;
        let (r, ry) = (self.config.radius, self.config.radius_y);
        let within = move |p: ChunkPos, slack: i32| {
            (p.x - center.x).abs() <= r + slack
                && (p.z - center.z).abs() <= r + slack
                && (p.y - center.y).abs() <= ry + slack
        };

        let mut scratch = std::mem::take(&mut self.scratch);
        scratch.clear();
        scratch.extend(world.loaded_positions().filter(|p| !within(*p, 1)));
        for p in &scratch {
            world.unload_chunk(*p);
            self.meshing.remove(p);
            self.applied.remove(p);
            self.dirty_since.remove(p);
        }
        let before = self.generating.len();
        self.generating.retain(|p, _| within(*p, 1));
        self.stats.cancelled_generation += (before - self.generating.len()) as u64;

        scratch.clear();
        for y in -ry..=ry {
            for z in -r..=r {
                for x in -r..=r {
                    let p = center.offset(x, y, z);
                    if world.is_loaded(p) || self.generating.contains_key(&p) {
                        continue;
                    }
                    if world.has_stashed(p) {
                        world.load_chunk(p); // edits beat the generator; no job needed
                    } else {
                        scratch.push(p);
                    }
                }
            }
        }
        scratch.sort_unstable_by_key(|p| dist2(*p, center));
        let free = self
            .config
            .max_generating
            .saturating_sub(self.generating.len());
        let admit = free.min(self.config.max_admit_per_tick).min(scratch.len());
        let mut jobs = Vec::with_capacity(admit);
        for &pos in &scratch[..admit] {
            let ticket = self.ticket();
            self.generating.insert(pos, ticket);
            jobs.push(GenJob {
                pos,
                ticket,
                generator: world.generator(),
            });
        }
        self.stats.pending_generation = scratch.len() - admit;
        self.stats.active_generation = self.generating.len();
        self.scratch = scratch;
        jobs
    }

    /// Installs a finished chunk if its job is still current. Returns whether it was used.
    pub fn finish_generation(&mut self, world: &mut VoxelWorld, result: GenResult) -> bool {
        if !self.is_current_generation(result.pos, result.ticket) {
            self.stats.stale_generation += 1;
            return false;
        }
        self.generating.remove(&result.pos);
        self.stats.active_generation = self.generating.len();
        self.stats.generation.record(result.duration);
        // Refused if the chunk got loaded meanwhile; a stashed edit replaces the result.
        let used = world.insert_generated(result.pos, result.chunk);
        if !used {
            self.stats.stale_generation += 1;
        }
        used
    }

    /// Snapshots dirty chunks and returns mesh jobs, nearest `center` first with aging
    /// (one tick of waiting offsets one chunk² of distance), at most one job per chunk.
    pub fn schedule_meshing(
        &mut self,
        world: &mut VoxelWorld,
        registry: &Arc<BlockRegistry>,
        center: ChunkPos,
    ) -> Vec<MeshJob> {
        let tick = self.tick;
        self.dirty_since.retain(|p, _| world.is_dirty(*p));
        let mut scratch = std::mem::take(&mut self.scratch);
        scratch.clear();
        for p in world.dirty_positions() {
            self.dirty_since.entry(p).or_insert(tick);
            // A chunk with a job in flight stays dirty until that job reports back.
            if !self.meshing.contains_key(&p) {
                scratch.push(p);
            }
        }
        // Nothing changed since the shown mesh was built: no work needed.
        scratch.retain(|p| {
            let unchanged = self
                .applied
                .get(p)
                .is_some_and(|k| mesh_key(world, *p).as_ref() == Some(k));
            if unchanged {
                world.clear_dirty(*p);
            }
            !unchanged
        });
        let since = &self.dirty_since;
        scratch.sort_unstable_by_key(|p| {
            let waited = tick - since.get(p).copied().unwrap_or(tick);
            (dist2(*p, center) - waited as i64, *p)
        });
        let free = self.config.max_meshing.saturating_sub(self.meshing.len());
        let admit = free.min(self.config.max_admit_per_tick).min(scratch.len());
        let mut jobs = Vec::with_capacity(admit);
        for &pos in &scratch[..admit] {
            let (Some(key), Some(snapshot)) =
                (mesh_key(world, pos), ChunkSnapshot::capture(world, pos))
            else {
                continue;
            };
            world.clear_dirty(pos);
            self.dirty_since.remove(&pos);
            let ticket = self.ticket();
            self.meshing.insert(pos, (ticket, key));
            jobs.push(MeshJob {
                pos,
                ticket,
                key,
                snapshot,
                registry: registry.clone(),
            });
        }
        self.stats.pending_meshing = scratch.len() - admit;
        self.stats.active_meshing = self.meshing.len();
        self.scratch = scratch;
        jobs
    }

    /// Returns the mesh if it was built from the chunk's current data; otherwise drops it
    /// (re-marking the chunk dirty if it is still loaded, so it gets rebuilt).
    pub fn finish_meshing(
        &mut self,
        world: &mut VoxelWorld,
        result: MeshResult,
    ) -> Option<ChunkMesh> {
        if !self.is_current_mesh(result.pos, result.ticket) {
            self.stats.stale_meshes += 1;
            return None;
        }
        self.meshing.remove(&result.pos);
        self.stats.active_meshing = self.meshing.len();
        self.stats.meshing.record(result.duration);
        if mesh_key(world, result.pos) != Some(result.key) {
            self.stats.stale_meshes += 1;
            world.request_remesh(result.pos);
            return None;
        }
        self.applied.insert(result.pos, result.key);
        Some(result.mesh)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coords::BlockPos;
    use crate::registry::BlockId;
    use crate::registry::tests::test_registry;
    use crate::world::tests::Flat;

    fn setup() -> (VoxelWorld, ChunkPipeline, Arc<BlockRegistry>) {
        let world = VoxelWorld::new(0, Arc::new(Flat));
        let config = StreamConfig {
            radius: 1,
            radius_y: 1,
            max_generating: 64,
            max_meshing: 64,
            max_admit_per_tick: 64,
            max_apply_per_tick: 64,
        };
        (world, ChunkPipeline::new(config), Arc::new(test_registry()))
    }

    const ORIGIN: ChunkPos = ChunkPos::new(0, 0, 0);

    /// Runs every generation job to completion, newest first (order must not matter).
    fn generate_all(w: &mut VoxelWorld, p: &mut ChunkPipeline, center: ChunkPos) {
        loop {
            let jobs = p.update(w, center);
            if jobs.is_empty() {
                break;
            }
            for job in jobs.into_iter().rev() {
                assert!(p.finish_generation(w, job.run()));
            }
        }
    }

    fn mesh_all(w: &mut VoxelWorld, p: &mut ChunkPipeline, reg: &Arc<BlockRegistry>) {
        loop {
            let jobs = p.schedule_meshing(w, reg, ORIGIN);
            if jobs.is_empty() {
                break;
            }
            for job in jobs {
                assert!(p.finish_meshing(w, job.run()).is_some());
            }
        }
    }

    #[test]
    fn generated_chunks_have_expected_data() {
        let (mut w, mut p, _) = setup();
        generate_all(&mut w, &mut p, ORIGIN);
        assert_eq!(w.loaded_count(), 27);
        let below = ChunkPos::new(1, -1, 0);
        assert_eq!(
            w.chunk(below).unwrap().blocks(),
            Flat.generate(below).blocks()
        );
        assert_eq!(p.stats.generation.len(), 27);
    }

    #[test]
    fn repeated_requests_do_not_duplicate_work() {
        let (mut w, mut p, _) = setup();
        p.config.max_admit_per_tick = 5;
        let mut seen = std::collections::HashSet::new();
        let mut pending = Vec::new();
        for _ in 0..10 {
            for job in p.update(&mut w, ORIGIN) {
                assert!(seen.insert(job.pos), "duplicate job for {:?}", job.pos);
                pending.push(job);
            }
        }
        assert_eq!(seen.len(), 27);
        // Nearest first: the centre chunk is admitted in the first batch.
        assert_eq!(pending[0].pos, ORIGIN);
        for job in pending {
            assert!(p.finish_generation(&mut w, job.run()));
        }
        assert!(p.update(&mut w, ORIGIN).is_empty());
    }

    #[test]
    fn concurrency_and_admission_are_bounded() {
        let (mut w, mut p, _) = setup();
        p.config.max_generating = 4;
        p.config.max_admit_per_tick = 3;
        assert_eq!(p.update(&mut w, ORIGIN).len(), 3);
        assert_eq!(p.update(&mut w, ORIGIN).len(), 1);
        assert!(p.update(&mut w, ORIGIN).is_empty());
        assert_eq!(p.stats.active_generation, 4);
        assert_eq!(p.stats.pending_generation, 23);
    }

    #[test]
    fn generation_for_a_left_area_is_cancelled_and_discarded() {
        let (mut w, mut p, _) = setup();
        let jobs = p.update(&mut w, ORIGIN);
        let far = ChunkPos::new(100, 0, 0);
        p.update(&mut w, far); // player moved away before the jobs finished
        assert_eq!(p.stats.cancelled_generation, 27);
        for job in jobs {
            assert!(!p.finish_generation(&mut w, job.run()));
        }
        assert!(!w.is_loaded(ORIGIN));
        assert_eq!(p.stats.stale_generation, 27);
    }

    #[test]
    fn edits_made_while_generation_is_pending_survive() {
        let (mut w, mut p, _) = setup();
        let jobs = p.update(&mut w, ORIGIN);
        // The chunk gets loaded and edited by another path before its job completes.
        let b = BlockPos::new(1, -1, 1);
        w.load_chunk(b.chunk());
        w.set_block(b, BlockId::AIR).unwrap();
        for job in jobs {
            p.finish_generation(&mut w, job.run());
        }
        assert_eq!(w.get_block(b), BlockId::AIR);
        assert_eq!(p.stats.stale_generation, 1);
    }

    #[test]
    fn stale_mesh_is_discarded_and_rebuilt() {
        let (mut w, mut p, reg) = setup();
        generate_all(&mut w, &mut p, ORIGIN);
        mesh_all(&mut w, &mut p, &reg);
        let b = BlockPos::new(5, -3, 5); // interior: dirties only its own chunk
        w.set_block(b, BlockId::AIR).unwrap(); // version N
        let jobs = p.schedule_meshing(&mut w, &reg, ORIGIN);
        assert_eq!(jobs.len(), 1);
        w.set_block(b.offset(1, 0, 0), BlockId::AIR).unwrap(); // version N+1 while meshing
        // Still dirty, but no second job while one is in flight.
        assert!(p.schedule_meshing(&mut w, &reg, ORIGIN).is_empty());
        let old = jobs.into_iter().next().unwrap().run();
        assert!(p.finish_meshing(&mut w, old).is_none());
        assert_eq!(p.stats.stale_meshes, 1);
        let jobs = p.schedule_meshing(&mut w, &reg, ORIGIN);
        assert_eq!(jobs.len(), 1);
        let mesh = p
            .finish_meshing(&mut w, jobs.into_iter().next().unwrap().run())
            .unwrap();
        // The applied mesh matches a synchronous mesh of the latest data.
        let mut expected = ChunkMesh::default();
        crate::meshing::mesh_chunk(&w, &reg, b.chunk(), &mut expected);
        assert_eq!(mesh.face_count(), expected.face_count());
        // Unchanged source data is not remeshed again.
        w.request_remesh(b.chunk());
        assert!(p.schedule_meshing(&mut w, &reg, ORIGIN).is_empty());
        assert!(!w.is_dirty(b.chunk()));
    }

    #[test]
    fn unloaded_chunks_never_receive_old_meshes() {
        let (mut w, mut p, reg) = setup();
        generate_all(&mut w, &mut p, ORIGIN);
        let jobs = p.schedule_meshing(&mut w, &reg, ORIGIN);
        p.update(&mut w, ChunkPos::new(100, 0, 0)); // everything unloads
        for job in jobs {
            assert!(p.finish_meshing(&mut w, job.run()).is_none());
        }
        assert_eq!(w.loaded_count(), 0);
    }

    #[test]
    fn reload_incarnation_invalidates_in_flight_mesh() {
        let (mut w, mut p, reg) = setup();
        generate_all(&mut w, &mut p, ORIGIN);
        let jobs = p.schedule_meshing(&mut w, &reg, ORIGIN);
        // Unload and reload the same coordinates with identical content behind the
        // pipeline's back: the ticket is still live, but the incarnation differs.
        let target = ChunkPos::new(0, -1, 0);
        w.unload_chunk(target);
        w.load_chunk(target);
        let mut rejected = 0;
        for job in jobs {
            let pos = job.pos;
            if p.finish_meshing(&mut w, job.run()).is_none() {
                rejected += 1;
                assert!(w.is_dirty(pos), "{pos:?} must be rebuilt");
            }
        }
        // The chunk itself and its 5 loaded neighbours ((0,-2,0) is out of range).
        assert_eq!(rejected, 6);
    }

    #[test]
    fn boundary_edit_invalidates_neighbour_geometry() {
        let (mut w, mut p, reg) = setup();
        generate_all(&mut w, &mut p, ORIGIN);
        mesh_all(&mut w, &mut p, &reg);
        // Edit east's low-X border so east (and its west neighbour) get snapshotted, then
        // dig the touching block at x = 15 in (0,-1,0) while east's job is in flight.
        let east = ChunkPos::new(1, -1, 0);
        w.set_block(BlockPos::new(16, -1, 0), BlockId::AIR).unwrap();
        let jobs = p.schedule_meshing(&mut w, &reg, ORIGIN);
        assert!(jobs.iter().any(|j| j.pos == east));
        w.set_block(BlockPos::new(15, -2, 0), BlockId::AIR).unwrap();
        for job in jobs {
            let pos = job.pos;
            let r = p.finish_meshing(&mut w, job.run());
            if pos == east {
                assert!(r.is_none(), "neighbour edit must invalidate");
            }
        }
        assert!(w.is_dirty(east));
        mesh_all(&mut w, &mut p, &reg);
        // The exposed face of (16,-2,0) towards the hole is now part of east's mesh.
        let mut m = ChunkMesh::default();
        crate::meshing::mesh_chunk(&w, &reg, east, &mut m);
        assert!(m.normals.contains(&[-1.0, 0.0, 0.0]));
    }

    #[test]
    fn saving_during_background_work_is_consistent() {
        let reg = test_registry();
        let (mut w, mut p, _) = setup();
        generate_all(&mut w, &mut p, ORIGIN);
        let edit = BlockPos::new(2, -3, 2);
        w.set_block(edit, BlockId::AIR).unwrap();
        // Move so new generation jobs are in flight while saving.
        let jobs = p.update(&mut w, ChunkPos::new(1, 0, 0));
        assert!(!jobs.is_empty());
        let dir = std::env::temp_dir().join(format!("rbvg_stream_save_{}", std::process::id()));
        crate::persistence::save_world(&dir, &w, &reg, None, &Default::default()).unwrap();
        let during = crate::persistence::load_chunks(&dir, &reg).unwrap();
        for job in jobs {
            p.finish_generation(&mut w, job.run());
        }
        crate::persistence::save_world(&dir, &w, &reg, None, &Default::default()).unwrap();
        let after = crate::persistence::load_chunks(&dir, &reg).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(during.len(), 1, "only the edited chunk is saved");
        assert_eq!(during.len(), after.len());
        assert_eq!(during[0].1.blocks(), after[0].1.blocks());
        assert_eq!(during[0].1.get(edit.local()), BlockId::AIR);
    }

    #[test]
    fn latency_stats_report_average_and_tail() {
        let mut s = LatencyStats::default();
        assert_eq!(s.percentile(0.95), Duration::ZERO);
        for ms in 1..=100 {
            s.record(Duration::from_millis(ms));
        }
        assert_eq!(s.max(), Duration::from_millis(100));
        assert_eq!(s.percentile(0.95), Duration::from_millis(95));
        assert_eq!(s.average(), Duration::from_micros(50_500));
    }
}
