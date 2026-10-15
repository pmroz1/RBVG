//! Chunk mesh generation with face culling.
//!
//! The output is renderer-agnostic vertex data; the client uploads it. Meshing reads an
//! owned [`ChunkSnapshot`] (the chunk plus the touching layer of each face neighbour), so
//! it runs on background tasks without locks or world access, and can be swapped for a
//! greedy mesher without changing the output type.

use crate::coords::{CHUNK_SIZE, CHUNK_VOLUME, ChunkPos, LocalPos};
use crate::registry::{BlockId, BlockRegistry};
use crate::world::{NEIGHBOURS, VoxelWorld};

/// Edge length of a snapshot: the chunk plus one block of border on every side.
const PADDED: usize = CHUNK_SIZE as usize + 2;

/// Immutable copy of everything needed to mesh one chunk: its blocks and the touching
/// layer of its six face neighbours (unloaded neighbours are air). About 11 KiB.
#[derive(Clone)]
pub struct ChunkSnapshot {
    pub pos: ChunkPos,
    blocks: Box<[BlockId; PADDED * PADDED * PADDED]>,
}

impl ChunkSnapshot {
    /// Copies the chunk at `pos` and its border. `None` if the chunk is not loaded.
    pub fn capture(world: &VoxelWorld, pos: ChunkPos) -> Option<Self> {
        let chunk = world.chunk(pos)?;
        let mut snap = Self {
            pos,
            blocks: Box::new([BlockId::AIR; PADDED * PADDED * PADDED]),
        };
        let n = CHUNK_SIZE;
        for (i, id) in chunk.blocks().iter().enumerate().take(CHUNK_VOLUME) {
            let i = i as i32;
            let (x, z, y) = (i % n, (i / n) % n, i / (n * n));
            *snap.slot(x, y, z) = *id;
        }
        for d in NEIGHBOURS {
            let Some(neighbour) = world.chunk(pos.offset(d.0, d.1, d.2)) else {
                continue;
            };
            // Along the face axis: our coordinate is -1 or 16, theirs is 15 or 0.
            let ours = |dir: i32, a: i32| {
                if dir == 0 {
                    a
                } else if dir > 0 {
                    n
                } else {
                    -1
                }
            };
            let theirs = |dir: i32, a: i32| {
                if dir == 0 {
                    a
                } else if dir > 0 {
                    0
                } else {
                    n - 1
                }
            };
            for a in 0..n {
                for b in 0..n {
                    // (a, b) span the two axes perpendicular to `d`.
                    let (u, v, w) = match d {
                        (_, 0, 0) => (0, a, b),
                        (0, _, 0) => (a, 0, b),
                        _ => (a, b, 0),
                    };
                    let local = LocalPos::new(
                        theirs(d.0, u) as u8,
                        theirs(d.1, v) as u8,
                        theirs(d.2, w) as u8,
                    );
                    *snap.slot(ours(d.0, u), ours(d.1, v), ours(d.2, w)) = neighbour.get(local);
                }
            }
        }
        Some(snap)
    }

    /// Block at chunk-local coordinates in `-1..=16`.
    fn get(&self, x: i32, y: i32, z: i32) -> BlockId {
        self.blocks[Self::index(x, y, z)]
    }

    fn slot(&mut self, x: i32, y: i32, z: i32) -> &mut BlockId {
        &mut self.blocks[Self::index(x, y, z)]
    }

    fn index(x: i32, y: i32, z: i32) -> usize {
        (x + 1) as usize + (z + 1) as usize * PADDED + (y + 1) as usize * PADDED * PADDED
    }
}

/// Vertex buffers for one chunk. Positions are relative to the chunk origin.
#[derive(Debug, Default, Clone)]
pub struct ChunkMesh {
    pub positions: Vec<[f32; 3]>,
    pub normals: Vec<[f32; 3]>,
    /// Face-local texture coordinates in `0..=1`; `v` grows downwards (image convention),
    /// so side faces are upright. Map into an atlas tile with [`crate::atlas::BlockAtlas`].
    pub uvs: Vec<[f32; 2]>,
    /// Per-vertex block id, so the client can resolve colours/materials.
    pub blocks: Vec<BlockId>,
    pub indices: Vec<u32>,
}

impl ChunkMesh {
    pub fn clear(&mut self) {
        self.positions.clear();
        self.normals.clear();
        self.uvs.clear();
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
    /// Texture coordinates of `corners`, upright on side faces.
    uvs: [[f32; 2]; 4],
}

const FACES: [Face; 6] = [
    Face {
        dir: (1, 0, 0),
        normal: [1.0, 0.0, 0.0],
        corners: [[1., 0., 0.], [1., 1., 0.], [1., 1., 1.], [1., 0., 1.]],
        uvs: [[1., 1.], [1., 0.], [0., 0.], [0., 1.]],
    },
    Face {
        dir: (-1, 0, 0),
        normal: [-1.0, 0.0, 0.0],
        corners: [[0., 0., 1.], [0., 1., 1.], [0., 1., 0.], [0., 0., 0.]],
        uvs: [[1., 1.], [1., 0.], [0., 0.], [0., 1.]],
    },
    Face {
        dir: (0, 1, 0),
        normal: [0.0, 1.0, 0.0],
        corners: [[0., 1., 0.], [0., 1., 1.], [1., 1., 1.], [1., 1., 0.]],
        uvs: [[0., 0.], [0., 1.], [1., 1.], [1., 0.]],
    },
    Face {
        dir: (0, -1, 0),
        normal: [0.0, -1.0, 0.0],
        corners: [[0., 0., 0.], [1., 0., 0.], [1., 0., 1.], [0., 0., 1.]],
        uvs: [[1., 0.], [0., 0.], [0., 1.], [1., 1.]],
    },
    Face {
        dir: (0, 0, 1),
        normal: [0.0, 0.0, 1.0],
        corners: [[0., 0., 1.], [1., 0., 1.], [1., 1., 1.], [0., 1., 1.]],
        uvs: [[0., 1.], [1., 1.], [1., 0.], [0., 0.]],
    },
    Face {
        dir: (0, 0, -1),
        normal: [0.0, 0.0, -1.0],
        corners: [[1., 0., 0.], [0., 0., 0.], [0., 1., 0.], [1., 1., 0.]],
        uvs: [[0., 1.], [1., 1.], [1., 0.], [0., 0.]],
    },
];

/// Rebuilds `out` for the chunk at `pos` directly from the world (synchronous
/// convenience over [`ChunkSnapshot::capture`] + [`mesh_snapshot`]).
pub fn mesh_chunk(
    world: &VoxelWorld,
    registry: &BlockRegistry,
    pos: ChunkPos,
    out: &mut ChunkMesh,
) {
    match ChunkSnapshot::capture(world, pos) {
        Some(snap) => mesh_snapshot(&snap, registry, out),
        None => out.clear(),
    }
}

/// Rebuilds `out` from a snapshot, emitting only faces adjacent to non-solid blocks
/// (including across chunk borders). `out` is cleared first so callers can reuse its
/// allocations.
pub fn mesh_snapshot(snap: &ChunkSnapshot, registry: &BlockRegistry, out: &mut ChunkMesh) {
    out.clear();
    let n = CHUNK_SIZE;
    for y in 0..n {
        for z in 0..n {
            for x in 0..n {
                let id = snap.get(x, y, z);
                if !registry.is_solid(id) {
                    continue;
                }
                for face in &FACES {
                    let neighbour = snap.get(x + face.dir.0, y + face.dir.1, z + face.dir.2);
                    if registry.is_solid(neighbour) {
                        continue;
                    }
                    let base = out.positions.len() as u32;
                    for (c, uv) in face.corners.iter().zip(face.uvs) {
                        out.positions
                            .push([c[0] + x as f32, c[1] + y as f32, c[2] + z as f32]);
                        out.normals.push(face.normal);
                        out.uvs.push(uv);
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
    use crate::coords::BlockPos;
    use crate::registry::BlockDef;
    use crate::world::tests::flat_world;

    fn registry() -> BlockRegistry {
        let mut r = BlockRegistry::default();
        r.register(BlockDef {
            id: "core:stone".into(),
            display_name: "Stone".into(),
            solid: true,
            color: [0.5; 3],
            missing: false,
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
    fn side_textures_are_upright_and_not_mirrored() {
        for f in &FACES {
            let n = f.normal;
            for (c, uv) in f.corners.iter().zip(f.uvs) {
                if n[1] == 0.0 {
                    // Seen from outside: v points down, u points right (= -normal × up).
                    assert_eq!(uv[1], 1.0 - c[1], "{n:?}");
                    let right = [n[2], 0.0, -n[0]];
                    let along = right[0] * c[0] + right[2] * c[2];
                    let offset = if right[0] + right[2] < 0.0 { 1.0 } else { 0.0 };
                    assert_eq!(uv[0], along + offset, "{n:?}");
                } else {
                    // North (-z) is up on both; the bottom is seen from below, so x flips.
                    let u = if n[1] > 0.0 { c[0] } else { 1.0 - c[0] };
                    assert_eq!(uv, [u, c[2]], "{n:?}");
                }
            }
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
        assert_eq!(m.uvs.len(), 24);
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

    #[test]
    fn snapshot_border_culls_against_each_neighbour() {
        // A block on every face of chunk (0,0,0), each touching a solid block in the
        // neighbour: the face towards the neighbour must be culled (5 faces each).
        let reg = registry();
        for (inside, outside) in [
            ((15, 5, 5), (16, 5, 5)),
            ((0, 5, 5), (-1, 5, 5)),
            ((5, 15, 5), (5, 16, 5)),
            ((5, 0, 5), (5, -1, 5)),
            ((5, 5, 15), (5, 5, 16)),
            ((5, 5, 0), (5, 5, -1)),
        ] {
            let mut w = crate::VoxelWorld::new(0, std::sync::Arc::new(crate::world::tests::Flat));
            for x in -1..=1 {
                for y in 1..=3 {
                    for z in -1..=1 {
                        w.load_chunk(ChunkPos::new(x, y, z)); // all air (y >= 0)
                    }
                }
            }
            // Shift up by one chunk so every neighbour is an all-air loaded chunk.
            let lift = |(x, y, z): (i32, i32, i32)| BlockPos::new(x, y + 32, z);
            w.set_block(lift(inside), BlockId(1)).unwrap();
            w.set_block(lift(outside), BlockId(1)).unwrap();
            let mut m = ChunkMesh::default();
            mesh_chunk(&w, &reg, ChunkPos::new(0, 2, 0), &mut m);
            assert_eq!(m.face_count(), 5, "{inside:?}");
        }
    }
}
