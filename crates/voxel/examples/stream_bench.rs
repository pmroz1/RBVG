//! Headless chunk generation + meshing benchmark (no GPU, no Bevy).
//!
//! `cargo run --release -p rbvg_voxel --example stream_bench [seed] [radius]`
//!
//! Streams a (2r+1)² × 5 chunk region through the same `ChunkPipeline` the client
//! uses, running each admitted batch of jobs on worker threads. Reports per-job
//! latency (avg/p50/p95/max) and wall-clock throughput. CPU-only: says nothing
//! about GPU upload or rendering cost.

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use rbvg_voxel::streaming::{ChunkPipeline, StreamConfig};
use rbvg_voxel::{BlockRegistry, TerrainGenerator, VoxelWorld};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let seed: u32 = args.next().map(|s| s.parse()).transpose()?.unwrap_or(42);
    let radius: i32 = args.next().map(|s| s.parse()).transpose()?.unwrap_or(6);
    let workers = std::thread::available_parallelism().map_or(4, |n| n.get());

    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut registry = BlockRegistry::default();
    let blocks = std::fs::read_to_string(root.join("content/core/blocks.toml"))?;
    registry.load_toml("core", &blocks)?;
    let generator = TerrainGenerator::new(seed, &registry)?;
    let center = rbvg_voxel::BlockPos::new(0, generator.surface_height(0, 0), 0).chunk();
    let registry = Arc::new(registry);
    let mut world = VoxelWorld::new(seed, Arc::new(generator));
    let mut pipeline = ChunkPipeline::new(StreamConfig {
        radius,
        radius_y: 2,
        max_generating: workers,
        max_meshing: workers,
        max_admit_per_tick: workers,
        max_apply_per_tick: usize::MAX,
    });

    let mut gen_times = Vec::new();
    let start = Instant::now();
    loop {
        let jobs = pipeline.update(&mut world, center);
        if jobs.is_empty() {
            break;
        }
        for r in run_parallel(jobs, |j| j.run()) {
            gen_times.push(r.duration);
            pipeline.finish_generation(&mut world, r);
        }
    }
    let gen_wall = start.elapsed();

    let mut mesh_times = Vec::new();
    let mut faces = 0;
    let start = Instant::now();
    loop {
        let jobs = pipeline.schedule_meshing(&mut world, &registry, center);
        if jobs.is_empty() {
            break;
        }
        for r in run_parallel(jobs, |j| j.run()) {
            mesh_times.push(r.duration);
            if let Some(mesh) = pipeline.finish_meshing(&mut world, r) {
                faces += mesh.face_count();
            }
        }
    }
    let mesh_wall = start.elapsed();

    println!("seed {seed}, radius {radius}, {workers} workers");
    report("generation", &mut gen_times, gen_wall);
    report("meshing", &mut mesh_times, mesh_wall);
    println!(
        "faces: {faces}, stale meshes: {}",
        pipeline.stats.stale_meshes
    );
    Ok(())
}

fn run_parallel<J: Send, R: Send>(jobs: Vec<J>, f: fn(J) -> R) -> Vec<R> {
    std::thread::scope(|s| {
        let handles: Vec<_> = jobs.into_iter().map(|j| s.spawn(move || f(j))).collect();
        handles
            .into_iter()
            .map(|h| h.join().expect("job panicked"))
            .collect()
    })
}

fn report(label: &str, times: &mut [Duration], wall: Duration) {
    times.sort();
    let n = times.len().max(1);
    let avg = times.iter().sum::<Duration>() / n as u32;
    let pct = |p: f64| {
        times
            .get(((n - 1) as f64 * p) as usize)
            .copied()
            .unwrap_or_default()
    };
    println!(
        "{label:>10}: {} jobs, per job avg {avg:?} p50 {:?} p95 {:?} max {:?}; wall {wall:?} ({:.0} chunks/s)",
        times.len(),
        pct(0.5),
        pct(0.95),
        times.last().copied().unwrap_or_default(),
        times.len() as f64 / wall.as_secs_f64().max(1e-9),
    );
}
