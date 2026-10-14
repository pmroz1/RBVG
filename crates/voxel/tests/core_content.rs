//! The shipped core content must load and satisfy the default generator.

use rbvg_voxel::meshing::{ChunkMesh, mesh_chunk};
use rbvg_voxel::{BlockRegistry, ChunkPos, TerrainGenerator, VoxelWorld};

#[test]
fn core_blocks_load_and_generate_visible_terrain() {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../content/core/blocks.toml"
    );
    let mut registry = BlockRegistry::default();
    registry
        .load_toml("core", &std::fs::read_to_string(path).unwrap())
        .unwrap();
    let generator = TerrainGenerator::new(42, &registry).unwrap();

    // Find the chunk containing the surface at the origin and check it meshes.
    let surface = ChunkPos::new(0, generator.surface_height(0, 0).div_euclid(16), 0);
    let mut world = VoxelWorld::new(42, Box::new(generator));
    for dy in -1..=1 {
        world.load_chunk(surface.offset(0, dy, 0));
    }
    let mut mesh = ChunkMesh::default();
    mesh_chunk(&world, &registry, surface, &mut mesh);
    assert!(mesh.face_count() > 0);
}
