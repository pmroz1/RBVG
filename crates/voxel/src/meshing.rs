//! Chunk mesh generation with face culling.
//!
//! The output is renderer-agnostic vertex data; the client uploads it. The mesher is a
//! plain function over world data so it can later run on a background task against a
//! chunk snapshot, and be swapped for a greedy mesher without changing the output type.

use crate::coords::{BlockPos, CHUNK_SIZE, ChunkPos, LocalPos};
use crate::registry::{BlockId, BlockRegistry};
use crate::world::VoxelWorld;

/// Vertex buffers for one chunk. Positions are relative to the chunk origin.
#[derive(Debug, Default, Clone)]
pub struct ChunkMesh {
    pub positions: Vec<[f32; 3]>,
    pub normals: Vec<[f32; 3]>,
    /// Per-vertex block id, so the client can resolve colours/materials.
    pub blocks: Vec<BlockId>,
    pub indices: Vec<u32>,
}

impl ChunkMesh {
    pub fn clear(&mut self) {
        self.positions.clear();
        self.normals.clear();
        self.blocks.clear();
        self.indices.clear();
    }

    pub fn is_empty(&self) -> bool {
        self.indices.is_empty()
    }

    pub fn face_count(&self) -> usize {
        self.indices.len() / 6
    }
}

struct Face {
    dir: (i32, i32, i32),
    normal: [f32; 3],
    /// Unit-cube corners, counter-clockwise when viewed from outside.
    corners: [[f32; 3]; 4],
}

const FACES: [Face; 6] = [
    Face {
        dir: (1, 0, 0),
        normal: [1.0, 0.0, 0.0],
        corners: [[1., 0., 0.], [1., 1., 0.], [1., 1., 1.], [1., 0., 1.]],
    },
    Face {
        dir: (-1, 0, 0),
        normal: [-1.0, 0.0, 0.0],
        corners: [[0., 0., 1.], [0., 1., 1.], [0., 1., 0.], [0., 0., 0.]],
    },
    Face {
        dir: (0, 1, 0),
        normal: [0.0, 1.0, 0.0],
        corners: [[0., 1., 0.], [0., 1., 1.], [1., 1., 1.], [1., 1., 0.]],
    },
    Face {
        dir: (0, -1, 0),
        normal: [0.0, -1.0, 0.0],
        corners: [[0., 0., 0.], [1., 0., 0.], [1., 0., 1.], [0., 0., 1.]],
    },
    Face {
        dir: (0, 0, 1),
        normal: [0.0, 0.0, 1.0],
        corners: [[0., 0., 1.], [1., 0., 1.], [1., 1., 1.], [0., 1., 1.]],
    },
    Face {
        dir: (0, 0, -1),
        normal: [0.0, 0.0, -1.0],
        corners: [[1., 0., 0.], [0., 0., 0.], [0., 1., 0.], [1., 1., 0.]],
    },
];

/// Rebuilds `out` for the chunk at `pos`, emitting only faces adjacent to non-solid
/// blocks (including across chunk borders). `out` is cleared first so callers can reuse
/// its allocations. Unloaded neighbours count as air.
pub fn mesh_chunk(
    world: &VoxelWorld,
    registry: &BlockRegistry,
    pos: ChunkPos,
    out: &mut ChunkMesh,
) {
    out.clear();
    let Some(chunk) = world.chunk(pos) else {
        return;
    };
    let origin = pos.origin();
    let n = CHUNK_SIZE;
    for y in 0..n {
        for z in 0..n {
            for x in 0..n {
                let id = chunk.get(LocalPos::new(x as u8, y as u8, z as u8));
                if !registry.is_solid(id) {
                    continue;
                }
                for face in &FACES {
                    let (nx, ny, nz) = (x + face.dir.0, y + face.dir.1, z + face.dir.2);
                    let neighbour = if (0..n).contains(&nx)
                        && (0..n).contains(&ny)
                        && (0..n).contains(&nz)
                    {
                        chunk.get(LocalPos::new(nx as u8, ny as u8, nz as u8))
                    } else {
                        world.get_block(BlockPos::new(origin.x + nx, origin.y + ny, origin.z + nz))
                    };
                    if registry.is_solid(neighbour) {
                        continue;
                    }
                    let base = out.positions.len() as u32;
                    for c in face.corners {
                        out.positions
                            .push([c[0] + x as f32, c[1] + y as f32, c[2] + z as f32]);
                        out.normals.push(face.normal);
                        out.blocks.push(id);
                    }
                    out.indices.extend_from_slice(&[
                        base,
                        base + 1,
                        base + 2,
                        base,
                        base + 2,
                        base + 3,
                    ]);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::BlockDef;
    use crate::world::tests::flat_world;

    fn registry() -> BlockRegistry {
        let mut r = BlockRegistry::default();
        r.register(BlockDef {
            id: "core:stone".into(),
            display_name: "Stone".into(),
            solid: true,
            color: [0.5; 3],
        })
        .unwrap();
        r
    }

    #[test]
    fn face_winding_matches_normals() {
        for f in &FACES {
            let [a, b, c, _] = f.corners;
            let u = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
            let v = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
            let cross = [
                u[1] * v[2] - u[2] * v[1],
                u[2] * v[0] - u[0] * v[2],
                u[0] * v[1] - u[1] * v[0],
            ];
            assert_eq!(cross, f.normal);
        }
    }

    #[test]
    fn single_block_has_six_faces() {
        let mut w = flat_world(1);
        let reg = registry();
        w.set_block(BlockPos::new(4, 4, 4), BlockId(1)).unwrap();
        let mut m = ChunkMesh::default();
        mesh_chunk(&w, &reg, ChunkPos::new(0, 0, 0), &mut m);
        assert_eq!(m.face_count(), 6);
        assert_eq!(m.positions.len(), 24);
    }

    #[test]
    fn buried_chunk_faces_are_culled_across_borders() {
        // Chunk (0,-1,0) is full and surrounded by full chunks below/beside and air above:
        // only its 16x16 top layer is visible. Side borders at radius edge face unloaded air.
        let w = flat_world(2);
        let reg = registry();
        let mut m = ChunkMesh::default();
        mesh_chunk(&w, &reg, ChunkPos::new(0, -1, 0), &mut m);
        assert_eq!(m.face_count(), 16 * 16);
        assert!(m.normals.iter().all(|n| *n == [0.0, 1.0, 0.0]));
    }

    #[test]
    fn adjacent_blocks_share_no_internal_faces() {
        let mut w = flat_world(1);
        let reg = registry();
        w.set_block(BlockPos::new(4, 4, 4), BlockId(1)).unwrap();
        w.set_block(BlockPos::new(5, 4, 4), BlockId(1)).unwrap();
        let mut m = ChunkMesh::default();
        mesh_chunk(&w, &reg, ChunkPos::new(0, 0, 0), &mut m);
        assert_eq!(m.face_count(), 10);
    }
}
