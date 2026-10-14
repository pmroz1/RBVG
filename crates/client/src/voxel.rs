//! Chunk streaming around the player and mesh upload.
//!
//! Generation and meshing run on Bevy's `AsyncComputeTaskPool`. Jobs own immutable
//! inputs (generator handle, chunk snapshot) produced by the Bevy-free
//! [`ChunkPipeline`]; finished tasks are polled without blocking and their results are
//! validated by the pipeline on the main thread before touching the world or the ECS.
//! Dropping a task handle cancels work that has not started yet.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use bevy::asset::RenderAssetUsages;
use bevy::mesh::{Indices, PrimitiveTopology};
use bevy::prelude::*;
use bevy::tasks::{AsyncComputeTaskPool, Task, block_on};
use rbvg_voxel::meshing::ChunkMesh;
use rbvg_voxel::streaming::{ChunkPipeline, GenResult, MeshResult, StreamConfig, Ticket};
use rbvg_voxel::{BlockId, BlockPos, BlockRegistry, ChunkPos, VoxelWorld};

use crate::player::Player;

#[derive(Resource)]
pub struct Blocks(pub Arc<BlockRegistry>);

/// Streaming scheduler and its statistics.
#[derive(Resource)]
pub struct Pipeline(pub ChunkPipeline);

/// Marks rendered chunk entities (used to count visible chunks).
#[derive(Component)]
pub struct ChunkRender;

/// In-flight background tasks, keyed by the pipeline ticket that owns them.
#[derive(Resource, Default)]
struct GenTasks(Vec<(ChunkPos, Ticket, Task<GenResult>)>);

#[derive(Resource, Default)]
struct MeshTasks(Vec<(ChunkPos, Ticket, Task<MeshResult>)>);

#[derive(Resource)]
pub struct GameWorld(pub VoxelWorld);

/// Per-block linear vertex colours, indexed by runtime id.
#[derive(Resource)]
struct BlockColors(Vec<[f32; 4]>);

#[derive(Resource)]
struct ChunkMaterial(Handle<StandardMaterial>);

/// Rendered chunk entities.
#[derive(Resource, Default)]
pub struct ChunkEntities(pub HashMap<ChunkPos, Entity>);

/// Renders and streams the world. Expects [`Blocks`] and [`GameWorld`] to be inserted.
pub struct VoxelPlugin;

impl Plugin for VoxelPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<ChunkEntities>()
            .init_resource::<GenTasks>()
            .init_resource::<MeshTasks>()
            .insert_resource(Pipeline(ChunkPipeline::new(StreamConfig::default())))
            .insert_resource(ClearColor(Color::srgb(0.53, 0.74, 0.92)))
            .add_systems(Startup, setup_scene)
            .add_systems(Update, (stream_chunks, mesh_chunks).chain());
    }
}

fn setup_scene(
    mut commands: Commands,
    mut materials: ResMut<Assets<StandardMaterial>>,
    blocks: Res<Blocks>,
) {
    let colors = blocks
        .0
        .iter()
        .map(|(_, d)| {
            let [r, g, b] = d.color;
            Color::srgb(r, g, b).to_linear().to_f32_array()
        })
        .collect();
    commands.insert_resource(BlockColors(colors));
    commands.insert_resource(ChunkMaterial(materials.add(StandardMaterial {
        base_color: Color::WHITE,
        perceptual_roughness: 1.0,
        ..default()
    })));
    commands.spawn((
        DirectionalLight {
            illuminance: 8_000.0,
            ..default()
        },
        Transform::from_xyz(0.3, 1.0, 0.5).looking_at(Vec3::ZERO, Vec3::Y),
    ));
    commands.insert_resource(GlobalAmbientLight {
        brightness: 600.0,
        ..default()
    });
}

/// Chunk containing a world-space point.
pub fn chunk_at(p: Vec3) -> ChunkPos {
    BlockPos::containing(p.to_array()).chunk()
}

/// Takes the results of finished tasks, at most `max`, without blocking.
fn take_finished<T>(tasks: &mut Vec<(ChunkPos, Ticket, Task<T>)>, max: usize) -> Vec<T> {
    let mut done = Vec::new();
    let mut i = 0;
    while i < tasks.len() && done.len() < max {
        if tasks[i].2.is_finished() {
            // Finished, so `block_on` returns immediately.
            done.push(block_on(tasks.swap_remove(i).2));
        } else {
            i += 1;
        }
    }
    done
}

fn stream_chunks(
    mut world: ResMut<GameWorld>,
    mut pipeline: ResMut<Pipeline>,
    mut tasks: ResMut<GenTasks>,
    player: Single<&Transform, With<Player>>,
) {
    let start = Instant::now();
    let finished = take_finished(&mut tasks.0, usize::MAX);
    let applied = !finished.is_empty();
    for result in finished {
        pipeline.0.finish_generation(&mut world.0, result);
    }
    if applied {
        pipeline.0.stats.apply.record(start.elapsed());
    }
    let pool = AsyncComputeTaskPool::get();
    for job in pipeline
        .0
        .update(&mut world.0, chunk_at(player.translation))
    {
        let (pos, ticket) = (job.pos, job.ticket);
        tasks
            .0
            .push((pos, ticket, pool.spawn(async move { job.run() })));
    }
    // Dropping a cancelled task stops it if it has not started; late results are
    // rejected by ticket anyway.
    let p = &pipeline.0;
    tasks
        .0
        .retain(|(pos, t, _)| p.is_current_generation(*pos, *t));
}

#[allow(clippy::too_many_arguments)]
fn mesh_chunks(
    mut commands: Commands,
    mut world: ResMut<GameWorld>,
    mut pipeline: ResMut<Pipeline>,
    mut tasks: ResMut<MeshTasks>,
    blocks: Res<Blocks>,
    mut colors: ResMut<BlockColors>,
    material: Res<ChunkMaterial>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut entities: ResMut<ChunkEntities>,
    player: Single<&Transform, With<Player>>,
) {
    if blocks.is_changed() {
        colors.0 = blocks
            .0
            .iter()
            .map(|(_, def)| {
                let [r, g, b] = def.color;
                Color::srgb(r, g, b).to_linear().to_f32_array()
            })
            .collect();
    }
    entities.0.retain(|pos, e| {
        let keep = world.0.is_loaded(*pos);
        if !keep {
            commands.entity(*e).despawn();
        }
        keep
    });

    let start = Instant::now();
    let finished = take_finished(&mut tasks.0, pipeline.0.config.max_apply_per_tick);
    let applied = !finished.is_empty();
    for result in finished {
        let pos = result.pos;
        let Some(mesh) = pipeline.0.finish_meshing(&mut world.0, result) else {
            continue;
        };
        if mesh.is_empty() {
            if let Some(e) = entities.0.remove(&pos) {
                commands.entity(e).despawn();
            }
            continue;
        }
        let handle = meshes.add(to_bevy_mesh(mesh, &colors.0));
        match entities.0.get(&pos) {
            Some(&e) => {
                commands.entity(e).insert(Mesh3d(handle));
            }
            None => {
                let o = pos.origin();
                let e = commands
                    .spawn((
                        ChunkRender,
                        Mesh3d(handle),
                        MeshMaterial3d(material.0.clone()),
                        Transform::from_xyz(o.x as f32, o.y as f32, o.z as f32),
                    ))
                    .id();
                entities.0.insert(pos, e);
            }
        }
    }
    if applied {
        pipeline.0.stats.apply.record(start.elapsed());
    }

    let pool = AsyncComputeTaskPool::get();
    let center = chunk_at(player.translation);
    for job in pipeline.0.schedule_meshing(&mut world.0, &blocks.0, center) {
        let (pos, ticket) = (job.pos, job.ticket);
        tasks
            .0
            .push((pos, ticket, pool.spawn(async move { job.run() })));
    }
    let p = &pipeline.0;
    tasks.0.retain(|(pos, t, _)| p.is_current_mesh(*pos, *t));
}

fn to_bevy_mesh(m: ChunkMesh, colors: &[[f32; 4]]) -> Mesh {
    let vertex_colors: Vec<[f32; 4]> = m
        .blocks
        .iter()
        .map(|b: &BlockId| {
            colors
                .get(usize::from(b.0))
                .copied()
                .unwrap_or([1.0, 0.0, 1.0, 1.0])
        })
        .collect();
    Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::RENDER_WORLD,
    )
    .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, m.positions)
    .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, m.normals)
    .with_inserted_attribute(Mesh::ATTRIBUTE_COLOR, vertex_colors)
    .with_inserted_indices(Indices::U32(m.indices))
}
