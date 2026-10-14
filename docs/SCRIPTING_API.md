# Scripting API (v1)

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

Mods are discovered in `mods/*/mod.toml` and loaded in id order. A mod that fails to load
is reported and skipped; the game still starts. Unknown manifest keys or capabilities are
errors.

## Execution model

- The entry script runs once at load time. It should register event handlers; world
  commands issued during load are ignored (with a warning).
- Handlers run only when the engine dispatches an event. There is no per-frame script
  callback.
- Every mod has its own Luau VM: globals, state and errors never leak between mods.
- Script globals are per-mod; the standard library and engine API tables are read-only.
- **Budgets:** loading and each event dispatch get 5 ms of wall-clock time per mod; a VM
  may use 16 MiB of heap; a handler may queue 256 commands per event. Exceeding a limit
  raises a Luau error in that handler.
- **Errors:** a failing handler is logged as `mod_id` + `@<mod_id>/<file>:<line>: message`
  and skipped. Other handlers and mods still run. The game never crashes due to a script.

### When script changes become visible

`world.*` mutation functions do not change the world directly; they queue commands.
After all mods have handled an event, the engine validates each command (block id
registered, chunk loaded) and applies it, in the same frame, in mod load order. Invalid
commands are logged and dropped. Changes made by scripts do **not** raise further
script events, which prevents feedback loops.

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

`world.set_block(position, block_id)` — queue a block change. `position` is
`{ x = int, y = int, z = int }`; `block_id` is a namespaced id such as `"core:stone"` or
`"core:air"`. Requires the `world.write` capability.

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

See [`mods/example_logger/main.luau`](../mods/example_logger/main.luau):

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

## Planned API (not implemented)

Target shapes for later phases: `world.get_block(pos)`, `world.spawn_entity(id, pos)`,
`entities.get_component(handle, type)`, `entities.apply_damage(handle, amount)`,
`player.give_item(item, n)`, `player.get_stat(name)`, `quests.start(player, quest)`,
`dialogue.open(player, dialogue)`, `scheduler.after(seconds, fn)`, custom mod events,
mod-scoped persistent storage. Entity access will use validated generational handles.
