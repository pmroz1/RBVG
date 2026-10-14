//! Sandboxed Luau mod runtime.
//!
//! Each mod gets its own Luau VM (error and state isolation). Scripts talk to the engine
//! only through the API installed here: they receive [`ScriptEvent`]s and emit
//! [`ScriptCommand`]s, which the host validates and applies. Scripts never get references
//! into engine memory. See `docs/SCRIPTING_API.md`.

mod events;
mod host;
mod manifest;

pub use events::{ScriptCommand, ScriptEvent};
pub use host::{DispatchStats, ScriptError, ScriptHost, ScriptHostConfig};
pub use manifest::{Capability, ManifestError, ModManifest};

/// Version of the script API. Mods declare the version they target in `mod.toml`.
pub const API_VERSION: u32 = 1;
