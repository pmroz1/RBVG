//! Replaceable, deterministic world generators.

use noise::{Fbm, MultiFractal, NoiseFn, Perlin};

use crate::chunk::Chunk;
use crate::coords::{BlockPos, CHUNK_SIZE, ChunkPos, LocalPos};
use crate::registry::{BlockId, BlockRegistry, RegistryError};

/// Produces the initial contents of a chunk. Must be deterministic for a given seed and
/// position, and thread-safe so generation can move to background tasks.
pub trait WorldGenerator: Send + Sync {
    fn generate(&self, pos: ChunkPos) -> Chunk;
}

/// Heightmap terrain: grass over dirt over stone, with occasional wooden trunks.
pub struct TerrainGenerator {
    seed: u32,
    height_noise: Fbm<Perlin>,
    grass: BlockId,
    dirt: BlockId,
    stone: BlockId,
    wood: BlockId,
}

/// Average surface height in blocks.
const BASE_HEIGHT: f64 = 24.0;
/// Peak deviation from the base height.
const HEIGHT_AMPLITUDE: f64 = 14.0;
const TRUNK_HEIGHT: i32 = 4;

impl TerrainGenerator {
    /// Requires `core:grass`, `core:dirt`, `core:stone` and `core:wood` to be registered.
    pub fn new(seed: u32, registry: &BlockRegistry) -> Result<Self, RegistryError> {
        Ok(Self {
            seed,
            height_noise: Fbm::<Perlin>::new(seed).set_octaves(4),
            grass: registry.require("core:grass")?,
            dirt: registry.require("core:dirt")?,
            stone: registry.require("core:stone")?,
            wood: registry.require("core:wood")?,
        })
    }

    /// Y of the top solid (grass) block in column `(x, z)`.
    pub fn surface_height(&self, x: i32, z: i32) -> i32 {
        let n = self
            .height_noise
            .get([f64::from(x) / 128.0, f64::from(z) / 128.0]);
        (BASE_HEIGHT + n * HEIGHT_AMPLITUDE).floor() as i32
    }

    fn has_tree(&self, x: i32, z: i32) -> bool {
        // Combine, then avalanche (murmur3 finaliser) so trees show no grid pattern.
        let mut h = (x as u32)
            .wrapping_mul(0x9E37_79B9)
            .wrapping_add((z as u32).wrapping_mul(0x85EB_CA6B))
            .wrapping_add(self.seed);
        h ^= h >> 16;
        h = h.wrapping_mul(0x85EB_CA6B);
        h ^= h >> 13;
        h = h.wrapping_mul(0xC2B2_AE35);
        h ^= h >> 16;
        h.is_multiple_of(97)
    }

    /// Block at a world position, computed from a precomputed column height.
    fn block_in_column(&self, y: i32, height: i32, tree: bool) -> BlockId {
        match y {
            y if y > height => {
                if tree && y <= height + TRUNK_HEIGHT {
                    self.wood
                } else {
                    BlockId::AIR
                }
            }
            y if y == height => self.grass,
            y if y > height - 4 => self.dirt,
            _ => self.stone,
        }
    }

    /// Block at any world position (convenience for tests and spawn placement).
    pub fn block_at(&self, p: BlockPos) -> BlockId {
        let h = self.surface_height(p.x, p.z);
        self.block_in_column(p.y, h, self.has_tree(p.x, p.z))
    }
}

impl WorldGenerator for TerrainGenerator {
    fn generate(&self, pos: ChunkPos) -> Chunk {
        let mut chunk = Chunk::default();
        let o = pos.origin();
        for lz in 0..CHUNK_SIZE {
            for lx in 0..CHUNK_SIZE {
                let (x, z) = (o.x + lx, o.z + lz);
                let h = self.surface_height(x, z);
                let tree = self.has_tree(x, z);
                if o.y > h + TRUNK_HEIGHT {
                    continue; // whole column segment is air
                }
                for ly in 0..CHUNK_SIZE {
                    let id = self.block_in_column(o.y + ly, h, tree);
                    if id != BlockId::AIR {
                        chunk.set_generated(LocalPos::new(lx as u8, ly as u8, lz as u8), id);
                    }
                }
            }
        }
        chunk
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::tests::test_registry;

    #[test]
    fn generation_is_deterministic_and_seed_dependent() {
        let reg = test_registry();
        let a = TerrainGenerator::new(7, &reg).unwrap();
        let b = TerrainGenerator::new(7, &reg).unwrap();
        let c = TerrainGenerator::new(8, &reg).unwrap();
        let pos = ChunkPos::new(-3, 1, 5);
        assert_eq!(a.generate(pos).blocks(), b.generate(pos).blocks());
        let differs = (-4..4).any(|x| a.surface_height(x * 16, 0) != c.surface_height(x * 16, 0));
        assert!(differs, "different seeds should give different terrain");
    }

    #[test]
    fn chunk_matches_point_queries() {
        let reg = test_registry();
        let g = TerrainGenerator::new(1, &reg).unwrap();
        let pos = ChunkPos::new(-1, 1, 0);
        let chunk = g.generate(pos);
        let o = pos.origin();
        for (lx, ly, lz) in [(0, 0, 0), (15, 15, 15), (3, 8, 12)] {
            let p = o.offset(lx, ly, lz);
            assert_eq!(chunk.get(p.local()), g.block_at(p));
        }
    }

    #[test]
    fn missing_core_block_is_an_error() {
        assert!(TerrainGenerator::new(0, &BlockRegistry::default()).is_err());
    }
}
