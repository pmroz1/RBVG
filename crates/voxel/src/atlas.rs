//! Block texture atlas: one padded tile per registered block, built from content data.
//!
//! Tiles are generated deterministically from each block's colour and persistent id (no
//! image assets yet), so mod blocks get textures without shipping files. Every tile has a
//! [`PAD`]-texel border copied from its edge, so filtering never bleeds into neighbours.
//! Missing-content placeholders get a magenta/black checkerboard. The atlas is plain
//! sRGB RGBA8 bytes; the client uploads it as one shared material texture.

use crate::registry::{BlockId, BlockRegistry};

/// Edge length of one block texture, in texels.
pub const TILE: u32 = 16;
/// Border copied from the tile edge on every side.
pub const PAD: u32 = 1;
const CELL: u32 = TILE + 2 * PAD;

/// Square RGBA8 (sRGB) atlas with one tile per runtime block id, plus a fallback tile.
#[derive(Debug, Clone)]
pub struct BlockAtlas {
    /// Edge length in texels.
    pub size: u32,
    /// `size * size * 4` bytes, row-major, top row first.
    pub pixels: Vec<u8>,
    columns: u32,
    tiles: u32,
}

impl BlockAtlas {
    /// Builds tiles for every block in `registry`; the last tile is the fallback used for
    /// ids the registry does not know.
    pub fn build(registry: &BlockRegistry) -> Self {
        let tiles = registry.len() as u32 + 1;
        let columns = (tiles as f64).sqrt().ceil() as u32;
        let size = columns * CELL;
        let mut atlas = Self {
            size,
            pixels: vec![0; (size * size * 4) as usize],
            columns,
            tiles,
        };
        let mut tile = [[0u8; 4]; (TILE * TILE) as usize];
        for (id, def) in registry.iter() {
            if def.missing {
                checker(&mut tile);
            } else {
                pattern(&mut tile, &def.id, def.color);
            }
            atlas.write(u32::from(id.0), &tile);
        }
        checker(&mut tile);
        atlas.write(tiles - 1, &tile);
        atlas
    }

    /// Maps a face-local `uv` (`0..=1`) into the tile of `block`.
    pub fn uv(&self, block: BlockId, uv: [f32; 2]) -> [f32; 2] {
        let index = u32::from(block.0).min(self.tiles - 1);
        let (x, y) = self.origin(index);
        let size = self.size as f32;
        [
            (x as f32 + uv[0] * TILE as f32) / size,
            (y as f32 + uv[1] * TILE as f32) / size,
        ]
    }

    /// Top-left texel of the unpadded tile `index`.
    fn origin(&self, index: u32) -> (u32, u32) {
        (
            index % self.columns * CELL + PAD,
            index / self.columns * CELL + PAD,
        )
    }

    fn write(&mut self, index: u32, tile: &[[u8; 4]]) {
        let (ox, oy) = self.origin(index);
        let last = TILE as i32 - 1;
        // Iterate over the padded cell and clamp into the tile, replicating its edges.
        for cy in -(PAD as i32)..(TILE + PAD) as i32 {
            for cx in -(PAD as i32)..(TILE + PAD) as i32 {
                let src = tile[(cy.clamp(0, last) * TILE as i32 + cx.clamp(0, last)) as usize];
                let x = (ox as i32 + cx) as u32;
                let y = (oy as i32 + cy) as u32;
                let i = ((y * self.size + x) * 4) as usize;
                self.pixels[i..i + 4].copy_from_slice(&src);
            }
        }
    }
}

/// Deterministic speckle in the block colour, slightly lighter at the top so the
/// orientation of side faces is visible.
fn pattern(tile: &mut [[u8; 4]], id: &str, color: [f32; 3]) {
    let seed = id.bytes().fold(0x811c_9dc5u32, |h, b| {
        (h ^ u32::from(b)).wrapping_mul(0x0100_0193)
    });
    for (i, texel) in tile.iter_mut().enumerate() {
        let (x, y) = (i as u32 % TILE, i as u32 / TILE);
        let mut h = seed ^ x.wrapping_mul(0x9e37_79b1) ^ y.wrapping_mul(0x85eb_ca77);
        h ^= h >> 15;
        h = h.wrapping_mul(0x2c1b_3c6d);
        h ^= h >> 12;
        let noise = (h & 0xff) as f32 / 255.0;
        let shade = (0.8 + 0.2 * noise) * (1.05 - 0.1 * y as f32 / (TILE - 1) as f32);
        let c = |v: f32| (v * shade * 255.0).round().clamp(0.0, 255.0) as u8;
        *texel = [c(color[0]), c(color[1]), c(color[2]), 255];
    }
}

fn checker(tile: &mut [[u8; 4]]) {
    for (i, texel) in tile.iter_mut().enumerate() {
        let (x, y) = (i as u32 % TILE, i as u32 / TILE);
        *texel = if (x / 4 + y / 4) % 2 == 0 {
            [255, 0, 255, 255]
        } else {
            [0, 0, 0, 255]
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn registry() -> BlockRegistry {
        let mut r = BlockRegistry::default();
        r.load_toml(
            "core",
            "format_version = 1\n\
             [[blocks]]\nid = \"core:stone\"\ndisplay_name = \"Stone\"\ncolor = [0.5, 0.5, 0.5]\n\
             [[blocks]]\nid = \"core:grass\"\ndisplay_name = \"Grass\"\ncolor = [0.2, 0.6, 0.2]",
        )
        .unwrap();
        r.register_missing("gone:block").unwrap();
        r
    }

    fn texel(a: &BlockAtlas, x: u32, y: u32) -> [u8; 4] {
        let i = ((y * a.size + x) * 4) as usize;
        a.pixels[i..i + 4].try_into().unwrap()
    }

    fn px(a: &BlockAtlas, uv: [f32; 2]) -> (u32, u32) {
        (
            (uv[0] * a.size as f32) as u32,
            (uv[1] * a.size as f32) as u32,
        )
    }

    #[test]
    fn tiles_are_padded_disjoint_and_deterministic() {
        let r = registry();
        let a = BlockAtlas::build(&r);
        assert_eq!(a.pixels.len(), (a.size * a.size * 4) as usize);
        assert_eq!(a.pixels, BlockAtlas::build(&r).pixels);
        let mut seen = Vec::new();
        for (id, _) in r.iter() {
            let (x0, y0) = px(&a, a.uv(id, [0.0, 0.0]));
            let (x1, y1) = px(&a, a.uv(id, [1.0, 1.0]));
            assert_eq!((x1 - x0, y1 - y0), (TILE, TILE));
            // Padding replicates the edge on all four sides.
            for i in 0..TILE {
                assert_eq!(texel(&a, x0 - 1, y0 + i), texel(&a, x0, y0 + i));
                assert_eq!(texel(&a, x1, y0 + i), texel(&a, x1 - 1, y0 + i));
                assert_eq!(texel(&a, x0 + i, y0 - 1), texel(&a, x0 + i, y0));
                assert_eq!(texel(&a, x0 + i, y1), texel(&a, x0 + i, y1 - 1));
            }
            assert!(!seen.contains(&(x0, y0)));
            seen.push((x0, y0));
        }
    }

    #[test]
    fn tiles_follow_content_and_missing_blocks_are_marked() {
        let r = registry();
        let a = BlockAtlas::build(&r);
        let grass = px(&a, a.uv(r.id("core:grass").unwrap(), [0.5, 0.5]));
        let [red, green, blue, alpha] = texel(&a, grass.0, grass.1);
        assert!(green > red && green > blue && alpha == 255);
        let missing = px(&a, a.uv(r.id("gone:block").unwrap(), [0.0, 0.0]));
        assert_eq!(texel(&a, missing.0, missing.1), [255, 0, 255, 255]);
        // Unknown ids fall back to the checkerboard instead of another block's tile.
        let unknown = px(&a, a.uv(BlockId(u16::MAX), [0.0, 0.0]));
        assert_eq!(texel(&a, unknown.0, unknown.1), [255, 0, 255, 255]);
        assert_ne!(unknown, missing);
    }
}
