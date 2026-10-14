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
    /// Allows `world.get_block` and `world.is_loaded`.
    #[serde(rename = "world.read")]
    WorldRead,
    /// Allows `ui.notify`.
    #[serde(rename = "ui.notify")]
    UiNotify,
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
    /// Mod ids that must be installed; they load before this mod.
    #[serde(default)]
    pub dependencies: Vec<String>,
    /// Mod ids that load before this mod if installed.
    #[serde(default)]
    pub optional_dependencies: Vec<String>,
    /// Block definition file (same format as `content/core/blocks.toml`), relative to the
    /// mod directory. Every block must be in the mod's own namespace.
    #[serde(default)]
    pub blocks: Option<PathBuf>,
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
    #[error("mod `{id}` requires missing mod `{missing}`")]
    MissingDependency { id: String, missing: String },
    #[error("mod dependency cycle: {0}")]
    Cycle(String),
    #[error(
        "{path}: `blocks` path `{blocks}` must be a relative `.toml` path inside the mod directory"
    )]
    BadBlocksPath { path: PathBuf, blocks: PathBuf },
    #[error("{path}: invalid dependency id `{id}`")]
    BadDependency { path: PathBuf, id: String },
    #[error("{path}: {source}")]
    Content {
        path: PathBuf,
        source: rbvg_voxel::registry::RegistryError,
    },
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
        let inside = |p: &Path| p.components().all(|c| matches!(c, Component::Normal(_)));
        if let Some(b) = &self.blocks
            && (!inside(b) || b.extension().is_none_or(|e| e != "toml"))
        {
            return Err(ManifestError::BadBlocksPath {
                path,
                blocks: b.clone(),
            });
        }
        for d in self.dependencies.iter().chain(&self.optional_dependencies) {
            if !is_valid_name_segment(d) || *d == self.id {
                return Err(ManifestError::BadDependency {
                    path,
                    id: d.clone(),
                });
            }
        }
        Ok(())
    }

    /// Registers this mod's blocks (if any) in `registry`.
    pub fn register_blocks(
        &self,
        registry: &mut rbvg_voxel::BlockRegistry,
    ) -> Result<(), ManifestError> {
        let Some(rel) = &self.blocks else {
            return Ok(());
        };
        let path = self.dir.join(rel);
        let src = std::fs::read_to_string(&path).map_err(|source| ManifestError::Io {
            path: path.clone(),
            source,
        })?;
        registry
            .load_toml(&self.id, &src)
            .map_err(|source| ManifestError::Content { path, source })
    }

    pub fn has(&self, cap: Capability) -> bool {
        self.capabilities.contains(&cap)
    }

    /// Loads every `*/mod.toml` under `root` in dependency order, smallest ready id first.
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
        resolve_order(mods)
    }
}

/// Orders mods so every dependency (and installed optional dependency) loads first.
/// Deterministic: among mods that are ready, the smallest id goes first.
pub fn resolve_order(mods: Vec<ModManifest>) -> Result<Vec<ModManifest>, ManifestError> {
    use std::collections::BTreeMap;
    let mut pending: BTreeMap<String, ModManifest> = BTreeMap::new();
    for m in mods {
        if let Some(first) = pending.get(&m.id) {
            return Err(ManifestError::Duplicate {
                id: m.id,
                first: first.dir.clone(),
                second: m.dir,
            });
        }
        pending.insert(m.id.clone(), m);
    }
    for m in pending.values() {
        if let Some(missing) = m.dependencies.iter().find(|d| !pending.contains_key(*d)) {
            return Err(ManifestError::MissingDependency {
                id: m.id.clone(),
                missing: missing.clone(),
            });
        }
    }
    let mut out: Vec<ModManifest> = Vec::with_capacity(pending.len());
    while !pending.is_empty() {
        let ready = pending.values().find(|m| {
            m.dependencies
                .iter()
                .chain(&m.optional_dependencies)
                .all(|d| !pending.contains_key(d))
        });
        let Some(id) = ready.map(|m| m.id.clone()) else {
            let ids: Vec<_> = pending.keys().cloned().collect();
            return Err(ManifestError::Cycle(ids.join(", ")));
        };
        out.extend(pending.remove(&id));
    }
    Ok(out)
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

    fn with_deps(id: &str, deps: &[&str]) -> ModManifest {
        let mut m = parse(OK).unwrap();
        m.id = id.into();
        m.dependencies = deps.iter().map(|d| (*d).into()).collect();
        m
    }

    #[test]
    fn dependencies_load_first_and_errors_are_reported() {
        let ids = |r: Result<Vec<ModManifest>, _>| -> Vec<String> {
            r.unwrap().into_iter().map(|m| m.id).collect()
        };
        let order = resolve_order(vec![
            with_deps("c", &["a"]),
            with_deps("b", &[]),
            with_deps("a", &["b"]),
        ]);
        assert_eq!(ids(order), ["b", "a", "c"]);
        assert!(matches!(
            resolve_order(vec![with_deps("a", &["nope"])]),
            Err(ManifestError::MissingDependency { .. })
        ));
        assert!(matches!(
            resolve_order(vec![with_deps("a", &["b"]), with_deps("b", &["a"])]),
            Err(ManifestError::Cycle(_))
        ));
    }

    #[test]
    fn accepts_valid_manifest() {
        let m = parse(OK).unwrap();
        assert!(m.has(Capability::WorldWrite));
    }

    #[test]
    fn resolver_rejects_duplicate_ids() {
        let mut first = with_deps("a", &[]);
        first.dir = "first".into();
        let mut second = first.clone();
        second.dir = "second".into();
        assert!(matches!(
            resolve_order(vec![first, second]),
            Err(ManifestError::Duplicate { id, first, second })
                if id == "a" && first == Path::new("first") && second == Path::new("second")
        ));
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
