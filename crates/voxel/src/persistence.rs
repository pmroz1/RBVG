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

use std::collections::HashSet;
use std::fs;
use std::io::{self, Read, Write};
use std::path::Path;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::chunk::Chunk;
use crate::coords::{CHUNK_VOLUME, ChunkPos};
use crate::registry::{BlockId, BlockRegistry, parse_namespaced};
use crate::world::VoxelWorld;

pub const SAVE_FORMAT_VERSION: u32 = 1;
const MAGIC: &[u8; 8] = b"RBVGCHNK";
const META_FILE: &str = "world.toml";
const CHUNKS_FILE: &str = "chunks.bin";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorldMeta {
    pub format_version: u32,
    pub seed: u32,
    #[serde(default)]
    pub player_position: Option<[f32; 3]>,
}

/// How loading handles persistent block names not provided by installed content.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoadPolicy {
    Strict,
    PreserveMissing,
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
    #[error("cannot save unregistered runtime block id {0:?}")]
    UnknownRuntimeBlock(BlockId),
    #[error("player position must contain only finite coordinates")]
    InvalidPlayerPosition,
}

/// Saves world metadata and every edited chunk into `dir` (created if missing).
pub fn save_world(
    dir: &Path,
    world: &VoxelWorld,
    registry: &BlockRegistry,
    player_position: Option<[f32; 3]>,
) -> Result<(), PersistError> {
    let meta = WorldMeta {
        format_version: SAVE_FORMAT_VERSION,
        seed: world.seed(),
        player_position,
    };
    validate_meta(&meta)?;
    let chunks = encode_chunks(world, registry)?;
    let meta = toml::to_string(&meta)?;
    fs::create_dir_all(dir)?;
    write_atomic(&dir.join(CHUNKS_FILE), &chunks)?;
    write_atomic(&dir.join(META_FILE), meta.as_bytes())?;
    Ok(())
}

pub fn load_meta(dir: &Path) -> Result<WorldMeta, PersistError> {
    let meta: WorldMeta = toml::from_str(&fs::read_to_string(dir.join(META_FILE))?)?;
    validate_meta(&meta)?;
    Ok(meta)
}

fn validate_meta(meta: &WorldMeta) -> Result<(), PersistError> {
    if meta.format_version != SAVE_FORMAT_VERSION {
        return Err(PersistError::UnsupportedVersion(meta.format_version));
    }
    if meta
        .player_position
        .is_some_and(|pos| pos.iter().any(|v| !v.is_finite()))
    {
        return Err(PersistError::InvalidPlayerPosition);
    }
    Ok(())
}

/// Reads edited chunks, translating palette names to current runtime ids.
pub fn load_chunks(
    dir: &Path,
    registry: &BlockRegistry,
) -> Result<Vec<(ChunkPos, Chunk)>, PersistError> {
    decode_chunks(
        &fs::read(dir.join(CHUNKS_FILE))?,
        registry,
        LoadPolicy::Strict,
    )
    .map(|(_, chunks)| chunks)
}

/// Reads edited chunks under `policy`. The registry is unchanged on any load error.
pub fn load_chunks_with_policy(
    dir: &Path,
    registry: &mut BlockRegistry,
    policy: LoadPolicy,
) -> Result<Vec<(ChunkPos, Chunk)>, PersistError> {
    let (staged, chunks) = decode_chunks(&fs::read(dir.join(CHUNKS_FILE))?, registry, policy)?;
    *registry = staged;
    Ok(chunks)
}

/// Like [`load_chunks`], but blocks whose content is not installed (e.g. a disabled mod)
/// are registered as "missing" placeholders, so saving again keeps their persistent ids.
pub fn load_chunks_lenient(
    dir: &Path,
    registry: &mut BlockRegistry,
) -> Result<Vec<(ChunkPos, Chunk)>, PersistError> {
    load_chunks_with_policy(dir, registry, LoadPolicy::PreserveMissing)
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

fn encode_chunks(world: &VoxelWorld, registry: &BlockRegistry) -> Result<Vec<u8>, PersistError> {
    let chunks: Vec<_> = world.modified_chunks().collect();
    let mut used = vec![false; registry.len()];
    used[0] = true;
    for (_, chunk) in &chunks {
        for b in chunk.blocks() {
            *used
                .get_mut(usize::from(b.0))
                .ok_or(PersistError::UnknownRuntimeBlock(*b))? = true;
        }
    }
    let palette: Vec<_> = registry
        .iter()
        .filter(|(id, _)| used[usize::from(id.0)])
        .collect();
    let mut indices = vec![0u16; registry.len()];
    let mut out = Vec::with_capacity(16 + chunks.len() * (12 + CHUNK_VOLUME * 2));
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&SAVE_FORMAT_VERSION.to_le_bytes());
    // Only referenced blocks survive in the palette, so removed placeholders disappear.
    out.extend_from_slice(&(palette.len() as u32).to_le_bytes());
    for (index, (id, def)) in palette.into_iter().enumerate() {
        indices[usize::from(id.0)] = index as u16;
        out.extend_from_slice(&(def.id.len() as u16).to_le_bytes());
        out.extend_from_slice(def.id.as_bytes());
    }
    let count = u32::try_from(chunks.len())
        .map_err(|_| PersistError::Corrupt("too many chunks to save"))?;
    out.extend_from_slice(&count.to_le_bytes());
    for (pos, chunk) in chunks {
        for v in [pos.x, pos.y, pos.z] {
            out.extend_from_slice(&v.to_le_bytes());
        }
        for b in chunk.blocks() {
            out.extend_from_slice(&indices[usize::from(b.0)].to_le_bytes());
        }
    }
    Ok(out)
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
    if palette_len == 0 {
        return Err(PersistError::Corrupt("empty palette"));
    }
    if palette_len > usize::from(u16::MAX) + 1 {
        return Err(PersistError::Corrupt("palette too large"));
    }
    let mut names = Vec::with_capacity(palette_len);
    let mut seen = HashSet::with_capacity(palette_len);
    for _ in 0..palette_len {
        let mut len = [0u8; 2];
        read(r, &mut len)?;
        let mut name = vec![0u8; usize::from(u16::from_le_bytes(len))];
        read(r, &mut name)?;
        let name =
            String::from_utf8(name).map_err(|_| PersistError::Corrupt("palette not UTF-8"))?;
        parse_namespaced(&name)
            .map_err(|_| PersistError::Corrupt("invalid block id in palette"))?;
        if !seen.insert(name.clone()) {
            return Err(PersistError::Corrupt("duplicate block id in palette"));
        }
        names.push(name);
    }
    Ok(names)
}

fn decode_chunks(
    mut bytes: &[u8],
    registry: &BlockRegistry,
    policy: LoadPolicy,
) -> Result<(BlockRegistry, Vec<(ChunkPos, Chunk)>), PersistError> {
    let mut staged = registry.clone();
    let r = &mut bytes;
    let mut palette = Vec::new();
    for name in read_header(r)? {
        let id = match staged.id(&name) {
            Some(id)
                if policy == LoadPolicy::PreserveMissing
                    || !staged.get(id).is_some_and(|def| def.missing) =>
            {
                id
            }
            _ if policy == LoadPolicy::Strict => return Err(PersistError::UnknownBlock(name)),
            _ => staged
                .register_missing(&name)
                .map_err(|_| PersistError::Corrupt("cannot register missing block"))?,
        };
        palette.push(id);
    }
    let count = read_u32(r)? as usize;
    if count.checked_mul(12 + CHUNK_VOLUME * 2) != Some(r.len()) {
        return Err(PersistError::Corrupt(
            "chunk count does not match file length",
        ));
    }
    let mut chunks = Vec::with_capacity(count);
    let mut positions = HashSet::with_capacity(count);
    for _ in 0..count {
        let pos = ChunkPos::new(read_i32(r)?, read_i32(r)?, read_i32(r)?);
        if !positions.insert(pos) {
            return Err(PersistError::Corrupt("duplicate chunk position"));
        }
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
    Ok((staged, chunks))
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

    const MOD_BLOCK: &str = r#"
        format_version = 1
        [[blocks]]
        id = "example:glass"
        display_name = "Original glass"
        solid = false
        color = [0.2, 0.4, 0.6]
    "#;

    fn test_dir(name: &str) -> std::path::PathBuf {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/test-data")
            .join(format!("persistence_{name}_{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn round_trip_preserves_edits_and_meta() {
        let reg = test_registry();
        let mut w = flat_world(1);
        let p = BlockPos::new(-5, 3, 7);
        let stone = reg.id("core:stone").unwrap();
        w.set_block(p, stone).unwrap();
        let dir = test_dir("round_trip");
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
    fn disabled_mod_survives_save_and_restores_its_original_definition() {
        let mut reg = test_registry();
        reg.load_toml("example", MOD_BLOCK).unwrap();
        let mut w = flat_world(1);
        let p = BlockPos::new(2, 2, 2);
        w.set_block(p, reg.id("example:glass").unwrap()).unwrap();
        let dir = test_dir("missing");
        save_world(&dir, &w, &reg, None).unwrap();
        let mut bare = test_registry();
        assert!(matches!(
            load_chunks_with_policy(&dir, &mut bare, LoadPolicy::Strict),
            Err(PersistError::UnknownBlock(name)) if name == "example:glass"
        ));
        assert!(bare.id("example:glass").is_none());
        let chunks = load_chunks_lenient(&dir, &mut bare).unwrap();
        let id = chunk_block(&chunks, p);
        assert_eq!(bare.name(id), "example:glass");
        assert!(bare.is_solid(id));
        assert!(bare.get(id).unwrap().missing);
        assert_eq!(bare.get(id).unwrap().color, [1.0, 0.0, 1.0]);
        assert!(matches!(
            load_chunks(&dir, &bare),
            Err(PersistError::UnknownBlock(_))
        ));
        let mut restored = flat_world(1);
        for (pos, chunk) in chunks {
            restored.insert_saved(pos, chunk);
        }
        save_world(&dir, &restored, &bare, None).unwrap();
        // Reinstall in a different registration order: saved names, not runtime ids, win.
        let mut fresh = BlockRegistry::default();
        fresh.load_toml("example", MOD_BLOCK).unwrap();
        fresh
            .load_toml("core", include_str!("../../../content/core/blocks.toml"))
            .unwrap();
        let reloaded = load_chunks_with_policy(&dir, &mut fresh, LoadPolicy::Strict).unwrap();
        let id = chunk_block(&reloaded, p);
        assert_ne!(id, reg.id("example:glass").unwrap());
        assert_eq!(fresh.name(id), "example:glass");
        let def = fresh.get(id).unwrap();
        assert!(!def.missing);
        assert!(!def.solid);
        assert_eq!(def.display_name, "Original glass");
        assert_eq!(def.color, [0.2, 0.4, 0.6]);
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
        let bytes = encode_chunks(&w, &reg).unwrap();
        assert!(matches!(
            decode_chunks(&bytes, &crate::BlockRegistry::default(), LoadPolicy::Strict),
            Err(PersistError::UnknownBlock(_))
        ));
        assert!(matches!(
            decode_chunks(&bytes[..bytes.len() - 1], &reg, LoadPolicy::Strict),
            Err(PersistError::Corrupt(_))
        ));
        assert!(matches!(
            decode_chunks(b"NOTASAVE", &reg, LoadPolicy::Strict),
            Err(PersistError::Corrupt("bad magic"))
        ));
    }

    #[test]
    fn corrupt_load_never_changes_the_registry() {
        let mut installed = test_registry();
        installed.load_toml("example", MOD_BLOCK).unwrap();
        let mut world = flat_world(1);
        let p = BlockPos::new(2, 2, 2);
        world
            .set_block(p, installed.id("example:glass").unwrap())
            .unwrap();
        let valid = encode_chunks(&world, &installed).unwrap();
        let mut bad_index = valid.clone();
        let first_block = valid.len() - CHUNK_VOLUME * 2;
        bad_index[first_block..first_block + 2].copy_from_slice(&u16::MAX.to_le_bytes());
        let mut trailing = valid.clone();
        trailing.push(0);
        let mut unsupported = valid.clone();
        unsupported[8..12].copy_from_slice(&2u32.to_le_bytes());
        let dir = test_dir("corrupt_atomic");
        for bytes in [
            valid[..valid.len() - 1].to_vec(),
            bad_index,
            trailing,
            unsupported,
        ] {
            fs::write(dir.join(CHUNKS_FILE), bytes).unwrap();
            let mut registry = test_registry();
            let before: Vec<_> = registry
                .iter()
                .map(|(id, def)| (id, def.id.clone(), def.missing))
                .collect();
            assert!(load_chunks_lenient(&dir, &mut registry).is_err());
            assert_eq!(
                registry
                    .iter()
                    .map(|(id, def)| (id, def.id.clone(), def.missing))
                    .collect::<Vec<_>>(),
                before
            );
            assert!(registry.id("example:glass").is_none());
        }
        fs::remove_dir_all(dir).unwrap();
    }

    fn header(names: &[&str]) -> Vec<u8> {
        let mut bytes = MAGIC.to_vec();
        bytes.extend_from_slice(&SAVE_FORMAT_VERSION.to_le_bytes());
        bytes.extend_from_slice(&(names.len() as u32).to_le_bytes());
        for name in names {
            bytes.extend_from_slice(&(name.len() as u16).to_le_bytes());
            bytes.extend_from_slice(name.as_bytes());
        }
        bytes.extend_from_slice(&0u32.to_le_bytes());
        bytes
    }

    #[test]
    fn refuses_malformed_palettes_and_duplicate_chunks() {
        let registry = test_registry();
        for names in [
            vec![],
            vec!["bad:name:extra"],
            vec!["Bad:name"],
            vec!["core:air", "core:air"],
        ] {
            assert!(matches!(
                decode_chunks(&header(&names), &registry, LoadPolicy::PreserveMissing),
                Err(PersistError::Corrupt(_))
            ));
        }
        let mut bytes = header(&["core:air"]);
        bytes[18] = 0xff;
        assert!(matches!(
            decode_chunks(&bytes, &registry, LoadPolicy::PreserveMissing),
            Err(PersistError::Corrupt("palette not UTF-8"))
        ));
        let mut world = flat_world(1);
        for p in [BlockPos::new(0, 1, 0), BlockPos::new(16, 1, 0)] {
            world.load_chunk(p.chunk());
            world
                .set_block(p, registry.id("core:stone").unwrap())
                .unwrap();
        }
        let mut bytes = encode_chunks(&world, &registry).unwrap();
        let record_size = 12 + CHUNK_VOLUME * 2;
        let first = bytes.len() - record_size * 2;
        let second = first + record_size;
        let first_pos: [u8; 12] = bytes[first..first + 12].try_into().unwrap();
        bytes[second..second + 12].copy_from_slice(&first_pos);
        assert!(matches!(
            decode_chunks(&bytes, &registry, LoadPolicy::PreserveMissing),
            Err(PersistError::Corrupt("duplicate chunk position"))
        ));
    }

    #[test]
    fn saves_only_referenced_definitions() {
        let mut registry = test_registry();
        let missing = registry.register_missing("removed:unused").unwrap();
        let mut world = flat_world(1);
        world.set_block(BlockPos::new(2, 2, 2), missing).unwrap();
        world
            .set_block(BlockPos::new(2, 2, 2), registry.id("core:stone").unwrap())
            .unwrap();
        let bytes = encode_chunks(&world, &registry).unwrap();
        let (loaded, _) =
            decode_chunks(&bytes, &test_registry(), LoadPolicy::PreserveMissing).unwrap();
        assert!(loaded.id("removed:unused").is_none());
        assert!(!loaded.iter().any(|(_, def)| def.missing));
    }

    #[test]
    fn still_loads_v1_full_registry_palettes() {
        let registry = test_registry();
        let names: Vec<_> = registry.iter().map(|(_, def)| def.id.as_str()).collect();
        let mut bytes = header(&names);
        let count = bytes.len() - 4;
        bytes[count..].copy_from_slice(&1u32.to_le_bytes());
        for coordinate in [-1i32, 0, 0] {
            bytes.extend_from_slice(&coordinate.to_le_bytes());
        }
        let stone = registry.id("core:stone").unwrap();
        for _ in 0..CHUNK_VOLUME {
            bytes.extend_from_slice(&stone.0.to_le_bytes());
        }
        let (_, chunks) = decode_chunks(&bytes, &registry, LoadPolicy::Strict).unwrap();
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].0, ChunkPos::new(-1, 0, 0));
        assert!(chunks[0].1.blocks().iter().all(|id| *id == stone));
    }

    #[test]
    fn rejects_nonfinite_metadata() {
        let dir = test_dir("metadata");
        for value in ["nan", "inf", "-inf"] {
            fs::write(
                dir.join(META_FILE),
                format!("format_version = 1\nseed = 7\nplayer_position = [{value}, 0.0, 0.0]\n"),
            )
            .unwrap();
            assert!(matches!(
                load_meta(&dir),
                Err(PersistError::InvalidPlayerPosition)
            ));
        }
        for invalid in [
            "format_version = 1\nseed = 'invalid'",
            "format_version = 1\nseed = 0\nplayer_position = [0.0, 0.0]",
            "format_version = 1\nseed = 0\nunknown = true",
        ] {
            fs::write(dir.join(META_FILE), invalid).unwrap();
            assert!(matches!(load_meta(&dir), Err(PersistError::Meta(_))));
        }
        fs::write(dir.join(META_FILE), "format_version = 2\nseed = 0").unwrap();
        assert!(matches!(
            load_meta(&dir),
            Err(PersistError::UnsupportedVersion(2))
        ));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn invalid_save_inputs_leave_existing_files_untouched() {
        let registry = test_registry();
        let mut world = flat_world(1);
        let p = BlockPos::new(2, 2, 2);
        world
            .set_block(p, registry.id("core:stone").unwrap())
            .unwrap();
        let dir = test_dir("save_validation");
        save_world(&dir, &world, &registry, Some([1.0, 2.0, 3.0])).unwrap();
        let meta = fs::read(dir.join(META_FILE)).unwrap();
        let chunks = fs::read(dir.join(CHUNKS_FILE)).unwrap();
        assert!(matches!(
            save_world(&dir, &world, &registry, Some([f32::NAN, 0.0, 0.0])),
            Err(PersistError::InvalidPlayerPosition)
        ));
        world.set_block(p, BlockId(u16::MAX)).unwrap();
        world.unload_chunk(p.chunk());
        assert!(matches!(
            save_world(&dir, &world, &registry, None),
            Err(PersistError::UnknownRuntimeBlock(BlockId(u16::MAX)))
        ));
        assert_eq!(fs::read(dir.join(META_FILE)).unwrap(), meta);
        assert_eq!(fs::read(dir.join(CHUNKS_FILE)).unwrap(), chunks);
        fs::remove_dir_all(dir).unwrap();
    }
}
