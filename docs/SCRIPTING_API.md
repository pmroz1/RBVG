# Scripting API (v1)

Phase 2 additions (`world.read`, `world.get_block`, `world.is_loaded`, `scheduler.*`) are
additive, so `API_VERSION` stays 1. `world.set_block` now uses the same explicit
coordinate validation as the read functions (finite integers in the 32-bit range).

Mods are written in [Luau](https://luau.org). API version: **1**
(`rbvg_scripting::API_VERSION`). Breaking changes bump the version; mods declaring a
different `api_version` are rejected at load time with a clear error.

## Mod package

```
mods/<mod_id>/
  mod.toml
  main.luau
```

```toml
id = "example_logger"     # [a-z0-9_]+, unique, also the content namespace; "core" is reserved
name = "Example Logger"
version = "0.1.0"
api_version = 1
entry = "main.luau"       # relative .luau path inside the mod directory
capabilities = ["world.write"]   # optional; privileges must be requested explicitly
```

| Capability | Grants |
|---|---|
| `world.write` | `world.set_block` |
| `world.read` | `world.get_block`, `world.is_loaded` |

Mods are discovered in `mods/*/mod.toml` and loaded in id order. A mod that fails to load
is reported and skipped; the game still starts. Unknown manifest keys or capabilities are
errors.

## Execution model

- The entry script runs once at load time. It should register event handlers; world
  commands issued during load are ignored (with a warning).
- Handlers run only when the engine dispatches an event or a scheduled callback becomes
  due. There is no per-frame script callback.
- Every mod has its own Luau VM: globals, state and errors never leak between mods.
- Script globals are per-mod; the standard library and engine API tables are read-only.
- **Budgets:** loading and each event dispatch get 5 ms of wall-clock time per mod; all
  scheduled callbacks of one mod in one scheduler tick share 5 ms; a VM may use 16 MiB of
  heap; a handler or callback may queue 256 commands. Exceeding a limit raises a Luau
  error in that handler/callback. A mod may have at most 256 pending scheduled callbacks;
  at most 64 callbacks run per tick across all mods.
- **Errors:** a failing handler is logged as `mod_id` + `@<mod_id>/<file>:<line>: message`
  and skipped. Other handlers and mods still run. The game never crashes due to a script.

### When script changes become visible

`world.*` mutation functions do not change the world directly; they queue commands.
After all mods have handled an event, the engine validates each command (block id
registered, chunk loaded) and applies it, in the same frame, in mod load order. Invalid
commands are logged and dropped (a solid block is also refused where it would overlap
the player). Scheduled callbacks follow the same rule: their commands are applied after
the scheduler tick that ran them.

Changes made by scripts do **not** raise further script events, which prevents feedback
loops. *Limitation:* mods cannot observe each other's edits through events, and a future
RPG event system will need explicit, budgeted script-originated events (with a cause /
depth field) instead of this blanket rule.

### World reads: consistency

`world.get_block` / `world.is_loaded` read the engine's live world through a read-only
view that exists only while the engine runs a handler or callback. Within one event
dispatch (or one scheduler tick) every script sees the same state: commands queued by
any script — including the caller — are **not** visible until the engine applies them
after the dispatch/tick. Reads never generate or load chunks. Outside handlers/callbacks
(e.g. at load time) the read functions raise an error.

## Globals

### `rbvg`

| Field | Description |
|---|---|
| `rbvg.api_version` | Integer API version (1) |
| `rbvg.mod_id` | This mod's id |

### `log`

`log.info(msg)`, `log.warn(msg)`, `log.error(msg)` — write to the engine log with the mod id
and line number.

### `events`

`events.subscribe(name, handler)` — call `handler(event)` whenever `name` happens.
Subscribing to an unknown event name is an error (catches typos).

### `world`

Positions are either a table `{ x = int, y = int, z = int }` or three numbers `x, y, z`.
Every coordinate must be a finite integer within the 32-bit range; anything else
(fractions, NaN, ±inf, strings, missing fields) raises an error.

| Function | Capability | Result |
|---|---|---|
| `world.set_block(position, block_id)` | `world.write` | Queues a block change. `block_id` is namespaced, e.g. `"core:stone"`, `"core:air"`. Position accepts the table form. |
| `world.get_block(position)` | `world.read` | The namespaced id at `position`, or **`nil` if its chunk is not loaded**. |
| `world.is_loaded(position)` | `world.read` | `true` if the chunk containing `position` is loaded. |

### `scheduler`

Delays are **simulation time** (Bevy virtual time): when the game is paused, time does not
advance and nothing fires; nothing catches up faster than the simulation runs.

| Function | Description |
|---|---|
| `scheduler.after(seconds, fn) -> handle` | Run `fn()` once, `seconds` (≥ 0) from now. |
| `scheduler.every(seconds, fn) -> handle` | Run `fn()` every `seconds` (≥ 0.05). Stops if `fn` errors. |
| `scheduler.cancel(handle) -> boolean` | Cancel a pending callback; `false` if it already ran or was unknown. |

Rules:

- `seconds` must be a finite number; negative, NaN, infinite or too-small repeat intervals
  are errors. A mod with 256 pending callbacks gets an error from `after`/`every`.
- Callbacks run on the engine's scripting step after event dispatch, each frame, through
  the same protected path as handlers (budget, memory, command cap, error logging).
- A zero-delay callback, and any callback scheduled from inside a callback, runs on the
  **next** scheduler tick at the earliest — never in the tick that scheduled it.
- Callbacks may schedule further callbacks and may read the world and queue commands.
- Order: by deadline, then by scheduling order (deterministic, across all mods).
- If the per-tick cap (64) or a mod's per-tick budget is exhausted, remaining due callbacks
  stay queued, in order, for the next tick. A callback that exceeds the budget errors.
- A failing callback is logged and released; other callbacks and mods keep running.
- A `scheduler.every` callback whose tick fell behind is not run repeatedly to catch up;
  its next deadline is never earlier than "now".
- **Lifecycle:** callbacks are in memory only. They are dropped when their mod is unloaded,
  when the world is reloaded (F9), and on application exit. Closures are never saved.

## Events

All v1 events are **notifications**: they fire after the change has been applied and
cannot be cancelled or modified. Cancellable `before_*` events are planned for Phase 3.

| Event | Payload | When |
|---|---|---|
| `block_broken` | `{ position = {x,y,z}, block = "core:stone" }` (the removed block) | A player broke a block |
| `block_placed` | `{ position = {x,y,z}, block = "core:dirt" }` (the new block) | A player placed a block |

## Security model

- Libraries: Luau safe libraries only (no `io`, no `os.execute`, no `debug` beyond Luau's
  restricted one); mlua's filesystem `require` is removed.
- No native handles: scripts see plain tables and strings, never pointers or ECS references.
- Capabilities gate privileged APIs; host functions validate every argument.
- The Luau sandbox is not treated as a complete security boundary: limits and validation
  are enforced on the host side as well.

## Example

[`mods/regrowth/main.luau`](../mods/regrowth/main.luau) combines events, reads, the
scheduler and validated writes: a broken block grows back after 3 s of simulation time,
but only if its chunk is still loaded and the spot is still empty (a block the player
placed there meanwhile is never overwritten).

```lua
events.subscribe("block_broken", function(e)
	local p = e.position
	local block = e.block
	scheduler.after(3.0, function()
		if not world.is_loaded(p) then
			return
		end
		if world.get_block(p) == "core:air" then
			world.set_block(p, block)
		end
	end)
end)
```

See also [`mods/example_logger/main.luau`](../mods/example_logger/main.luau):

```lua
local broken = 0

events.subscribe("block_broken", function(e)
	broken += 1
	local p = e.position
	log.info(`{e.block} broken at ({p.x}, {p.y}, {p.z}); total broken: {broken}`)
	if broken % 10 == 0 then
		world.set_block(p, "core:stone")
	end
end)
```

## Phase 3 additions

**Events.** `events.subscribe(name, fn)` now accepts `block_broken`, `block_placed`
(notifications), `before_block_break` (**cancellable**) and `interact` (player pressed E
on a block; not cancellable). Every payload has `position`, `block`, `origin`
(`"player"`, `"mod"`; `"engine"` reserved), `origin_mod` (mod origin only) and `depth`
(0 = player/engine, +1 per wave of script-caused events). `before_block_break` payloads
also have `event.cancel()`; calling it elsewhere is a script error. All handlers of a
cancellable event run in mod load order (dependencies first); the action is cancelled if
any called `cancel()`. Commands queued by handlers of a cancelled event are still applied.

**Propagation.** Script `world.set_block` calls that change a block raise
`block_placed`/`block_broken` with origin `mod`. They are queued and dispatched in the
*next wave* after all commands of the current wave were applied (never recursively).
Per pump: max depth 8, max 256 events, max 1024 commands; excess is dropped and logged.
No-op or rejected commands raise no event. Scheduler callbacks' commands go through the
same pump.

**`ui.notify(message)`** (capability `ui.notify`): queues a notification (1–200 chars)
the client shows on screen for a few seconds.

**Manifest.** `dependencies = ["id"]`, `optional_dependencies = ["id"]` (load order is
deterministic, dependencies first; missing deps and cycles are errors), `blocks =
"blocks.toml"` (block definitions in the mod's own namespace; relative path inside the
mod directory). `api_version` must equal the engine's; version ranges are not
implemented. `name` is the human-readable title.

**Save compatibility.** Saves store block names. If a mod is removed, its blocks load as
solid magenta "Missing: id" placeholders and are saved back under their original ids, so
re-enabling the mod restores them. Both startup and F9 use `LoadPolicy::PreserveMissing`.
Placeholders belong to the loaded world's registry snapshot, not installed content.
They cannot be newly placed from the creative catalog. Saving preserves referenced unknown
names under format v1; reinstall the mod and restart to recover real definitions and behavior.
Corrupt or partial saves are errors, never an invitation to overwrite the existing world.

## Authoring a mod

1. Create `mods/<id>/mod.toml` (see `mods/ancient_shrine/mod.toml`) and `main.luau`.
2. Request only the capabilities you need: `world.read`, `world.write`, `ui.notify`.
3. Define blocks in `blocks.toml` with ids `<id>:<name>`.
4. Subscribe to events; schedule delayed work with `scheduler.after`; always check
   `world.is_loaded(p)` before reading or writing after a delay.
5. Errors are logged with mod id and line and never crash the game; a capability error
   means the manifest is missing a capability.

`ancient_shrine`: the `interact` handler ignores every block except
`ancient_shrine:altar`, notifies, and schedules a 3 s ritual. The callback re-checks the
altar is still loaded and present, then turns the blocks around the altar into
`ancient_shrine:glowstone` and notifies again. Other mods observe the result as
`block_placed` events with `origin == "mod"`. Removing the mod removes the behaviour; the
engine has no shrine-specific code.

## Planned API (not implemented)

Target shapes for later phases: `world.spawn_entity(id, pos)`,
`entities.get_component(handle, type)`, `entities.apply_damage(handle, amount)`,
`player.give_item(item, n)`, `player.get_stat(name)`, `quests.start(player, quest)`,
`dialogue.open(player, dialogue)`, custom mod events,
mod-scoped persistent storage. Entity access will use validated generational handles.
