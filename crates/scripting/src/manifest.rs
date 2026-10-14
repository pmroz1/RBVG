//! `mod.toml` parsing and validation.

use std::path::{Component, Path, PathBuf};

use rbvg_voxel::registry::is_valid_name_segment;
use serde::Deserialize;
use thiserror::Error;

use crate::API_VERSION;

/// Privileges a mod must request explicitly in its manifest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize)]
pub enum Capability {
    /// Allows `world.set_block`.
    #[serde(rename = "world.write")]
    WorldWrite,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModManifest {
    /// Unique id; also the namespace for the mod's content ids.
    pub id: String,
    pub name: String,
    pub version: String,
    /// Script API version the mod was written against.
    pub api_version: u32,
    /// Script entry point, relative to the mod directory.
    pub entry: PathBuf,
    #[serde(default)]
    pub capabilities: Vec<Capability>,
    /// Directory the manifest was loaded from (not part of the file).
    #[serde(skip)]
    pub dir: PathBuf,
}

#[derive(Debug, Error)]
pub enum ManifestError {
    #[error("{path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("{path}: {source}")]
    Parse {
        path: PathBuf,
        source: toml::de::Error,
    },
    #[error("{path}: invalid mod id `{id}` (use [a-z0-9_])")]
    InvalidId { path: PathBuf, id: String },
    #[error("{path}: mod id `{id}` is reserved for the engine")]
    ReservedId { path: PathBuf, id: String },
    #[error("{path}: api_version {found} is not supported (engine provides {API_VERSION})")]
    ApiVersion { path: PathBuf, found: u32 },
    #[error("{path}: entry `{entry}` must be a relative `.luau` path inside the mod directory")]
    BadEntry { path: PathBuf, entry: PathBuf },
    #[error("mod id `{id}` is declared by both {first} and {second}")]
    Duplicate {
        id: String,
        first: PathBuf,
        second: PathBuf,
    },
}

impl ModManifest {
    /// Reads and validates `<dir>/mod.toml`.
    pub fn load(dir: &Path) -> Result<Self, ManifestError> {
        let path = dir.join("mod.toml");
        let src = std::fs::read_to_string(&path).map_err(|source| ManifestError::Io {
            path: path.clone(),
            source,
        })?;
        let mut m: ModManifest = toml::from_str(&src).map_err(|source| ManifestError::Parse {
            path: path.clone(),
            source,
        })?;
        m.dir = dir.to_owned();
        m.validate(&path)?;
        Ok(m)
    }

    fn validate(&self, path: &Path) -> Result<(), ManifestError> {
        let path = path.to_owned();
        if !is_valid_name_segment(&self.id) {
            return Err(ManifestError::InvalidId {
                path,
                id: self.id.clone(),
            });
        }
        if self.id == "core" {
            return Err(ManifestError::ReservedId {
                path,
                id: self.id.clone(),
            });
        }
        if self.api_version != API_VERSION {
            return Err(ManifestError::ApiVersion {
                path,
                found: self.api_version,
            });
        }
        let inside = self
            .entry
            .components()
            .all(|c| matches!(c, Component::Normal(_)));
        if !inside || self.entry.extension().is_none_or(|e| e != "luau") {
            return Err(ManifestError::BadEntry {
                path,
                entry: self.entry.clone(),
            });
        }
        Ok(())
    }

    pub fn has(&self, cap: Capability) -> bool {
        self.capabilities.contains(&cap)
    }

    /// Loads every `*/mod.toml` under `root`, sorted by id for a deterministic load order.
    /// A missing `root` yields no mods.
    pub fn discover(root: &Path) -> Result<Vec<Self>, ManifestError> {
        let io = |source| ManifestError::Io {
            path: root.to_owned(),
            source,
        };
        if !root.exists() {
            return Ok(Vec::new());
        }
        let mut mods = Vec::new();
        for entry in std::fs::read_dir(root).map_err(io)? {
            let dir = entry.map_err(io)?.path();
            if dir.join("mod.toml").is_file() {
                mods.push(Self::load(&dir)?);
            }
        }
        mods.sort_by(|a, b| a.id.cmp(&b.id));
        if let Some(w) = mods.windows(2).find(|w| w[0].id == w[1].id) {
            return Err(ManifestError::Duplicate {
                id: w[0].id.clone(),
                first: w[0].dir.clone(),
                second: w[1].dir.clone(),
            });
        }
        Ok(mods)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(src: &str) -> Result<ModManifest, ManifestError> {
        let m: ModManifest = toml::from_str(src).unwrap();
        m.validate(Path::new("mod.toml")).map(|_| m)
    }

    const OK: &str = r#"
        id = "demo"
        name = "Demo"
        version = "0.1.0"
        api_version = 1
        entry = "main.luau"
        capabilities = ["world.write"]
    "#;

    #[test]
    fn accepts_valid_manifest() {
        let m = parse(OK).unwrap();
        assert!(m.has(Capability::WorldWrite));
    }

    #[test]
    fn rejects_invalid_manifests() {
        assert!(matches!(
            parse(&OK.replace("\"demo\"", "\"Demo!\"")),
            Err(ManifestError::InvalidId { .. })
        ));
        assert!(matches!(
            parse(&OK.replace("\"demo\"", "\"core\"")),
            Err(ManifestError::ReservedId { .. })
        ));
        assert!(matches!(
            parse(&OK.replace("api_version = 1", "api_version = 99")),
            Err(ManifestError::ApiVersion { .. })
        ));
        for entry in ["../evil.luau", "/etc/x.luau", "main.lua"] {
            assert!(
                matches!(
                    parse(&OK.replace("main.luau", entry)),
                    Err(ManifestError::BadEntry { .. })
                ),
                "{entry}"
            );
        }
        assert!(toml::from_str::<ModManifest>(&OK.replace("world.write", "fs.read")).is_err());
    }
}
