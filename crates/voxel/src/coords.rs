//! World coordinate system.
//!
//! Right-handed, +Y up (matches Bevy). Block `(x, y, z)` occupies the unit cube
//! `[x, x+1) × [y, y+1) × [z, z+1)`. Chunks are `CHUNK_SIZE³` blocks; conversions use
//! Euclidean division so negative coordinates map correctly (block -1 is in chunk -1).

use serde::{Deserialize, Serialize};

/// Edge length of a cubic chunk, in blocks.
pub const CHUNK_SIZE: i32 = 16;
/// Number of blocks in one chunk.
pub const CHUNK_VOLUME: usize = (CHUNK_SIZE * CHUNK_SIZE * CHUNK_SIZE) as usize;

/// Integer world-space block coordinate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct BlockPos {
    pub x: i32,
    pub y: i32,
    pub z: i32,
}

/// Chunk coordinate (block coordinate divided by [`CHUNK_SIZE`], rounded toward -∞).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ChunkPos {
    pub x: i32,
    pub y: i32,
    pub z: i32,
}

/// Block coordinate inside a chunk, each component in `0..CHUNK_SIZE`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LocalPos {
    pub x: u8,
    pub y: u8,
    pub z: u8,
}

impl BlockPos {
    pub const fn new(x: i32, y: i32, z: i32) -> Self {
        Self { x, y, z }
    }

    pub fn chunk(self) -> ChunkPos {
        ChunkPos::new(
            self.x.div_euclid(CHUNK_SIZE),
            self.y.div_euclid(CHUNK_SIZE),
            self.z.div_euclid(CHUNK_SIZE),
        )
    }

    pub fn local(self) -> LocalPos {
        LocalPos {
            x: self.x.rem_euclid(CHUNK_SIZE) as u8,
            y: self.y.rem_euclid(CHUNK_SIZE) as u8,
            z: self.z.rem_euclid(CHUNK_SIZE) as u8,
        }
    }

    pub const fn offset(self, dx: i32, dy: i32, dz: i32) -> Self {
        Self::new(self.x + dx, self.y + dy, self.z + dz)
    }

    /// Block containing the given world-space point.
    pub fn containing(p: [f32; 3]) -> Self {
        Self::new(p[0].floor() as i32, p[1].floor() as i32, p[2].floor() as i32)
    }
}

impl ChunkPos {
    pub const fn new(x: i32, y: i32, z: i32) -> Self {
        Self { x, y, z }
    }

    /// World position of this chunk's minimum corner block.
    pub const fn origin(self) -> BlockPos {
        BlockPos::new(self.x * CHUNK_SIZE, self.y * CHUNK_SIZE, self.z * CHUNK_SIZE)
    }

    pub const fn offset(self, dx: i32, dy: i32, dz: i32) -> Self {
        Self::new(self.x + dx, self.y + dy, self.z + dz)
    }
}

impl LocalPos {
    /// Panics in debug builds if a component is out of range.
    pub fn new(x: u8, y: u8, z: u8) -> Self {
        debug_assert!(i32::from(x.max(y).max(z)) < CHUNK_SIZE);
        Self { x, y, z }
    }

    /// Index into dense chunk storage (x fastest, then z, then y).
    pub fn index(self) -> usize {
        let s = CHUNK_SIZE as usize;
        self.x as usize + self.z as usize * s + self.y as usize * s * s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn negative_coordinates_map_to_correct_chunk() {
        let p = BlockPos::new(-1, -16, -17);
        assert_eq!(p.chunk(), ChunkPos::new(-1, -1, -2));
        assert_eq!(p.local(), LocalPos::new(15, 0, 15));
    }

    #[test]
    fn chunk_and_local_round_trip() {
        for v in [-33, -17, -16, -1, 0, 1, 15, 16, 31, 1000] {
            let p = BlockPos::new(v, -v, v * 3);
            let o = p.chunk().origin();
            let l = p.local();
            assert_eq!(
                o.offset(l.x.into(), l.y.into(), l.z.into()),
                p,
                "round trip failed for {p:?}"
            );
        }
    }

    #[test]
    fn containing_floors_negative_points() {
        assert_eq!(BlockPos::containing([-0.1, 0.0, 2.9]), BlockPos::new(-1, 0, 2));
    }
}
