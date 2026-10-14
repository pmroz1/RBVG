//! Luau VM management, API bindings, event dispatch and the callback scheduler.

use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use mlua::{Function, Lua, LuaOptions, MultiValue, StdLib, Table, Value, VmState};
use rbvg_voxel::BlockPos;
use rbvg_voxel::registry::parse_namespaced;
use thiserror::Error;

use crate::API_VERSION;
use crate::events::{ScriptCommand, ScriptEvent};
use crate::manifest::{Capability, ModManifest};
use crate::query::WorldQuery;

const HANDLERS_KEY: &str = "rbvg_handlers";
/// Registry table `id -> function` of pending scheduled callbacks.
const SCHEDULED_KEY: &str = "rbvg_scheduled";
/// Registry slot holding the scoped world-query function while the host runs scripts.
const QUERY_KEY: &str = "rbvg_world_query";
/// Shortest `scheduler.every` interval, so repeating callbacks cannot run every frame.
pub const MIN_REPEAT_INTERVAL: Duration = Duration::from_millis(50);

/// Resource limits applied to every mod.
#[derive(Debug, Clone)]
pub struct ScriptHostConfig {
    /// Wall-clock time one mod may spend loading, or handling one event.
    pub budget: Duration,
    /// Maximum Luau heap per mod, in bytes.
    pub memory_limit: usize,
    /// Maximum commands one mod may queue while handling one event or one callback.
    pub max_commands: usize,
    /// Maximum pending scheduled callbacks per mod.
    pub max_scheduled_per_mod: usize,
    /// Maximum scheduled callbacks run per [`ScriptHost::tick`], across all mods.
    pub max_callbacks_per_tick: usize,
}

impl Default for ScriptHostConfig {
    fn default() -> Self {
        Self {
            budget: Duration::from_millis(5),
            memory_limit: 16 * 1024 * 1024,
            max_commands: 256,
            max_scheduled_per_mod: 256,
            max_callbacks_per_tick: 64,
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

/// Timing/error summary of one [`ScriptHost::dispatch`] or [`ScriptHost::tick`].
#[derive(Debug, Default, Clone, Copy)]
pub struct DispatchStats {
    pub duration: Duration,
    pub errors: usize,
    /// Scheduled callbacks run (ticks only).
    pub callbacks: usize,
}

type Deadline = Arc<Mutex<Option<Instant>>>;
type CommandQueue = Arc<Mutex<Vec<ScriptCommand>>>;

/// One pending callback. Ordered by `(deadline, seq)`; `seq` is unique host-wide, so
/// equal deadlines run in scheduling order, across mods too.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct Scheduled {
    deadline: Duration,
    seq: u64,
    id: i64,
    interval: Option<Duration>,
}

#[derive(Default)]
struct ModSchedule {
    queue: BinaryHeap<Reverse<Scheduled>>,
    next_id: i64,
}

/// Simulation clock and scheduling sequence shared by all mods of one host.
#[derive(Default)]
struct Clock {
    now: Mutex<Duration>,
    seq: AtomicU64,
}

struct ModInstance {
    id: String,
    lua: Lua,
    deadline: Deadline,
    commands: CommandQueue,
    schedule: Arc<Mutex<ModSchedule>>,
}

impl ModInstance {
    /// Runs `f` with this mod's execution deadline armed and, if given, `world`
    /// reachable from `world.get_block`/`world.is_loaded`. The query function is scoped:
    /// it is destroyed when this returns, so scripts can never keep a world reference.
    fn run<R>(
        &self,
        budget: Duration,
        world: Option<&dyn WorldQuery>,
        f: impl FnOnce(&Lua) -> mlua::Result<R>,
    ) -> mlua::Result<R> {
        *lock(&self.deadline) = Some(Instant::now() + budget);
        let result = self.lua.scope(|scope| {
            if let Some(world) = world {
                let query = scope.create_function(move |_, (x, y, z): (i32, i32, i32)| {
                    Ok(world.block(BlockPos::new(x, y, z)))
                })?;
                self.lua.set_named_registry_value(QUERY_KEY, query)?;
            }
            f(&self.lua)
        });
        *lock(&self.deadline) = None;
        if let Err(e) = self.lua.set_named_registry_value(QUERY_KEY, Value::Nil) {
            tracing::error!(mod_id = %self.id, "failed to clear world query: {e}");
        }
        result
    }

    /// Head of this mod's queue if it is due at `now` and was scheduled before `seq_limit`.
    fn due(&self, now: Duration, seq_limit: u64) -> Option<Scheduled> {
        let s = lock(&self.schedule);
        let Reverse(head) = *s.queue.peek()?;
        (head.deadline <= now && head.seq < seq_limit).then_some(head)
    }
}

/// Owns all loaded mods. Scripts only run inside [`ScriptHost::load_mod`],
/// [`ScriptHost::dispatch`] and [`ScriptHost::tick`], so the engine controls exactly when
/// script code executes.
pub struct ScriptHost {
    config: ScriptHostConfig,
    mods: Vec<ModInstance>,
    clock: Arc<Clock>,
}

impl ScriptHost {
    pub fn new(config: ScriptHostConfig) -> Self {
        Self {
            config,
            mods: Vec::new(),
            clock: Arc::default(),
        }
    }

    /// Simulation time accumulated by [`Self::tick`].
    pub fn now(&self) -> Duration {
        *lock(&self.clock.now)
    }

    /// Pending scheduled callbacks across all mods.
    pub fn scheduled_count(&self) -> usize {
        self.mods
            .iter()
            .map(|m| lock(&m.schedule).queue.len())
            .sum()
    }

    /// Unloads a mod: its VM, handlers and pending callbacks are dropped. Returns false if
    /// no such mod is loaded.
    pub fn unload_mod(&mut self, id: &str) -> bool {
        let before = self.mods.len();
        self.mods.retain(|m| m.id != id);
        before != self.mods.len()
    }

    /// Discards every pending scheduled callback (all callbacks are world-scoped; call
    /// this when the world is replaced, e.g. on reload).
    pub fn clear_scheduled(&mut self) {
        for m in &self.mods {
            lock(&m.schedule).queue.clear();
            let reset = m
                .lua
                .create_table()
                .and_then(|t| m.lua.set_named_registry_value(SCHEDULED_KEY, t));
            if let Err(e) = reset {
                tracing::error!(mod_id = %m.id, "failed to clear scheduled callbacks: {e}");
            }
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
    pub fn load_mod_source(
        &mut self,
        manifest: &ModManifest,
        src: &str,
    ) -> Result<(), ScriptError> {
        let err = |source| ScriptError::Lua {
            mod_id: manifest.id.clone(),
            source,
        };
        if self.mods.iter().any(|m| m.id == manifest.id) {
            return Err(err(mlua::Error::runtime(
                "a mod with this id is already loaded",
            )));
        }
        let instance = create_instance(manifest, &self.config, &self.clock).map_err(err)?;
        let chunk_name = format!("@{}/{}", manifest.id, manifest.entry.display());
        instance
            .run(self.config.budget, None, |lua| {
                lua.load(src).set_name(chunk_name).exec()
            })
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
    /// dispatch or affects other mods. All handlers read the same `world` state: queued
    /// commands are applied by the engine only after this returns.
    pub fn dispatch(
        &self,
        event: &ScriptEvent,
        world: &dyn WorldQuery,
    ) -> (Vec<ScriptCommand>, DispatchStats) {
        let start = Instant::now();
        let mut errors = 0;
        let mut out = Vec::new();
        for m in &self.mods {
            let result = m.run(self.config.budget, Some(world), |lua| {
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
            callbacks: 0,
        };
        (out, stats)
    }

    /// Advances simulation time by `dt` and runs due callbacks in `(deadline, seq)`
    /// order across all mods, returning the commands they queued.
    ///
    /// - Callbacks scheduled during this tick (including zero-delay ones) wait for the
    ///   next tick, so a callback chain advances at most one step per tick.
    /// - At most `max_callbacks_per_tick` callbacks run; the rest stay queued in order.
    /// - Each mod gets one `budget` of wall-clock time per tick shared by its callbacks.
    ///   A callback exceeding the remainder is interrupted (logged); once a mod's budget
    ///   is spent, its remaining due callbacks wait for the next tick.
    pub fn tick(
        &mut self,
        dt: Duration,
        world: &dyn WorldQuery,
    ) -> (Vec<ScriptCommand>, DispatchStats) {
        let start = Instant::now();
        let now = {
            let mut now = lock(&self.clock.now);
            *now = now.saturating_add(dt);
            *now
        };
        let seq_limit = self.clock.seq.load(Ordering::Relaxed);
        let budget = self.config.budget;
        let mut spent = vec![Duration::ZERO; self.mods.len()];
        let mut stats = DispatchStats::default();
        let mut out = Vec::new();
        while stats.callbacks < self.config.max_callbacks_per_tick {
            let next = self
                .mods
                .iter()
                .enumerate()
                .filter(|(i, _)| spent[*i] < budget)
                .filter_map(|(i, m)| m.due(now, seq_limit).map(|s| (s, i)))
                .min();
            let Some((entry, i)) = next else { break };
            let m = &self.mods[i];
            lock(&m.schedule).queue.pop();
            stats.callbacks += 1;
            let started = Instant::now();
            let result = m.run(budget - spent[i], Some(world), |lua| {
                let callbacks: Table = lua.named_registry_value(SCHEDULED_KEY)?;
                let Some(f) = callbacks.get::<Option<Function>>(entry.id)? else {
                    return Ok(false); // cancelled
                };
                if entry.interval.is_none() {
                    callbacks.set(entry.id, Value::Nil)?; // release the reference
                }
                f.call::<()>(())?;
                // Repeating callbacks continue unless they cancelled themselves.
                Ok(entry.interval.is_some() && callbacks.contains_key(entry.id)?)
            });
            spent[i] += started.elapsed();
            match result {
                Ok(true) => {
                    let interval = entry.interval.unwrap_or_default();
                    let deadline = entry.deadline.saturating_add(interval).max(now);
                    lock(&m.schedule).queue.push(Reverse(Scheduled {
                        deadline,
                        seq: self.clock.seq.fetch_add(1, Ordering::Relaxed),
                        ..entry
                    }));
                }
                Ok(false) => {}
                Err(e) => {
                    stats.errors += 1;
                    tracing::error!(mod_id = %m.id, "scheduled callback error: {e}");
                    // A failing repeating callback is not rescheduled.
                    let dropped = m
                        .lua
                        .named_registry_value::<Table>(SCHEDULED_KEY)
                        .and_then(|t| t.set(entry.id, Value::Nil));
                    if let Err(e) = dropped {
                        tracing::error!(mod_id = %m.id, "failed to release callback: {e}");
                    }
                }
            }
            out.append(&mut lock(&m.commands));
        }
        stats.duration = start.elapsed();
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

/// Converts a Lua number to a block coordinate, rejecting fractions, NaN, infinities
/// and values outside the `i32` range.
fn coord(v: Value, axis: &str) -> mlua::Result<i32> {
    let n = match v {
        Value::Integer(i) => i32::try_from(i).ok(),
        Value::Number(n)
            if n.is_finite()
                && n.fract() == 0.0
                && (f64::from(i32::MIN)..=f64::from(i32::MAX)).contains(&n) =>
        {
            Some(n as i32)
        }
        _ => None,
    };
    n.ok_or_else(|| {
        mlua::Error::runtime(format!(
            "coordinate `{axis}` must be an integer in the 32-bit range"
        ))
    })
}

fn table_pos(t: &Table) -> mlua::Result<BlockPos> {
    Ok(BlockPos::new(
        coord(t.get("x")?, "x")?,
        coord(t.get("y")?, "y")?,
        coord(t.get("z")?, "z")?,
    ))
}

/// Accepts `(x, y, z)` or a single `{ x = .., y = .., z = .. }` table.
fn args_pos(args: MultiValue) -> mlua::Result<BlockPos> {
    let mut it = args.into_iter();
    match (it.next(), it.next(), it.next(), it.next()) {
        (Some(Value::Table(t)), None, None, None) => table_pos(&t),
        (Some(x), Some(y), Some(z), None) => Ok(BlockPos::new(
            coord(x, "x")?,
            coord(y, "y")?,
            coord(z, "z")?,
        )),
        _ => Err(mlua::Error::runtime(
            "expected (x, y, z) or a position table { x = .., y = .., z = .. }",
        )),
    }
}

/// Validates a delay in seconds (finite, >= 0) and turns it into a deadline.
fn deadline_after(
    clock: &Clock,
    seconds: f64,
    min: Duration,
) -> mlua::Result<(Duration, Duration)> {
    let delay = Duration::try_from_secs_f64(seconds)
        .ok()
        .filter(|d| *d >= min)
        .ok_or_else(|| {
            mlua::Error::runtime(format!(
                "delay must be a finite number of seconds >= {}",
                min.as_secs_f64()
            ))
        })?;
    let deadline = lock(&clock.now)
        .checked_add(delay)
        .ok_or_else(|| mlua::Error::runtime("delay is too large"))?;
    Ok((delay, deadline))
}

/// Builds a sandboxed VM with the engine API installed.
fn create_instance(
    manifest: &ModManifest,
    config: &ScriptHostConfig,
    clock: &Arc<Clock>,
) -> mlua::Result<ModInstance> {
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
    let can_read = manifest.has(Capability::WorldRead);
    let query = move |lua: &Lua, args: MultiValue, api: &str| -> mlua::Result<Option<String>> {
        if !can_read {
            return Err(mlua::Error::runtime(format!(
                "world.{api} requires the `world.read` capability in mod.toml"
            )));
        }
        let p = args_pos(args)?;
        let Some(f) = lua.named_registry_value::<Option<Function>>(QUERY_KEY)? else {
            return Err(mlua::Error::runtime(format!(
                "world.{api} is only available in event handlers and scheduled callbacks"
            )));
        };
        f.call((p.x, p.y, p.z))
    };
    world.set(
        "get_block",
        lua.create_function(move |lua, args| query(lua, args, "get_block"))?,
    )?;
    world.set(
        "is_loaded",
        lua.create_function(move |lua, args| Ok(query(lua, args, "is_loaded")?.is_some()))?,
    )?;
    globals.set("world", world)?;

    let schedule = Arc::new(Mutex::new(ModSchedule::default()));
    lua.set_named_registry_value(SCHEDULED_KEY, lua.create_table()?)?;
    let scheduler = lua.create_table()?;
    let max = config.max_scheduled_per_mod;
    for (name, min, repeat) in [
        ("after", Duration::ZERO, false),
        ("every", MIN_REPEAT_INTERVAL, true),
    ] {
        let (schedule, clock) = (schedule.clone(), clock.clone());
        let f = lua.create_function(move |lua, (seconds, f): (f64, Function)| {
            let (delay, deadline) = deadline_after(&clock, seconds, min)
                .map_err(|e| mlua::Error::runtime(format!("scheduler.{name}: {e}")))?;
            let mut s = lock(&schedule);
            if s.queue.len() >= max {
                return Err(mlua::Error::runtime(format!(
                    "scheduler.{name}: too many pending callbacks (max {max})"
                )));
            }
            s.next_id += 1;
            let id = s.next_id;
            let callbacks: Table = lua.named_registry_value(SCHEDULED_KEY)?;
            callbacks.set(id, f)?;
            s.queue.push(Reverse(Scheduled {
                deadline,
                seq: clock.seq.fetch_add(1, Ordering::Relaxed),
                id,
                interval: repeat.then_some(delay),
            }));
            Ok(id)
        })?;
        scheduler.set(name, f)?;
    }
    let s = schedule.clone();
    scheduler.set(
        "cancel",
        lua.create_function(move |lua, id: i64| {
            let callbacks: Table = lua.named_registry_value(SCHEDULED_KEY)?;
            if !callbacks.contains_key(id)? {
                return Ok(false);
            }
            callbacks.set(id, Value::Nil)?;
            lock(&s).queue.retain(|Reverse(e)| e.id != id);
            Ok(true)
        })?,
    )?;
    globals.set("scheduler", scheduler)?;

    // Freeze the API and standard library; script globals become per-VM locals.
    lua.sandbox(true)?;
    Ok(ModInstance {
        id: manifest.id.clone(),
        lua,
        deadline,
        commands,
        schedule,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::query::NoWorld;

    /// Chunks with y >= 0 are loaded: stone below y = 10, air above.
    struct TestWorld;
    impl WorldQuery for TestWorld {
        fn block(&self, p: BlockPos) -> Option<&str> {
            (p.y >= 0).then_some(if p.y < 10 { "core:stone" } else { "core:air" })
        }
    }

    const SECOND: Duration = Duration::from_secs(1);

    fn xs(cmds: &[ScriptCommand]) -> Vec<i32> {
        cmds.iter()
            .map(|ScriptCommand::SetBlock { pos, .. }| pos.x)
            .collect()
    }

    /// Mod whose entry script defines `mark(x)` (queues a set_block at x) and runs `src`.
    fn load(host: &mut ScriptHost, id: &str, src: &str) {
        let prelude =
            "local function mark(x) world.set_block({ x = x, y = 0, z = 0 }, \"core:stone\") end\n";
        host.load_mod_source(
            &manifest(id, vec![Capability::WorldWrite, Capability::WorldRead]),
            &format!("{prelude}{src}"),
        )
        .unwrap();
    }

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
        let (cmds, stats) = host.dispatch(&broken(), &NoWorld);
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
        assert!(host.dispatch(&placed, &NoWorld).0.is_empty());
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
        let (cmds, stats) = host.dispatch(&broken(), &NoWorld);
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
        let (cmds, stats) = host.dispatch(&broken(), &NoWorld);
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
            .load_mod_source(
                &manifest("typo", vec![]),
                r#"events.subscribe("blok_broken", print)"#,
            )
            .unwrap_err();
        assert!(err.to_string().contains("unknown event"), "{err}");
        assert!(
            err.to_string().contains("typo/main.luau:1"),
            "line info: {err}"
        );
    }

    #[test]
    fn scripts_read_blocks_and_unloaded_reads_are_nil() {
        let mut host = ScriptHost::new(ScriptHostConfig::default());
        load(
            &mut host,
            "reader",
            r#"
            events.subscribe("block_broken", function(e)
                assert(world.get_block(1, 5, 1) == "core:stone")
                assert(world.get_block({ x = 1, y = 20, z = 1 }) == "core:air")
                assert(world.get_block(e.position) == "core:stone")
            end)
            events.subscribe("block_placed", function()
                assert(world.get_block(1, -5, 1) == nil)
                assert(world.is_loaded(1, 5, 1) == true)
                assert(world.is_loaded(1, -5, 1) == false)
            end)
        "#,
        );
        assert_eq!(host.dispatch(&broken(), &TestWorld).1.errors, 0);
        let placed = ScriptEvent::BlockPlaced {
            pos: BlockPos::new(0, 0, 0),
            block: "core:dirt".into(),
        };
        assert_eq!(host.dispatch(&placed, &TestWorld).1.errors, 0);
    }

    #[test]
    fn reads_need_capability_and_a_running_engine_context() {
        let mut host = ScriptHost::new(ScriptHostConfig::default());
        host.load_mod_source(
            &manifest("noread", vec![]),
            r#"events.subscribe("block_broken", function() world.get_block(0, 0, 0) end)"#,
        )
        .unwrap();
        let (_, stats) = host.dispatch(&broken(), &TestWorld);
        assert_eq!(stats.errors, 1, "world.read missing");
        // During loading there is no world to read.
        load(
            &mut host,
            "early",
            r#"
            local ok, err = pcall(world.get_block, 0, 0, 0)
            assert(not ok and string.find(tostring(err), "only available"), tostring(err))
        "#,
        );
    }

    #[test]
    fn invalid_coordinates_are_rejected() {
        let mut host = ScriptHost::new(ScriptHostConfig::default());
        load(
            &mut host,
            "coords",
            r#"
            events.subscribe("block_broken", function()
                for _, args in {
                    { 1.5, 0, 0 }, { 0 / 0, 0, 0 }, { math.huge, 0, 0 }, { 2 ^ 40, 0, 0 },
                    { "a", 0, 0 }, { 0, 0 }, { 0, 0, 0, 0 }, { { x = 1, y = 2 } },
                } do
                    assert(not pcall(world.get_block, table.unpack(args)), tostring(args[1]))
                    assert(not pcall(world.is_loaded, table.unpack(args)))
                end
                assert(not pcall(world.set_block, { x = 0.5, y = 0, z = 0 }, "core:stone"))
                assert(world.get_block(-2147483648, 2147483647, 0) ~= nil)
            end)
        "#,
        );
        let (cmds, stats) = host.dispatch(&broken(), &TestWorld);
        assert_eq!(stats.errors, 0);
        assert!(cmds.is_empty());
    }

    #[test]
    fn callbacks_run_after_the_simulation_delay_with_deterministic_order() {
        let mut host = ScriptHost::new(ScriptHostConfig::default());
        load(
            &mut host,
            "a",
            r#"
            scheduler.after(2.0, function() mark(3) end)
            scheduler.after(1.0, function() mark(1) end)
            scheduler.after(1.0, function() mark(2) end)
        "#,
        );
        load(
            &mut host,
            "b",
            "scheduler.after(1.0, function() mark(10) end)",
        );
        assert_eq!(host.scheduled_count(), 4);
        assert!(host.tick(SECOND / 2, &NoWorld).0.is_empty());
        let (cmds, stats) = host.tick(SECOND / 2, &NoWorld);
        // Equal deadlines run in scheduling order, across mods.
        assert_eq!(xs(&cmds), vec![1, 2, 10]);
        assert_eq!(stats.callbacks, 3);
        assert!(host.tick(SECOND * 99 / 100, &NoWorld).0.is_empty());
        assert_eq!(xs(&host.tick(SECOND / 100, &NoWorld).0), vec![3]);
        assert_eq!(host.scheduled_count(), 0);
        // Paused simulation (dt = 0) runs nothing new.
        assert!(host.tick(Duration::ZERO, &NoWorld).0.is_empty());
    }

    #[test]
    fn zero_delay_and_nested_callbacks_run_on_later_ticks() {
        let mut host = ScriptHost::new(ScriptHostConfig::default());
        load(
            &mut host,
            "chain",
            r#"
            scheduler.after(0, function()
                mark(1)
                scheduler.after(0, function() mark(2) end)
            end)
        "#,
        );
        assert_eq!(xs(&host.tick(Duration::ZERO, &NoWorld).0), vec![1]);
        assert_eq!(xs(&host.tick(Duration::ZERO, &NoWorld).0), vec![2]);
    }

    #[test]
    fn invalid_durations_and_unbounded_scheduling_are_rejected() {
        // Generous budget: this test checks limits, not timing, and must not flake on slow CI.
        let mut host = ScriptHost::new(ScriptHostConfig {
            budget: Duration::from_secs(1),
            ..ScriptHostConfig::default()
        });
        load(
            &mut host,
            "limits",
            r#"
            for _, d in { -1, 0 / 0, math.huge, -math.huge, "soon" } do
                assert(not pcall(scheduler.after, d, print), tostring(d))
            end
            assert(not pcall(scheduler.every, 0.01, print), "too frequent")
            assert(not pcall(scheduler.after, 1), "missing function")
            local ok = 0
            for i = 1, 1000 do
                if pcall(scheduler.after, 1, print) then ok += 1 end
            end
            assert(ok == 256, ok)
        "#,
        );
        assert_eq!(host.scheduled_count(), 256);
    }

    #[test]
    fn per_tick_callback_cap_defers_the_rest_in_order() {
        let config = ScriptHostConfig {
            max_callbacks_per_tick: 4,
            ..ScriptHostConfig::default()
        };
        let mut host = ScriptHost::new(config);
        load(
            &mut host,
            "many",
            "for i = 1, 6 do scheduler.after(0, function() mark(i) end) end",
        );
        assert_eq!(xs(&host.tick(SECOND, &NoWorld).0), vec![1, 2, 3, 4]);
        assert_eq!(xs(&host.tick(SECOND, &NoWorld).0), vec![5, 6]);
    }

    #[test]
    fn callbacks_stay_within_budgets_and_errors_are_isolated() {
        let mut host = ScriptHost::new(ScriptHostConfig::default());
        load(
            &mut host,
            "a_bad",
            r#"
            scheduler.after(1, function() while true do end end)
            scheduler.after(1, function() error("boom") end)
            scheduler.after(1, function() for i = 1, 1000 do mark(i) end end)
        "#,
        );
        load(
            &mut host,
            "b_good",
            "scheduler.after(1, function() mark(7) end)",
        );
        let start = Instant::now();
        let (cmds, stats) = host.tick(SECOND, &NoWorld);
        assert!(start.elapsed() < SECOND);
        // The loop used up a_bad's budget, so its other callbacks wait for a later tick.
        assert_eq!(xs(&cmds), vec![7]);
        assert_eq!(stats.errors, 1);
        let (cmds, stats) = host.tick(Duration::ZERO, &NoWorld);
        // `boom` fails; the command flood is stopped at max_commands and fails too.
        assert_eq!(stats.errors, 2);
        assert_eq!(cmds.len(), 256);
        assert_eq!(host.scheduled_count(), 0);
        assert_eq!(host.tick(SECOND, &NoWorld).1.errors, 0);
    }

    #[test]
    fn unloading_a_mod_or_world_drops_its_callbacks() {
        let mut host = ScriptHost::new(ScriptHostConfig::default());
        load(
            &mut host,
            "gone",
            "scheduler.after(1, function() mark(1) end)",
        );
        load(
            &mut host,
            "kept",
            "scheduler.after(1, function() mark(2) end)",
        );
        assert!(host.unload_mod("gone"));
        assert!(!host.unload_mod("gone"));
        assert_eq!(host.scheduled_count(), 1);
        assert_eq!(xs(&host.tick(SECOND, &NoWorld).0), vec![2]);

        load(
            &mut host,
            "reload",
            "scheduler.every(1, function() mark(3) end)",
        );
        host.clear_scheduled(); // world reload
        assert_eq!(host.scheduled_count(), 0);
        assert!(host.tick(SECOND * 5, &NoWorld).0.is_empty());
    }

    #[test]
    fn every_repeats_until_cancelled() {
        let mut host = ScriptHost::new(ScriptHostConfig::default());
        load(
            &mut host,
            "repeat",
            r#"
            local n = 0
            local handle
            handle = scheduler.every(0.5, function()
                n += 1
                mark(n)
                if n == 3 then assert(scheduler.cancel(handle)) end
            end)
            local other = scheduler.after(1, function() mark(100) end)
            assert(scheduler.cancel(other))
            assert(not scheduler.cancel(other))
        "#,
        );
        let mut all = Vec::new();
        for _ in 0..10 {
            all.extend(xs(&host.tick(SECOND / 2, &NoWorld).0));
        }
        assert_eq!(all, vec![1, 2, 3]);
        assert_eq!(host.scheduled_count(), 0);
    }
}
