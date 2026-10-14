//! Chunk streaming around the player and mesh upload.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use bevy::asset::RenderAssetUsages;
use bevy::mesh::{Indices, PrimitiveTopology};
use bevy::prelude::*;
use rbvg_voxel::meshing::{ChunkMesh, mesh_chunk};
use rbvg_voxel::{BlockId, BlockPos, BlockRegistry, ChunkPos, VoxelWorld};

use crate::player::Player;

/// Horizontal chunk radius kept loaded around the player.
const LOAD_RADIUS: i32 = 6;
/// Vertical chunk radius kept loaded around the player.
const LOAD_RADIUS_Y: i32 = 2;
const MAX_LOADS_PER_FRAME: usize = 8;
const MAX_MESHES_PER_FRAME: usize = 12;

#[derive(Resource)]
pub struct Blocks(pub BlockRegistry);

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

/// Timings for the debug overlay.
#[derive(Resource, Default)]
pub struct VoxelStats {
    pub last_generation: Duration,
    pub last_meshing: Duration,
    pub meshed_last_frame: usize,
    pub pending_loads: usize,
}

/// Renders and streams the world. Expects [`Blocks`] and [`GameWorld`] to be inserted.
pub struct VoxelPlugin;

impl Plugin for VoxelPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<ChunkEntities>()
            .init_resource::<VoxelStats>()
            .insert_resource(ClearColor(Color::srgb(0.53, 0.74, 0.92)))
            .add_systems(Startup, setup_scene)
            .add_systems(Update, (stream_chunks, mesh_dirty_chunks).chain());
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

fn stream_chunks(
    mut commands: Commands,
    mut world: ResMut<GameWorld>,
    mut entities: ResMut<ChunkEntities>,
    mut stats: ResMut<VoxelStats>,
    player: Single<&Transform, With<Player>>,
) {
    let center = chunk_at(player.translation);
    let in_range = |p: ChunkPos| {
        (p.x - center.x).abs() <= LOAD_RADIUS
            && (p.z - center.z).abs() <= LOAD_RADIUS
            && (p.y - center.y).abs() <= LOAD_RADIUS_Y
    };

    // Unload with one chunk of hysteresis so walking along a border does not thrash.
    let far: Vec<ChunkPos> = world
        .0
        .loaded_positions()
        .filter(|p| {
            (p.x - center.x).abs() > LOAD_RADIUS + 1
                || (p.z - center.z).abs() > LOAD_RADIUS + 1
                || (p.y - center.y).abs() > LOAD_RADIUS_Y + 1
        })
        .collect();
    for p in far {
        world.0.unload_chunk(p);
        if let Some(e) = entities.0.remove(&p) {
            commands.entity(e).despawn();
        }
    }

    let mut missing = Vec::new();
    for y in -LOAD_RADIUS_Y..=LOAD_RADIUS_Y {
        for z in -LOAD_RADIUS..=LOAD_RADIUS {
            for x in -LOAD_RADIUS..=LOAD_RADIUS {
                let p = center.offset(x, y, z);
                if in_range(p) && !world.0.is_loaded(p) {
                    missing.push(p);
                }
            }
        }
    }
    missing.sort_unstable_by_key(|p| {
        let (dx, dy, dz) = (p.x - center.x, p.y - center.y, p.z - center.z);
        dx * dx + dy * dy + dz * dz
    });
    stats.pending_loads = missing.len();
    if missing.is_empty() {
        return;
    }
    // shortcut: generation runs on the main thread with a per-frame cap; move to
    // AsyncComputeTaskPool when view distance grows.
    let start = Instant::now();
    for p in missing.into_iter().take(MAX_LOADS_PER_FRAME) {
        world.0.load_chunk(p);
    }
    stats.last_generation = start.elapsed();
}

fn mesh_dirty_chunks(
    mut commands: Commands,
    mut world: ResMut<GameWorld>,
    blocks: Res<Blocks>,
    colors: Res<BlockColors>,
    material: Res<ChunkMaterial>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut entities: ResMut<ChunkEntities>,
    mut stats: ResMut<VoxelStats>,
    mut buffer: Local<ChunkMesh>,
    player: Single<&Transform, With<Player>>,
) {
    let dirty = world
        .0
        .take_dirty_nearest(chunk_at(player.translation), MAX_MESHES_PER_FRAME);
    stats.meshed_last_frame = dirty.len();
    if dirty.is_empty() {
        return;
    }
    let start = Instant::now();
    for pos in dirty {
        mesh_chunk(&world.0, &blocks.0, pos, &mut buffer);
        if buffer.is_empty() {
            if let Some(e) = entities.0.remove(&pos) {
                commands.entity(e).despawn();
            }
            continue;
        }
        let handle = meshes.add(to_bevy_mesh(&buffer, &colors.0));
        match entities.0.get(&pos) {
            Some(&e) => {
                commands.entity(e).insert(Mesh3d(handle));
            }
            None => {
                let o = pos.origin();
                let e = commands
                    .spawn((
                        Mesh3d(handle),
                        MeshMaterial3d(material.0.clone()),
                        Transform::from_xyz(o.x as f32, o.y as f32, o.z as f32),
                    ))
                    .id();
                entities.0.insert(pos, e);
            }
        }
    }
    stats.last_meshing = start.elapsed();
}

fn to_bevy_mesh(m: &ChunkMesh, colors: &[[f32; 4]]) -> Mesh {
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
    .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, m.positions.clone())
    .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, m.normals.clone())
    .with_inserted_attribute(Mesh::ATTRIBUTE_COLOR, vertex_colors)
    .with_inserted_indices(Indices::U32(m.indices.clone()))
}
