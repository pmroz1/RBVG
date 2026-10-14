//! Chunk map with loading, unloading, edits and remesh tracking.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use thiserror::Error;

use crate::chunk::Chunk;
use crate::coords::{BlockPos, CHUNK_SIZE, ChunkPos};
use crate::generation::WorldGenerator;
use crate::registry::BlockId;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum WorldError {
    #[error("chunk {0:?} is not loaded")]
    ChunkNotLoaded(ChunkPos),
}

/// Face-neighbour offsets, used for boundary bookkeeping and mesh snapshots.
pub(crate) const NEIGHBOURS: [(i32, i32, i32); 6] = [
    (1, 0, 0),
    (-1, 0, 0),
    (0, 1, 0),
    (0, -1, 0),
    (0, 0, 1),
    (0, 0, -1),
];

/// Process-wide source of chunk revisions. Never reset, so a revision identifies one
/// chunk *instance and content* across unload/reload and even across world reloads.
static NEXT_REVISION: AtomicU64 = AtomicU64::new(1);

fn next_revision() -> u64 {
    NEXT_REVISION.fetch_add(1, Ordering::Relaxed)
}

/// A loaded chunk plus its revision stamp (fresh on load and on every change).
struct Resident {
    chunk: Chunk,
    revision: u64,
}

/// The block world. Unloaded positions read as air.
pub struct VoxelWorld {
    seed: u32,
    generator: Arc<dyn WorldGenerator>,
    chunks: HashMap<ChunkPos, Resident>,
    /// Edited chunks that are not resident (unloaded, or restored from a save before
    /// being streamed in). They take priority over the generator when loaded.
    stash: HashMap<ChunkPos, Chunk>,
    /// Loaded chunks whose mesh is stale.
    dirty: HashSet<ChunkPos>,
}

impl VoxelWorld {
    pub fn new(seed: u32, generator: Arc<dyn WorldGenerator>) -> Self {
        Self {
            seed,
            generator,
            chunks: HashMap::new(),
            stash: HashMap::new(),
            dirty: HashSet::new(),
        }
    }

    pub fn seed(&self) -> u32 {
        self.seed
    }

    pub fn is_loaded(&self, pos: ChunkPos) -> bool {
        self.chunks.contains_key(&pos)
    }

    pub fn chunk(&self, pos: ChunkPos) -> Option<&Chunk> {
        self.chunks.get(&pos).map(|r| &r.chunk)
    }

    /// Revision of a loaded chunk. A new, never-reused value is assigned whenever the
    /// chunk is loaded or changed, so equal revisions mean identical content and instance.
    pub fn revision(&self, pos: ChunkPos) -> Option<u64> {
        self.chunks.get(&pos).map(|r| r.revision)
    }

    /// Shared handle to the generator, so background tasks can generate chunks.
    pub fn generator(&self) -> Arc<dyn WorldGenerator> {
        self.generator.clone()
    }

    /// Whether an edited, non-resident copy of the chunk exists (it beats the generator).
    pub fn has_stashed(&self, pos: ChunkPos) -> bool {
        self.stash.contains_key(&pos)
    }

    pub fn loaded_count(&self) -> usize {
        self.chunks.len()
    }

    pub fn loaded_positions(&self) -> impl Iterator<Item = ChunkPos> + '_ {
        self.chunks.keys().copied()
    }

    /// Loads a chunk from the stash or the generator. Returns false if already loaded.
    pub fn load_chunk(&mut self, pos: ChunkPos) -> bool {
        if self.chunks.contains_key(&pos) {
            return false;
        }
        let chunk = self
            .stash
            .remove(&pos)
            .unwrap_or_else(|| self.generator.generate(pos));
        self.install(pos, chunk);
        true
    }

    /// Installs a chunk produced off-thread by the generator. Returns false (and drops
    /// it) if the chunk is already loaded. A stashed edited copy wins over `chunk`.
    pub fn insert_generated(&mut self, pos: ChunkPos, chunk: Chunk) -> bool {
        if self.chunks.contains_key(&pos) {
            return false;
        }
        let chunk = self.stash.remove(&pos).unwrap_or(chunk);
        self.install(pos, chunk);
        true
    }

    fn install(&mut self, pos: ChunkPos, chunk: Chunk) {
        let revision = next_revision();
        self.chunks.insert(pos, Resident { chunk, revision });
        self.mark_with_neighbours(pos);
    }

    /// Unloads a chunk, keeping it in memory if it was edited. Returns false if not loaded.
    pub fn unload_chunk(&mut self, pos: ChunkPos) -> bool {
        let Some(Resident { chunk, .. }) = self.chunks.remove(&pos) else {
            return false;
        };
        if chunk.is_modified() {
            self.stash.insert(pos, chunk);
        }
        self.dirty.remove(&pos);
        for (dx, dy, dz) in NEIGHBOURS {
            self.mark_dirty(pos.offset(dx, dy, dz));
        }
        true
    }

    /// Installs an edited chunk (e.g. from a save), replacing any resident copy.
    pub fn insert_saved(&mut self, pos: ChunkPos, chunk: Chunk) {
        if self.chunks.contains_key(&pos) {
            self.install(pos, chunk);
        } else {
            self.stash.insert(pos, chunk);
        }
    }

    pub fn get_block(&self, p: BlockPos) -> BlockId {
        self.block_if_loaded(p).unwrap_or(BlockId::AIR)
    }

    /// Block at `p`, or `None` if its chunk is not loaded (unlike [`Self::get_block`]).
    pub fn block_if_loaded(&self, p: BlockPos) -> Option<BlockId> {
        self.chunks.get(&p.chunk()).map(|r| r.chunk.get(p.local()))
    }

    /// Sets a block in a loaded chunk, returning the previous block.
    pub fn set_block(&mut self, p: BlockPos, id: BlockId) -> Result<BlockId, WorldError> {
        let cpos = p.chunk();
        let resident = self
            .chunks
            .get_mut(&cpos)
            .ok_or(WorldError::ChunkNotLoaded(cpos))?;
        let local = p.local();
        let old = resident.chunk.set(local, id);
        if old != id {
            resident.revision = next_revision();
            self.mark_dirty(cpos);
            // Faces of the neighbouring chunk may become visible/hidden.
            let edge = |v: u8| match i32::from(v) {
                0 => -1,
                v if v == CHUNK_SIZE - 1 => 1,
                _ => 0,
            };
            let (ex, ey, ez) = (edge(local.x), edge(local.y), edge(local.z));
            if ex != 0 {
                self.mark_dirty(cpos.offset(ex, 0, 0));
            }
            if ey != 0 {
                self.mark_dirty(cpos.offset(0, ey, 0));
            }
            if ez != 0 {
                self.mark_dirty(cpos.offset(0, 0, ez));
            }
        }
        Ok(old)
    }

    /// Every edited chunk, resident or stashed. This is what persistence saves.
    pub fn modified_chunks(&self) -> impl Iterator<Item = (ChunkPos, &Chunk)> {
        self.chunks
            .iter()
            .map(|(p, r)| (p, &r.chunk))
            .filter(|(_, c)| c.is_modified())
            .chain(self.stash.iter())
            .map(|(p, c)| (*p, c))
    }

    pub fn is_dirty(&self, pos: ChunkPos) -> bool {
        self.dirty.contains(&pos)
    }

    pub fn dirty_positions(&self) -> impl Iterator<Item = ChunkPos> + '_ {
        self.dirty.iter().copied()
    }

    /// Clears the dirty flag (the caller took responsibility for remeshing).
    pub fn clear_dirty(&mut self, pos: ChunkPos) -> bool {
        self.dirty.remove(&pos)
    }

    /// Marks a loaded chunk's mesh as stale (e.g. after a discarded mesh result).
    pub fn request_remesh(&mut self, pos: ChunkPos) {
        self.mark_dirty(pos);
    }

    pub fn dirty_count(&self) -> usize {
        self.dirty.len()
    }

    /// Removes and returns up to `max` dirty chunks, nearest to `center` first.
    pub fn take_dirty_nearest(&mut self, center: ChunkPos, max: usize) -> Vec<ChunkPos> {
        let mut all: Vec<ChunkPos> = self.dirty.iter().copied().collect();
        all.sort_unstable_by_key(|p| {
            let (dx, dy, dz) = (p.x - center.x, p.y - center.y, p.z - center.z);
            dx * dx + dy * dy + dz * dz
        });
        all.truncate(max);
        for p in &all {
            self.dirty.remove(p);
        }
        all
    }

    fn mark_dirty(&mut self, pos: ChunkPos) {
        if self.chunks.contains_key(&pos) {
            self.dirty.insert(pos);
        }
    }

    fn mark_with_neighbours(&mut self, pos: ChunkPos) {
        self.mark_dirty(pos);
        for (dx, dy, dz) in NEIGHBOURS {
            self.mark_dirty(pos.offset(dx, dy, dz));
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Generator that fills everything below y=0 with block 1.
    pub(crate) struct Flat;
    impl WorldGenerator for Flat {
        fn generate(&self, pos: ChunkPos) -> Chunk {
            let mut c = Chunk::default();
            if pos.y < 0 {
                for i in 0..CHUNK_SIZE as u8 {
                    for j in 0..CHUNK_SIZE as u8 {
                        for k in 0..CHUNK_SIZE as u8 {
                            c.set_generated(crate::LocalPos::new(i, j, k), BlockId(1));
                        }
                    }
                }
            }
            c
        }
    }

    pub(crate) fn flat_world(radius: i32) -> VoxelWorld {
        let mut w = VoxelWorld::new(0, Arc::new(Flat));
        for x in -radius..radius {
            for y in -radius..radius {
                for z in -radius..radius {
                    w.load_chunk(ChunkPos::new(x, y, z));
                }
            }
        }
        w
    }

    #[test]
    fn set_and_get_across_negative_coordinates() {
        let mut w = flat_world(1);
        let p = BlockPos::new(-1, -1, -1);
        assert_eq!(w.get_block(p), BlockId(1));
        assert_eq!(w.set_block(p, BlockId::AIR), Ok(BlockId(1)));
        assert_eq!(w.get_block(p), BlockId::AIR);
        assert_eq!(
            w.set_block(BlockPos::new(100, 0, 0), BlockId(1)),
            Err(WorldError::ChunkNotLoaded(ChunkPos::new(6, 0, 0)))
        );
    }

    #[test]
    fn boundary_edit_dirties_neighbour() {
        let mut w = flat_world(1);
        w.take_dirty_nearest(ChunkPos::new(0, 0, 0), usize::MAX);
        w.set_block(BlockPos::new(0, -1, 5), BlockId::AIR).unwrap();
        let mut dirty = w.take_dirty_nearest(ChunkPos::new(0, 0, 0), usize::MAX);
        dirty.sort();
        // x=0 is the chunk's low X edge, y=-1 is the top layer of chunk y=-1.
        assert_eq!(
            dirty,
            vec![
                ChunkPos::new(-1, -1, 0),
                ChunkPos::new(0, -1, 0),
                ChunkPos::new(0, 0, 0)
            ]
        );
    }

    #[test]
    fn edits_survive_unload_and_reload() {
        let mut w = flat_world(1);
        let p = BlockPos::new(3, -2, 3);
        w.set_block(p, BlockId::AIR).unwrap();
        assert!(w.unload_chunk(p.chunk()));
        assert_eq!(w.get_block(p), BlockId::AIR, "unloaded reads as air");
        assert_eq!(w.modified_chunks().count(), 1);
        assert!(w.load_chunk(p.chunk()));
        assert_eq!(w.get_block(p), BlockId::AIR);
        assert_eq!(w.get_block(p.offset(1, 0, 0)), BlockId(1));
    }

    #[test]
    fn revisions_change_on_edit_and_never_repeat_across_reload() {
        let mut w = flat_world(1);
        let p = BlockPos::new(3, -2, 3);
        let r0 = w.revision(p.chunk()).unwrap();
        w.set_block(p, BlockId(1)).unwrap(); // no-op edit keeps the revision
        assert_eq!(w.revision(p.chunk()), Some(r0));
        w.set_block(p, BlockId::AIR).unwrap();
        let r1 = w.revision(p.chunk()).unwrap();
        assert_ne!(r0, r1);
        w.unload_chunk(p.chunk());
        assert_eq!(w.revision(p.chunk()), None);
        w.load_chunk(p.chunk());
        let r2 = w.revision(p.chunk()).unwrap();
        assert!(r2 != r0 && r2 != r1, "reload is a new incarnation");
    }

    #[test]
    fn generated_result_never_overwrites_loaded_or_stashed_edits() {
        let mut w = flat_world(1);
        let p = BlockPos::new(3, -2, 3);
        // A background result arriving for a loaded (edited) chunk is dropped.
        w.set_block(p, BlockId::AIR).unwrap();
        assert!(!w.insert_generated(p.chunk(), Flat.generate(p.chunk())));
        assert_eq!(w.get_block(p), BlockId::AIR);
        // For an unloaded chunk with a stashed edit, the stash wins over the result.
        w.unload_chunk(p.chunk());
        assert!(w.has_stashed(p.chunk()));
        assert!(w.insert_generated(p.chunk(), Flat.generate(p.chunk())));
        assert_eq!(w.get_block(p), BlockId::AIR);
        assert!(!w.has_stashed(p.chunk()));
    }
}
