//! Sandboxed Luau mod runtime.
//!
//! Each mod gets its own Luau VM (error and state isolation). Scripts talk to the engine
//! only through the API installed here: they receive [`ScriptEvent`]s and emit
//! [`ScriptCommand`]s, which the host validates and applies. They read the world through a
//! narrow [`WorldQuery`] that is only reachable while the host runs them, and schedule
//! callbacks on a simulation-time clock advanced by [`ScriptHost::tick`]. Scripts never
//! get references into engine memory. See `docs/SCRIPTING_API.md`.

mod events;
mod host;
mod manifest;
mod pump;
mod query;

pub use events::{Envelope, EventOrigin, ScriptCommand, ScriptEvent};
pub use host::{DispatchStats, MIN_REPEAT_INTERVAL, ScriptError, ScriptHost, ScriptHostConfig};
pub use manifest::{Capability, ManifestError, ModManifest, resolve_order};
pub use pump::{CommandApplier, PumpLimits, PumpStats, pump};
pub use query::{NoWorld, VoxelQuery, WorldQuery};

/// Version of the script API. Mods declare the version they target in `mod.toml`.
pub const API_VERSION: u32 = 1;
