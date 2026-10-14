//! Versioned world save format.
//!
//! A save directory contains:
//! - `world.toml`: [`WorldMeta`] (format version, seed, player position).
//! - `chunks.bin`: only *edited* chunks; everything else is regenerated from the seed.
//!
//! `chunks.bin` layout (little-endian): magic `RBVGCHNK`, `u32` format version,
//! `u32` palette length, palette entries (`u16` byte length + UTF-8 namespaced block id),
//! `u32` chunk count, then per chunk `i32 x, y, z` and `CHUNK_VOLUME` `u16` palette indices.
//! The palette decouples saves from runtime block ids, which change with mod sets.
//!
//! Each file is written to a temporary sibling and renamed into place, so a crash never
//! leaves a half-written file. `chunks.bin` is written before `world.toml`.

use std::fs;
use std::io::{self, Read, Write};
use std::path::Path;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::chunk::Chunk;
use crate::coords::{CHUNK_VOLUME, ChunkPos};
use crate::registry::{BlockId, BlockRegistry};
use crate::world::VoxelWorld;

pub const SAVE_FORMAT_VERSION: u32 = 1;
const MAGIC: &[u8; 8] = b"RBVGCHNK";
const META_FILE: &str = "world.toml";
const CHUNKS_FILE: &str = "chunks.bin";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorldMeta {
    pub format_version: u32,
    pub seed: u32,
    #[serde(default)]
    pub player_position: Option<[f32; 3]>,
}

#[derive(Debug, Error)]
pub enum PersistError {
    #[error("i/o error: {0}")]
    Io(#[from] io::Error),
    #[error("invalid world.toml: {0}")]
    Meta(#[from] toml::de::Error),
    #[error("failed to encode world.toml: {0}")]
    MetaEncode(#[from] toml::ser::Error),
    #[error("unsupported save format version {0} (supported: {SAVE_FORMAT_VERSION})")]
    UnsupportedVersion(u32),
    #[error("corrupt chunk file: {0}")]
    Corrupt(&'static str),
    #[error("save uses block `{0}`, which is not registered by the current content/mods")]
    UnknownBlock(String),
}

/// Saves world metadata and every edited chunk into `dir` (created if missing).
pub fn save_world(
    dir: &Path,
    world: &VoxelWorld,
    registry: &BlockRegistry,
    player_position: Option<[f32; 3]>,
) -> Result<(), PersistError> {
    fs::create_dir_all(dir)?;
    write_atomic(&dir.join(CHUNKS_FILE), &encode_chunks(world, registry))?;
    let meta = WorldMeta {
        format_version: SAVE_FORMAT_VERSION,
        seed: world.seed(),
        player_position,
    };
    write_atomic(&dir.join(META_FILE), toml::to_string(&meta)?.as_bytes())?;
    Ok(())
}

pub fn load_meta(dir: &Path) -> Result<WorldMeta, PersistError> {
    let meta: WorldMeta = toml::from_str(&fs::read_to_string(dir.join(META_FILE))?)?;
    if meta.format_version != SAVE_FORMAT_VERSION {
        return Err(PersistError::UnsupportedVersion(meta.format_version));
    }
    Ok(meta)
}

/// Reads edited chunks, translating palette names to current runtime ids.
pub fn load_chunks(
    dir: &Path,
    registry: &BlockRegistry,
) -> Result<Vec<(ChunkPos, Chunk)>, PersistError> {
    decode_chunks(&fs::read(dir.join(CHUNKS_FILE))?, registry)
}

/// Like [`load_chunks`], but blocks whose content is not installed (e.g. a disabled mod)
/// are registered as "missing" placeholders, so saving again keeps their persistent ids.
pub fn load_chunks_lenient(
    dir: &Path,
    registry: &mut BlockRegistry,
) -> Result<Vec<(ChunkPos, Chunk)>, PersistError> {
    let bytes = fs::read(dir.join(CHUNKS_FILE))?;
    for name in palette_names(&bytes)? {
        if registry.id(&name).is_none() {
            registry
                .register_missing(&name)
                .map_err(|_| PersistError::Corrupt("invalid block id in palette"))?;
        }
    }
    decode_chunks(&bytes, registry)
}

fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let tmp = path.with_extension("tmp");
    {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    fs::rename(tmp, path)
}

fn encode_chunks(world: &VoxelWorld, registry: &BlockRegistry) -> Vec<u8> {
    let chunks: Vec<_> = world.modified_chunks().collect();
    let mut out = Vec::with_capacity(16 + chunks.len() * (12 + CHUNK_VOLUME * 2));
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&SAVE_FORMAT_VERSION.to_le_bytes());
    // Palette = full registry; small, and keeps indices equal to runtime ids on save.
    out.extend_from_slice(&(registry.len() as u32).to_le_bytes());
    for (_, def) in registry.iter() {
        out.extend_from_slice(&(def.id.len() as u16).to_le_bytes());
        out.extend_from_slice(def.id.as_bytes());
    }
    out.extend_from_slice(&(chunks.len() as u32).to_le_bytes());
    for (pos, chunk) in chunks {
        for v in [pos.x, pos.y, pos.z] {
            out.extend_from_slice(&v.to_le_bytes());
        }
        for b in chunk.blocks() {
            out.extend_from_slice(&b.0.to_le_bytes());
        }
    }
    out
}

fn read_header(r: &mut &[u8]) -> Result<Vec<String>, PersistError> {
    let mut magic = [0u8; 8];
    read(r, &mut magic)?;
    if &magic != MAGIC {
        return Err(PersistError::Corrupt("bad magic"));
    }
    let version = read_u32(r)?;
    if version != SAVE_FORMAT_VERSION {
        return Err(PersistError::UnsupportedVersion(version));
    }
    let palette_len = read_u32(r)? as usize;
    if palette_len > usize::from(u16::MAX) + 1 {
        return Err(PersistError::Corrupt("palette too large"));
    }
    let mut names = Vec::with_capacity(palette_len);
    for _ in 0..palette_len {
        let mut len = [0u8; 2];
        read(r, &mut len)?;
        let mut name = vec![0u8; usize::from(u16::from_le_bytes(len))];
        read(r, &mut name)?;
        let name =
            String::from_utf8(name).map_err(|_| PersistError::Corrupt("palette not UTF-8"))?;
        names.push(name);
    }
    Ok(names)
}

fn palette_names(mut bytes: &[u8]) -> Result<Vec<String>, PersistError> {
    read_header(&mut bytes)
}

fn decode_chunks(
    mut bytes: &[u8],
    registry: &BlockRegistry,
) -> Result<Vec<(ChunkPos, Chunk)>, PersistError> {
    let r = &mut bytes;
    let mut palette = Vec::new();
    for name in read_header(r)? {
        palette.push(registry.id(&name).ok_or(PersistError::UnknownBlock(name))?);
    }
    let count = read_u32(r)? as usize;
    let mut chunks = Vec::with_capacity(count.min(r.len() / (CHUNK_VOLUME * 2)));
    for _ in 0..count {
        let pos = ChunkPos::new(read_i32(r)?, read_i32(r)?, read_i32(r)?);
        let mut blocks = Box::new([BlockId::AIR; CHUNK_VOLUME]);
        for b in blocks.iter_mut() {
            let mut v = [0u8; 2];
            read(r, &mut v)?;
            *b = *palette
                .get(usize::from(u16::from_le_bytes(v)))
                .ok_or(PersistError::Corrupt("palette index out of range"))?;
        }
        chunks.push((pos, Chunk::from_blocks(blocks)));
    }
    if !r.is_empty() {
        return Err(PersistError::Corrupt("trailing data"));
    }
    Ok(chunks)
}

fn read(r: &mut &[u8], buf: &mut [u8]) -> Result<(), PersistError> {
    r.read_exact(buf)
        .map_err(|_| PersistError::Corrupt("unexpected end of file"))
}

fn read_u32(r: &mut &[u8]) -> Result<u32, PersistError> {
    let mut b = [0u8; 4];
    read(r, &mut b)?;
    Ok(u32::from_le_bytes(b))
}

fn read_i32(r: &mut &[u8]) -> Result<i32, PersistError> {
    let mut b = [0u8; 4];
    read(r, &mut b)?;
    Ok(i32::from_le_bytes(b))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coords::BlockPos;
    use crate::registry::tests::test_registry;
    use crate::world::tests::flat_world;

    #[test]
    fn round_trip_preserves_edits_and_meta() {
        let reg = test_registry();
        let mut w = flat_world(1);
        let p = BlockPos::new(-5, 3, 7);
        let stone = reg.id("core:stone").unwrap();
        w.set_block(p, stone).unwrap();
        let dir = std::env::temp_dir().join(format!("rbvg_save_test_{}", std::process::id()));
        save_world(&dir, &w, &reg, Some([1.0, 2.0, 3.0])).unwrap();

        let meta = load_meta(&dir).unwrap();
        assert_eq!(meta.seed, 0);
        assert_eq!(meta.player_position, Some([1.0, 2.0, 3.0]));
        let chunks = load_chunks(&dir, &reg).unwrap();
        assert_eq!(chunks.len(), 1);
        let (pos, chunk) = &chunks[0];
        assert_eq!(*pos, p.chunk());
        assert_eq!(chunk.get(p.local()), stone);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn missing_content_survives_a_lenient_round_trip() {
        let reg = test_registry();
        let mut w = flat_world(1);
        let p = BlockPos::new(2, 2, 2);
        w.set_block(p, reg.id("core:stone").unwrap()).unwrap();
        let bytes = encode_chunks(&w, &reg);
        let dir = std::env::temp_dir().join(format!("rbvg_missing_test_{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(CHUNKS_FILE), &bytes).unwrap();
        // "Mod removed": the new registry only knows air.
        let mut bare = crate::BlockRegistry::default();
        let chunks = load_chunks_lenient(&dir, &mut bare).unwrap();
        let id = chunk_block(&chunks, p);
        assert_eq!(bare.name(id), "core:stone");
        assert!(bare.is_solid(id));
        fs::remove_dir_all(&dir).unwrap();
    }

    fn chunk_block(chunks: &[(ChunkPos, Chunk)], p: BlockPos) -> BlockId {
        chunks
            .iter()
            .find(|(c, _)| *c == p.chunk())
            .unwrap()
            .1
            .get(p.local())
    }

    #[test]
    fn rejects_unknown_blocks_and_corruption() {
        let reg = test_registry();
        let mut w = flat_world(1);
        w.set_block(BlockPos::new(0, 0, 0), BlockId(1)).unwrap();
        let bytes = encode_chunks(&w, &reg);
        assert!(matches!(
            decode_chunks(&bytes, &crate::BlockRegistry::default()),
            Err(PersistError::UnknownBlock(_))
        ));
        assert!(matches!(
            decode_chunks(&bytes[..bytes.len() - 1], &reg),
            Err(PersistError::Corrupt(_))
        ));
        assert!(matches!(
            decode_chunks(b"NOTASAVE", &reg),
            Err(PersistError::Corrupt("bad magic"))
        ));
    }
}
