//! Luau VM management, API bindings and event dispatch.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use mlua::{Function, Lua, LuaOptions, StdLib, Table, Value, VmState};
use rbvg_voxel::BlockPos;
use rbvg_voxel::registry::parse_namespaced;
use thiserror::Error;

use crate::events::{ScriptCommand, ScriptEvent};
use crate::manifest::{Capability, ModManifest};
use crate::API_VERSION;

const HANDLERS_KEY: &str = "rbvg_handlers";

/// Resource limits applied to every mod.
#[derive(Debug, Clone)]
pub struct ScriptHostConfig {
    /// Wall-clock time one mod may spend loading, or handling one event.
    pub budget: Duration,
    /// Maximum Luau heap per mod, in bytes.
    pub memory_limit: usize,
    /// Maximum commands one mod may queue while handling one event.
    pub max_commands: usize,
}

impl Default for ScriptHostConfig {
    fn default() -> Self {
        Self {
            budget: Duration::from_millis(5),
            memory_limit: 16 * 1024 * 1024,
            max_commands: 256,
        }
    }
}

#[derive(Debug, Error)]
pub enum ScriptError {
    #[error("mod `{mod_id}`: cannot read {path}: {source}")]
    Io {
        mod_id: String,
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("mod `{mod_id}` failed to load: {source}")]
    Lua { mod_id: String, source: mlua::Error },
}

/// Timing/error summary of one [`ScriptHost::dispatch`].
#[derive(Debug, Default, Clone, Copy)]
pub struct DispatchStats {
    pub duration: Duration,
    pub errors: usize,
}

type Deadline = Arc<Mutex<Option<Instant>>>;
type CommandQueue = Arc<Mutex<Vec<ScriptCommand>>>;

struct ModInstance {
    id: String,
    lua: Lua,
    deadline: Deadline,
    commands: CommandQueue,
}

impl ModInstance {
    /// Runs `f` with this mod's execution deadline armed.
    fn run<R>(&self, budget: Duration, f: impl FnOnce(&Lua) -> mlua::Result<R>) -> mlua::Result<R> {
        *lock(&self.deadline) = Some(Instant::now() + budget);
        let result = f(&self.lua);
        *lock(&self.deadline) = None;
        result
    }
}

/// Owns all loaded mods. Scripts only run inside [`ScriptHost::load_mod`] and
/// [`ScriptHost::dispatch`], so the engine controls exactly when script code executes.
pub struct ScriptHost {
    config: ScriptHostConfig,
    mods: Vec<ModInstance>,
}

impl ScriptHost {
    pub fn new(config: ScriptHostConfig) -> Self {
        Self {
            config,
            mods: Vec::new(),
        }
    }

    pub fn mod_ids(&self) -> impl Iterator<Item = &str> {
        self.mods.iter().map(|m| m.id.as_str())
    }

    /// Reads the manifest's entry script and loads it.
    pub fn load_mod(&mut self, manifest: &ModManifest) -> Result<(), ScriptError> {
        let path = manifest.dir.join(&manifest.entry);
        let src = std::fs::read_to_string(&path).map_err(|source| ScriptError::Io {
            mod_id: manifest.id.clone(),
            path,
            source,
        })?;
        self.load_mod_source(manifest, &src)
    }

    /// Creates an isolated VM for the mod and runs its entry script (within budget).
    pub fn load_mod_source(&mut self, manifest: &ModManifest, src: &str) -> Result<(), ScriptError> {
        let err = |source| ScriptError::Lua {
            mod_id: manifest.id.clone(),
            source,
        };
        let instance = create_instance(manifest, &self.config).map_err(err)?;
        let chunk_name = format!("@{}/{}", manifest.id, manifest.entry.display());
        instance
            .run(self.config.budget, |lua| lua.load(src).set_name(chunk_name).exec())
            .map_err(err)?;
        let dropped = std::mem::take(&mut *lock(&instance.commands)).len();
        if dropped > 0 {
            tracing::warn!(mod_id = %manifest.id, dropped, "world commands issued while loading are ignored; issue them from event handlers");
        }
        tracing::info!(mod_id = %manifest.id, version = %manifest.version, "loaded mod");
        self.mods.push(instance);
        Ok(())
    }

    /// Delivers an event to every subscribed handler, in mod load order, and returns the
    /// commands they queued. A failing handler is logged and skipped; it never aborts the
    /// dispatch or affects other mods.
    pub fn dispatch(&self, event: &ScriptEvent) -> (Vec<ScriptCommand>, DispatchStats) {
        let start = Instant::now();
        let mut errors = 0;
        let mut out = Vec::new();
        for m in &self.mods {
            let result = m.run(self.config.budget, |lua| {
                let handlers: Table = lua.named_registry_value(HANDLERS_KEY)?;
                let Some(list) = handlers.get::<Option<Table>>(event.name())? else {
                    return Ok(0);
                };
                let payload = event_payload(lua, event)?;
                let mut failed = 0;
                for f in list.sequence_values::<Function>() {
                    if let Err(e) = f.and_then(|f| f.call::<()>(&payload)) {
                        failed += 1;
                        tracing::error!(mod_id = %m.id, event = event.name(), "script error: {e}");
                    }
                }
                Ok(failed)
            });
            match result {
                Ok(n) => errors += n,
                Err(e) => {
                    errors += 1;
                    tracing::error!(mod_id = %m.id, event = event.name(), "script error: {e}");
                }
            }
            out.append(&mut lock(&m.commands));
        }
        let stats = DispatchStats {
            duration: start.elapsed(),
            errors,
        };
        (out, stats)
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

fn event_payload(lua: &Lua, event: &ScriptEvent) -> mlua::Result<Table> {
    let t = lua.create_table()?;
    match event {
        ScriptEvent::BlockBroken { pos, block } | ScriptEvent::BlockPlaced { pos, block } => {
            t.set("position", pos_table(lua, *pos)?)?;
            t.set("block", block.as_str())?;
        }
    }
    Ok(t)
}

fn pos_table(lua: &Lua, p: BlockPos) -> mlua::Result<Table> {
    let t = lua.create_table()?;
    t.set("x", p.x)?;
    t.set("y", p.y)?;
    t.set("z", p.z)?;
    Ok(t)
}

fn table_pos(t: &Table) -> mlua::Result<BlockPos> {
    Ok(BlockPos::new(t.get("x")?, t.get("y")?, t.get("z")?))
}

/// Builds a sandboxed VM with the engine API installed.
fn create_instance(manifest: &ModManifest, config: &ScriptHostConfig) -> mlua::Result<ModInstance> {
    let lua = Lua::new_with(StdLib::ALL_SAFE, LuaOptions::default())?;
    lua.set_memory_limit(config.memory_limit)?;
    let globals = lua.globals();
    // mlua installs a filesystem-backed `require` for Luau; mods must not touch the disk.
    globals.raw_set("require", Value::Nil)?;

    let deadline = Deadline::default();
    let dl = deadline.clone();
    lua.set_interrupt(move |_| {
        if lock(&dl).is_some_and(|d| Instant::now() >= d) {
            return Err(mlua::Error::runtime("script exceeded its execution budget"));
        }
        Ok(VmState::Continue)
    });

    let id = manifest.id.clone();
    let rbvg = lua.create_table()?;
    rbvg.set("api_version", API_VERSION)?;
    rbvg.set("mod_id", id.as_str())?;
    globals.set("rbvg", rbvg)?;

    let log = lua.create_table()?;
    for level in ["info", "warn", "error"] {
        let id = id.clone();
        let f = lua.create_function(move |lua, msg: String| {
            let line = lua.inspect_stack(1, |d| d.current_line()).flatten();
            let line = line.map_or_else(|| "?".to_owned(), |l| l.to_string());
            match level {
                "info" => tracing::info!(target: "rbvg::script", mod_id = %id, line, "{msg}"),
                "warn" => tracing::warn!(target: "rbvg::script", mod_id = %id, line, "{msg}"),
                _ => tracing::error!(target: "rbvg::script", mod_id = %id, line, "{msg}"),
            }
            Ok(())
        })?;
        log.set(level, f)?;
    }
    globals.set("log", log)?;

    lua.set_named_registry_value(HANDLERS_KEY, lua.create_table()?)?;
    let events = lua.create_table()?;
    events.set(
        "subscribe",
        lua.create_function(|lua, (name, f): (String, Function)| {
            if !ScriptEvent::NAMES.contains(&name.as_str()) {
                return Err(mlua::Error::runtime(format!(
                    "unknown event `{name}` (known: {})",
                    ScriptEvent::NAMES.join(", ")
                )));
            }
            let handlers: Table = lua.named_registry_value(HANDLERS_KEY)?;
            let list = match handlers.get::<Option<Table>>(name.as_str())? {
                Some(l) => l,
                None => {
                    let l = lua.create_table()?;
                    handlers.set(name, &l)?;
                    l
                }
            };
            list.push(f)
        })?,
    )?;
    globals.set("events", events)?;

    let commands = CommandQueue::default();
    let world = lua.create_table()?;
    let (q, can_write, max) = (
        commands.clone(),
        manifest.has(Capability::WorldWrite),
        config.max_commands,
    );
    world.set(
        "set_block",
        lua.create_function(move |_, (pos, block): (Table, String)| {
            if !can_write {
                return Err(mlua::Error::runtime(
                    "world.set_block requires the `world.write` capability in mod.toml",
                ));
            }
            parse_namespaced(&block).map_err(mlua::Error::external)?;
            let mut q = lock(&q);
            if q.len() >= max {
                return Err(mlua::Error::runtime(format!(
                    "too many world commands in one event (max {max})"
                )));
            }
            q.push(ScriptCommand::SetBlock {
                mod_id: id.clone(),
                pos: table_pos(&pos)?,
                block,
            });
            Ok(())
        })?,
    )?;
    globals.set("world", world)?;

    // Freeze the API and standard library; script globals become per-VM locals.
    lua.sandbox(true)?;
    Ok(ModInstance {
        id: manifest.id.clone(),
        lua,
        deadline,
        commands,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest(id: &str, caps: Vec<Capability>) -> ModManifest {
        ModManifest {
            id: id.into(),
            name: id.into(),
            version: "0.1.0".into(),
            api_version: API_VERSION,
            entry: "main.luau".into(),
            capabilities: caps,
            dir: PathBuf::new(),
        }
    }

    fn broken() -> ScriptEvent {
        ScriptEvent::BlockBroken {
            pos: BlockPos::new(-3, 7, 12),
            block: "core:stone".into(),
        }
    }

    #[test]
    fn event_reaches_script_and_command_comes_back() {
        let mut host = ScriptHost::new(ScriptHostConfig::default());
        let src = r#"
            events.subscribe("block_broken", function(e)
                log.info("broke " .. e.block)
                world.set_block({ x = e.position.x, y = e.position.y + 1, z = e.position.z }, e.block)
            end)
        "#;
        host.load_mod_source(&manifest("demo", vec![Capability::WorldWrite]), src)
            .unwrap();
        let (cmds, stats) = host.dispatch(&broken());
        assert_eq!(stats.errors, 0);
        assert_eq!(
            cmds,
            vec![ScriptCommand::SetBlock {
                mod_id: "demo".into(),
                pos: BlockPos::new(-3, 8, 12),
                block: "core:stone".into(),
            }]
        );
        // Unsubscribed events are a no-op.
        let placed = ScriptEvent::BlockPlaced {
            pos: BlockPos::new(0, 0, 0),
            block: "core:dirt".into(),
        };
        assert!(host.dispatch(&placed).0.is_empty());
    }

    #[test]
    fn runaway_script_is_interrupted_and_others_still_run() {
        let mut host = ScriptHost::new(ScriptHostConfig::default());
        host.load_mod_source(
            &manifest("a_loop", vec![]),
            r#"events.subscribe("block_broken", function() while true do end end)"#,
        )
        .unwrap();
        host.load_mod_source(
            &manifest("b_ok", vec![Capability::WorldWrite]),
            r#"events.subscribe("block_broken", function(e) world.set_block(e.position, "core:dirt") end)"#,
        )
        .unwrap();
        let start = Instant::now();
        let (cmds, stats) = host.dispatch(&broken());
        assert!(start.elapsed() < Duration::from_secs(1));
        assert_eq!(stats.errors, 1);
        assert_eq!(cmds.len(), 1);

        let err = host
            .load_mod_source(&manifest("c_load_loop", vec![]), "while true do end")
            .unwrap_err();
        assert!(err.to_string().contains("execution budget"), "{err}");
    }

    #[test]
    fn capabilities_and_ids_are_enforced() {
        let mut host = ScriptHost::new(ScriptHostConfig::default());
        host.load_mod_source(
            &manifest("nocap", vec![]),
            r#"events.subscribe("block_broken", function(e) world.set_block(e.position, "core:dirt") end)"#,
        )
        .unwrap();
        host.load_mod_source(
            &manifest("badid", vec![Capability::WorldWrite]),
            r#"events.subscribe("block_broken", function(e) world.set_block(e.position, "NOT VALID") end)"#,
        )
        .unwrap();
        let (cmds, stats) = host.dispatch(&broken());
        assert!(cmds.is_empty());
        assert_eq!(stats.errors, 2);
    }

    #[test]
    fn sandbox_hides_host_access_and_freezes_api() {
        let mut host = ScriptHost::new(ScriptHostConfig::default());
        host.load_mod_source(
            &manifest("sandbox", vec![]),
            r#"
                assert(require == nil, "require")
                assert(io == nil, "io")
                assert(os == nil or os.execute == nil, "os.execute")
                assert(not pcall(function() world.set_block = nil end), "api is read-only")
            "#,
        )
        .unwrap();
        let err = host
            .load_mod_source(&manifest("typo", vec![]), r#"events.subscribe("blok_broken", print)"#)
            .unwrap_err();
        assert!(err.to_string().contains("unknown event"), "{err}");
        assert!(err.to_string().contains("typo/main.luau:1"), "line info: {err}");
    }
}
