# Decision log

Short records of choices and their trade-offs. Newest last.

## D1 — Three-crate workspace
Voxel world, scripting and client are separate crates; the two libraries do not depend on
Bevy. *Why:* keeps simulation headless-testable and server-ready without creating many
tiny crates up front. *Cost:* the client holds a few gameplay systems (player, interaction)
that will move to a shared simulation crate when the server arrives.

## D2 — Bevy 0.20 with `3d` + `ui` features only
Default features minus audio and 2D. *Why:* smaller build. Add `audio` when sound lands.

## D3 — Cubic 16³ chunks, dense `u16` storage
*Why:* simple indexing, cache-friendly, vertical unboundedness, 8 KiB per chunk.
*Alternative considered:* palette-compressed storage — worth it once memory is measured as
a problem (large view distances).

## D4 — Runtime ids vs persistent ids
`BlockId(u16)` is assigned by registration order; saves store names via a palette.
*Why:* runtime speed with stable saves across mod configurations.

## D5 — Edits-only persistence, custom binary chunk file
Only modified chunks are saved; the rest regenerates from the seed. Binary layout is
versioned with a magic header and palette; metadata is TOML. Temp-file + rename writes.
*Trade-off:* the generator must stay deterministic per seed and version; a generator
change needs a save-format/version bump.

## D6 — Face-culled meshing with vertex colours
*Why:* smallest correct mesher; output type is mesher-agnostic so greedy meshing and a
texture array can replace it without touching callers.

## D7 — One Luau VM per mod, `mlua` with `luau` + `send`
*Why:* hard isolation of state and failures between mods; `send` makes the host a normal
Bevy `Resource`. *Cost:* per-VM memory overhead (small for Luau).

## D8 — Scripts emit commands, never mutate directly
API functions queue `ScriptCommand`s that the engine validates and applies after dispatch.
*Why:* no borrows across the boundary, one validation point, and the same shape works for
an authoritative server.

## D9 — Wall-clock budget via Luau interrupts
5 ms per mod per event, plus heap and command caps. *Why:* guarantees scripts cannot
freeze the frame. *Trade-off:* wall-clock is nondeterministic; a deterministic
instruction-count budget may be preferable for lockstep/replay later.

## D10 — Notification-only events in API v1 (superseded by D21)
*Why:* cancellation needs a defined "before" phase and ordering rules; deferring keeps v1
honest. Planned as `before_*` events in Phase 3.

## D11 — Simple AABB collision instead of Rapier
Axis-separated sweep with sub-steps against voxels. *Why:* players only need voxel
collision now; Rapier is justified once entities need rigid-body interactions.

## D12 — Main-thread generation and meshing with per-frame caps (superseded by D13)
*Why:* simplest correct streaming for Phase 1.

## D13 — Async pipeline: Bevy-free scheduler, Bevy task pool as executor
`rbvg_voxel::streaming::ChunkPipeline` decides what to generate/mesh and validates
results; the client only spawns its jobs on `AsyncComputeTaskPool` and polls finished
tasks. *Why:* all ordering, dedup and staleness logic is unit-testable with simulated
completions, and a headless server can drive it with any executor. Jobs own immutable
inputs (`Arc` generator/registry, copied snapshots), so there is no shared mutable world
and no lock. *Trade-off:* a few small maps instead of an explicit state machine; started
tasks cannot be interrupted, their results are just discarded.

## D14 — Globally unique chunk revisions as incarnation + version
A process-wide atomic counter stamps a chunk on install and on every change. *Why:* one
`u64` comparison detects both edits and unload/reload (even after a world reload), where
per-chunk counters could coincide. A mesh is keyed by the revisions of the chunk and its
6 face neighbours. *Trade-off:* conservative — an interior edit in a neighbour also
invalidates an in-flight mesh (it is simply redone).

## D15 — Mesh snapshots copy the chunk plus one border layer per face
18³ padded copy (~11 KiB) per job. *Why:* owned input, no locks during meshing, and
exactly the data face culling needs. Edges/corners are not needed for culling; they will
be added when ambient occlusion needs them.

## D16 — World reads via a scoped, read-only query (no snapshots)
Scripts read through `WorldQuery`, a narrow trait the engine implements over
`&VoxelWorld`. It is installed with `Lua::scope` only for the duration of one handler or
callback call. *Why:* zero copying, no lifetime escape, no reentrancy (commands are still
queued), and the scripting crate stays Bevy-free. Consistency: one dispatch/tick sees one
world state; queued commands are visible after they are applied. Unloaded → `nil`, never
a synchronous load. Gated by the `world.read` capability.

## D17 — In-memory, simulation-time scheduler per mod
Per-mod binary heap ordered by `(deadline, global seq)`; callbacks live in the mod VM's
registry table and are removed on run/cancel/error/unload/reload. *Why:* deterministic
order, O(log n) per operation, no threads, no leaks, and no closure serialization.
Zero-delay and nested callbacks run on the next tick, so a mod cannot loop forever in one
tick. *Trade-off:* scheduled work is lost on world reload and exit; durable timers will
need data-only (serializable) jobs later.

## D18 — Script edits still do not raise events (superseded by D19)
Commands from handlers and callbacks are applied without dispatching `block_*` events.
*Why:* prevents feedback loops between mods. *Limitation:* mods cannot react to other
mods' edits; an RPG event system will need explicit script-originated events with a cause
and recursion depth limit.

## D19 — Wave-based bounded event pump
Script edits now raise events with origin `mod` and depth+1, dispatched in later waves by
`rbvg_scripting::pump` (depth 8 / 256 events / 1024 commands per pump, excess dropped and
logged). *Why:* cross-mod reactions without recursion or unbounded cascades. The engine
side implements `CommandApplier`, keeping the pump Bevy-free and testable headless.
*Trade-off:* dropped events are lost, not deferred. (Events over the event limit are now
deferred, see D25.)

## D20 — Missing mod content becomes placeholders
Unknown palette names in a save register as solid "Missing" blocks so they round-trip
instead of becoming air or blocking the load. Mods' block files are registered before the
save is read.

## D21 — Cancellable before-block-break events
`before_block_break` runs before a player break and may call `e.cancel()`; cancellation
is combined across handlers in deterministic mod order. Other events are notifications
and cannot be cancelled. *Why:* mods can protect blocks without coupling gameplay to
rendering. Commands remain queued and are applied after dispatch, even when cancelled.

## D22 — Creative catalog, not survival inventory
Keep nine slots, with a keyboard-scrollable, alphabetically sorted content catalog and
explicit slot assignment. Selection/validation lives in the voxel library; Bevy only
renders the panel and translates keys. Missing definitions are visible but unavailable
for new placement. *Why:* all mod content is reachable without hardcoded IDs or item,
stacking and crafting systems.

## D23 — World-scoped missing content, explicit load policy
Startup and F9 use `PreserveMissing`; strict loading remains available to library callers.
Stage registry and chunks before replacing the world; keep the installed content registry
separate. Save only palette names actually referenced by edited chunks, retaining unknown
names exactly. *Why:* disabling/reinstalling a mod must not lose blocks or leave obsolete
placeholders in global content. Schema stays v1. *Limit:* the two save files are separately
atomic, not jointly crash-atomic; a future durable-save revision needs a generation manifest.

## D24 — Generated padded atlas instead of image assets
Each block gets a deterministic 16×16 tile derived from its colour and id, packed into one
square atlas with a 1-texel replicated border, used by one shared material with nearest
sampling. *Why:* textured, upright faces for all content (including mod blocks) without
shipping or licensing images, and no per-block materials. *Limit:* no authored textures,
mipmaps, per-face textures or transparency yet; a later `texture = "..."` field in block
files can fill the same tiles.

## D25 — Bounded deferral of script events
Events over the per-pump event limit (256) are carried over, in order and with their depth,
up to 256 per pump, and dispatched first in the next frame. Depth overruns and command
overruns are still dropped and logged. Loading another world clears the backlog. *Why:* a
burst of legitimate events (e.g. a large ritual) is not silently lost, while the depth
limit still ends feedback loops and the per-frame cost stays bounded.

## D26 — Minimal survival block inventory, stored in `world.toml`
Survival uses a nine-slot, 64-per-stack `BlockInventory` of placeable blocks in
`rbvg_voxel`, not items or ECS entities. Mining adds the actually removed block after
`set_block`, refuses the break when it does not fit, and grants nothing for cancelled
breaks or script edits; placement consumes one block only after a successful edit. The
inventory is saved by block name as optional `[[inventory]]` tables in `world.toml`, so
schema v1 saves without it still load (empty inventory) and no new reader is needed; older
engines reject the unknown key instead of losing it. Unknown names become world-scoped
missing placeholders, like palette names. The game mode is not persisted. *Why:* a
playable loop without an item hierarchy, crafting or a new file. *Limit:* blocks only; no
drops, splitting UI or per-player inventories, and a block whose definition later becomes
non-solid stays stored but cannot be placed.
