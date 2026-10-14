//! Block registry: maps persistent namespaced ids (`core:stone`) to compact runtime ids.
//!
//! Runtime [`BlockId`]s are assigned in registration order and are **not** stable across
//! different content/mod configurations. Anything persisted must store the namespaced id.

use std::collections::HashMap;

use serde::Deserialize;
use thiserror::Error;

/// Compact runtime block id. `0` is always air.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct BlockId(pub u16);

impl BlockId {
    pub const AIR: BlockId = BlockId(0);
}

/// Persistent id of the built-in air block.
pub const AIR_NAME: &str = "core:air";

/// Supported version of block definition files.
pub const BLOCK_FORMAT_VERSION: u32 = 1;

/// A block type definition, loaded from content files.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BlockDef {
    /// Namespaced persistent id, e.g. `core:stone`.
    pub id: String,
    pub display_name: String,
    /// Whether the block collides and occludes neighbouring faces.
    #[serde(default = "default_true")]
    pub solid: bool,
    /// sRGB base colour (0..=1 per channel); also tints the block's generated atlas tile.
    pub color: [f32; 3],
    /// Runtime-only stand-in for content unavailable in this world.
    #[serde(skip)]
    pub missing: bool,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct BlockFile {
    format_version: u32,
    #[serde(default)]
    blocks: Vec<BlockDef>,
}

#[derive(Debug, Error)]
pub enum RegistryError {
    #[error("invalid namespaced id `{0}` (expected `namespace:name` using [a-z0-9_])")]
    InvalidId(String),
    #[error("block id `{0}` exceeds the save format's 65535-byte limit")]
    IdTooLong(String),
    #[error("duplicate block id `{0}`")]
    Duplicate(String),
    #[error("`{AIR_NAME}` is built in and cannot be redefined")]
    AirRedefined,
    #[error(
        "block `{id}` is in namespace `{found}`, but this source may only define `{expected}:*`"
    )]
    WrongNamespace {
        id: String,
        found: String,
        expected: String,
    },
    #[error("unknown block `{0}`")]
    UnknownBlock(String),
    #[error("too many block types (max {})", u16::MAX)]
    Full,
    #[error("unsupported block file format_version {0} (supported: {BLOCK_FORMAT_VERSION})")]
    UnsupportedVersion(u32),
    #[error("block `{id}` has invalid colour component (must be within 0..=1)")]
    InvalidColor { id: String },
    #[error("failed to parse block definitions: {0}")]
    Parse(#[from] toml::de::Error),
}

/// Returns true for `[a-z0-9_]+`.
pub fn is_valid_name_segment(s: &str) -> bool {
    !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

/// Splits and validates a `namespace:name` identifier, returning the namespace.
pub fn parse_namespaced(id: &str) -> Result<&str, RegistryError> {
    match id.split_once(':') {
        Some((ns, name)) if is_valid_name_segment(ns) && is_valid_name_segment(name) => Ok(ns),
        _ => Err(RegistryError::InvalidId(id.to_owned())),
    }
}

/// Central block registry.
#[derive(Debug, Clone)]
pub struct BlockRegistry {
    defs: Vec<BlockDef>,
    by_name: HashMap<String, BlockId>,
}

impl Default for BlockRegistry {
    fn default() -> Self {
        let air = BlockDef {
            id: AIR_NAME.to_owned(),
            display_name: "Air".to_owned(),
            solid: false,
            color: [0.0; 3],
            missing: false,
        };
        Self {
            by_name: HashMap::from([(air.id.clone(), BlockId::AIR)]),
            defs: vec![air],
        }
    }
}

impl BlockRegistry {
    /// Registers a definition, validating its id. Returns the assigned runtime id.
    pub fn register(&mut self, def: BlockDef) -> Result<BlockId, RegistryError> {
        if def.id.len() > usize::from(u16::MAX) {
            return Err(RegistryError::IdTooLong(def.id));
        }
        parse_namespaced(&def.id)?;
        if def.id == AIR_NAME {
            return Err(RegistryError::AirRedefined);
        }
        if self.by_name.contains_key(&def.id) {
            return Err(RegistryError::Duplicate(def.id));
        }
        if def.color.iter().any(|c| !(0.0..=1.0).contains(c)) {
            return Err(RegistryError::InvalidColor { id: def.id });
        }
        let id = BlockId(u16::try_from(self.defs.len()).map_err(|_| RegistryError::Full)?);
        self.by_name.insert(def.id.clone(), id);
        self.defs.push(def);
        Ok(id)
    }

    /// Parses a TOML block file and registers every block. Every id must belong to
    /// `namespace`, so one content pack cannot silently define another's blocks.
    /// The registry is unchanged if any definition fails validation.
    pub fn load_toml(&mut self, namespace: &str, src: &str) -> Result<(), RegistryError> {
        let file: BlockFile = toml::from_str(src)?;
        if file.format_version != BLOCK_FORMAT_VERSION {
            return Err(RegistryError::UnsupportedVersion(file.format_version));
        }
        let mut staged = self.clone();
        for def in file.blocks {
            let ns = parse_namespaced(&def.id)?;
            if ns != namespace {
                return Err(RegistryError::WrongNamespace {
                    found: ns.to_owned(),
                    expected: namespace.to_owned(),
                    id: def.id,
                });
            }
            staged.register(def)?;
        }
        *self = staged;
        Ok(())
    }

    /// Registers a solid, magenta stand-in for content that a save references but no
    /// installed mod provides, so the block survives load/save round trips instead of
    /// being replaced by air. Returns the existing id if `name` is already registered.
    pub fn register_missing(&mut self, name: &str) -> Result<BlockId, RegistryError> {
        if let Some(id) = self.id(name) {
            return Ok(id);
        }
        self.register(BlockDef {
            id: name.to_owned(),
            display_name: format!("Missing: {name}"),
            solid: true,
            color: [1.0, 0.0, 1.0],
            missing: true,
        })
    }

    pub fn id(&self, name: &str) -> Option<BlockId> {
        self.by_name.get(name).copied()
    }

    /// Like [`Self::id`] but reports a missing block as an error.
    pub fn require(&self, name: &str) -> Result<BlockId, RegistryError> {
        self.id(name)
            .ok_or_else(|| RegistryError::UnknownBlock(name.to_owned()))
    }

    pub fn get(&self, id: BlockId) -> Option<&BlockDef> {
        self.defs.get(usize::from(id.0))
    }

    /// Persistent name of a runtime id. Unknown ids are reported as air.
    pub fn name(&self, id: BlockId) -> &str {
        self.get(id).map_or(AIR_NAME, |d| &d.id)
    }

    pub fn is_solid(&self, id: BlockId) -> bool {
        self.get(id).is_some_and(|d| d.solid)
    }

    /// All definitions in runtime-id order.
    pub fn iter(&self) -> impl Iterator<Item = (BlockId, &BlockDef)> {
        self.defs
            .iter()
            .enumerate()
            .map(|(i, d)| (BlockId(i as u16), d))
    }

    pub fn len(&self) -> usize {
        self.defs.len()
    }

    pub fn is_empty(&self) -> bool {
        false // air is always registered
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Registry with the core blocks used by the terrain generator.
    pub(crate) fn test_registry() -> BlockRegistry {
        let mut r = BlockRegistry::default();
        for name in ["grass", "dirt", "stone", "wood"] {
            r.register(BlockDef {
                id: format!("core:{name}"),
                display_name: name.to_owned(),
                solid: true,
                color: [0.5; 3],
                missing: false,
            })
            .unwrap();
        }
        r
    }

    const SRC: &str = r#"
        format_version = 1
        [[blocks]]
        id = "core:stone"
        display_name = "Stone"
        color = [0.5, 0.5, 0.5]
    "#;

    #[test]
    fn loads_and_resolves_blocks() {
        let mut r = BlockRegistry::default();
        r.load_toml("core", SRC).unwrap();
        let stone = r.id("core:stone").unwrap();
        assert_eq!(stone, BlockId(1));
        assert!(r.is_solid(stone));
        assert!(!r.is_solid(BlockId::AIR));
        assert_eq!(r.name(stone), "core:stone");
        assert!(!r.get(stone).unwrap().missing);
        assert!(!r.get(BlockId::AIR).unwrap().missing);
    }

    #[test]
    fn missing_flag_is_runtime_only() {
        let mut r = BlockRegistry::default();
        let id = r.register_missing("mod:removed").unwrap();
        assert!(r.get(id).unwrap().missing);
        assert!(
            r.load_toml("core", &format!("{SRC}\nmissing = true"))
                .is_err()
        );
        assert!(r.id("core:stone").is_none());
    }

    #[test]
    fn rejects_bad_definitions() {
        let mut r = BlockRegistry::default();
        r.load_toml("core", SRC).unwrap();
        assert!(matches!(
            r.load_toml("core", SRC),
            Err(RegistryError::Duplicate(_))
        ));
        assert!(matches!(
            r.load_toml("other", SRC),
            Err(RegistryError::WrongNamespace { .. })
        ));
        let bad = SRC.replace("core:stone", "Core:Stone");
        assert!(matches!(
            r.load_toml("core", &bad),
            Err(RegistryError::InvalidId(_))
        ));
        let air = SRC.replace("core:stone", "core:air");
        assert!(matches!(
            r.load_toml("core", &air),
            Err(RegistryError::AirRedefined)
        ));
        let v2 = SRC.replace("format_version = 1", "format_version = 2");
        assert!(matches!(
            r.load_toml("core", &v2),
            Err(RegistryError::UnsupportedVersion(2))
        ));
    }

    #[test]
    fn block_ids_fit_the_save_format() {
        let mut r = BlockRegistry::default();
        let name = format!("mod:{}", "a".repeat(usize::from(u16::MAX) - 4));
        let id = r.register_missing(&name).unwrap();
        assert_eq!(r.name(id), name);
        let too_long = format!("{name}a");
        assert!(matches!(
            r.register_missing(&too_long),
            Err(RegistryError::IdTooLong(_))
        ));
        assert_eq!(r.len(), 2);
        assert!(r.id(&too_long).is_none());
    }

    #[test]
    fn block_files_register_atomically() {
        for second in [
            SRC.to_owned(),
            SRC.replace("core:stone", "other:stone"),
            SRC.replace("core:stone", "core:bad").replace("0.5", "2.0"),
        ] {
            let mut r = test_registry();
            let before = r.len();
            let src = format!(
                "{}\n{}",
                SRC.replace("core:stone", "core:new"),
                second.replace("format_version = 1", "")
            );
            assert!(r.load_toml("core", &src).is_err());
            assert_eq!(r.len(), before);
            assert!(r.id("core:new").is_none());
            assert_eq!(r.id("core:stone"), Some(BlockId(3)));
        }
    }
}
