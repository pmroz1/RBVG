# Architecture

## Goals that shape the structure

1. The simulation must run without a renderer (tests today, a dedicated server later).
2. Scripts are a first-class way to define gameplay, and they never get direct access to
   engine memory.
3. Content is data, identified by stable namespaced ids.

## Workspace

| Crate | Depends on | Owns |
|---|---|---|
| `rbvg_voxel` | serde, toml, noise, thiserror | Coordinates, block registry, chunk storage, `VoxelWorld`, world generators, meshing (to plain vertex arrays), raycast, AABB collision, persistence |
| `rbvg_scripting` | `rbvg_voxel`, mlua (Luau), tracing | Mod manifests, one sandboxed Luau VM per mod, event dispatch, script command queue, execution budgets |
| `rbvg_client` (bin `rbvg`) | both + Bevy | Window, rendering, input, first-person player, block interaction, HUD, save/load keys, wiring scripts to gameplay |

Neither library crate depends on Bevy. A future `rbvg_server` crate (headless Bevy
`MinimalPlugins` or a plain loop) can reuse both unchanged. Gameplay systems that are
currently in the client (player movement, interaction) are thin and use library-crate
logic (`physics::move_aabb`, `raycast::raycast`), so moving them to a shared
simulation crate later is mechanical.

This is intentionally three crates, not ten. New crates are split out when a boundary
is needed (e.g. `rbvg_sim` once a server exists), not in advance.

## Coordinate system

- Right-handed, +Y up, −Z forward — identical to Bevy, so no conversions.
- `BlockPos(i32, i32, i32)`: block `(x, y, z)` occupies `[x, x+1) × [y, y+1) × [z, z+1)`.
- `ChunkPos`: block coordinate `div_euclid` 16. `LocalPos`: `rem_euclid` 16. Euclidean
  division makes negative coordinates correct (block −1 is local 15 of chunk −1).
- `BlockPos::containing(point)` floors a world-space point.
- Chunks are cubic (16³), so the world is unbounded vertically as well as horizontally.

## Chunk format

- `Chunk` holds `Box<[BlockId; 4096]>` (8 KiB), index `x + 16·z + 256·y`.
- `BlockId(u16)` is a **runtime** id assigned in registration order; `0` is always
  `core:air`. Runtime ids are never persisted.
- Each chunk carries a `version` counter (bumped on every change) and a `modified` flag
  (set by edits, not by generation) that decides what gets saved.
- `VoxelWorld` keeps loaded chunks in a `HashMap<ChunkPos, Chunk>` together with a
  **revision**: a process-wide unique `u64` assigned when a chunk becomes resident (load,
  generated insert, save restore) and again on every changing `set_block`. Because it is
  never reused, a revision identifies both the chunk *incarnation* and its *version*, even
  across unload/reload and world reloads. Async work is validated against it. The world also keeps a *stash* of
  edited chunks that are not resident (unloaded or restored from a save before being
  streamed in), and a *dirty set* of chunks whose mesh is stale. Edits on a chunk border
  also dirty the neighbour. Blocks are never ECS entities; one entity per non-empty
  rendered chunk.

## Async chunk pipeline

All scheduling decisions live in `rbvg_voxel::streaming::ChunkPipeline` (Bevy-free and
deterministically testable). The client only runs the jobs it hands out on Bevy's
`AsyncComputeTaskPool` and passes results back; it never waits on a task (`block_on`
only on tasks that report `is_finished()`).

Lifecycle per chunk position (kept as small maps, not a general state machine):

| State | Representation |
|---|---|
| Requested | in range and missing (counted as *pending generation*) |
| Generating | `generating[pos] = ticket`, job in flight |
| Generated / loaded | resident in `VoxelWorld` with a revision |
| Meshing | dirty, `meshing[pos] = ticket`, job in flight |
| Ready | `applied[pos] = MeshKey` of the shown mesh |
| Unloading | beyond radius + 1: chunk unloaded (edits stashed), its tickets forgotten |

- **Generation** (`update`): unload chunks beyond radius + 1 and forget their in-flight
  jobs (results are discarded on arrival), restore stashed edited chunks synchronously
  (cheap, and edits must win), then admit at most `max_admit_per_tick` new `GenJob`s
  nearest-first while fewer than `max_generating` are in flight. A position with a job in
  flight is never requested twice. A `GenJob` owns only `pos`, a `ticket` and an
  `Arc<dyn WorldGenerator>`, so workers share no mutable state.
- **Generation results** (`finish_generation`) are applied only if the ticket is still the
  current one for that position *and* `VoxelWorld::insert_generated` accepts it: it refuses
  if the chunk is already resident or an edited copy is stashed, so persisted/edited data
  always beats terrain.
- **Meshing** (`schedule_meshing`): dirty, loaded chunks are prioritised by squared
  distance minus ticks waited (aging, so far chunks cannot starve), at most `max_meshing`
  in flight and one job per chunk. A job owns a `ChunkSnapshot` — the 16³ chunk plus the
  touching layer of each of the 6 face neighbours (18³ padded copy, ~11 KiB) — and a
  `MeshKey`: the revisions of the chunk and its 6 neighbours at capture time. No locks are
  held while meshing; the snapshot is owned. A dirty chunk whose key equals the shown
  mesh's key is cleaned without work.
- **Mesh results** (`finish_meshing`) are applied only if the ticket matches and the key
  still equals the current key. Otherwise the result is counted as stale and the chunk is
  re-dirtied, so the newest data is meshed next. Because revisions are globally unique,
  a result from a previous incarnation can never match a reloaded chunk.
- **Boundary edits** dirty the neighbour (as before) and change its key (the neighbour's
  key contains the edited chunk's revision), so in-flight neighbour meshes are discarded.
- **Client application:** at most `max_apply_per_tick` meshes are uploaded per frame;
  entities of unloaded chunks are despawned. Tasks that are no longer current are dropped,
  which cancels them if they have not started.
- **Saving during activity:** saving reads only `VoxelWorld` (resident + stashed edited
  chunks); in-flight jobs hold no world data that can be lost. Generated chunks are never
  `modified`, so they are not saved. F9 reload calls `ChunkPipeline::reset()`.

`StreamConfig` defaults: radius 6 (horizontal) / 2 (vertical), 8 generation and 8 meshing
jobs in flight, 8 admissions and 12 mesh uploads per frame.

### Benchmark

`cargo run --release -p rbvg_voxel --example stream_bench [seed] [radius]` streams a
(2r+1)² × 5 region through the real pipeline with worker threads, no GPU. Reference run
(seed 42, radius 6, 845 chunks, 4-core CI-class VM, CPU only):

| Stage | avg/job | p50 | p95 | max | wall (4 threads) |
|---|---|---|---|---|---|
| Generation | 45 µs | 42 µs | 84 µs | 173 µs | 53 ms |
| Meshing (incl. snapshot copy) | 50 µs | 57 µs | 106 µs | 217 µs | 107 ms |

Wall time includes spawning a thread per job in the benchmark harness. These numbers say
nothing about GPU upload or rendering cost.

## World generation

`WorldGenerator` (`Send + Sync`, `fn generate(&self, ChunkPos) -> Chunk`) is the extension
point. `TerrainGenerator` is a 4-octave fBm Perlin heightmap with grass/dirt/stone layers
and hashed tree trunks. Every block is a pure function of `(seed, x, y, z)`, so chunks can
be generated in any order and on any thread, and only edits need saving.

## Meshing and rendering

- `meshing::mesh_snapshot` emits one quad per block face whose neighbour is not solid,
  looking across chunk borders through the snapshot's border layers (unloaded neighbours
  count as air). `mesh_chunk` (capture + mesh) remains for synchronous use and tests.
- Output is per-vertex block ids plus face-local UVs (upright on side faces, not mirrored).
  Swapping in a greedy mesher only changes this function / the client mapping.
- `atlas::BlockAtlas` builds one 16×16 tile per registered block (1-texel replicated
  border, plus a checkerboard fallback) from content colours and ids, deterministically and
  without image assets. Missing-content placeholders get the magenta/black checkerboard.
- The client keeps one entity per non-empty chunk with a single shared
  `StandardMaterial` whose base colour texture is the atlas (nearest sampling). When the
  registry changes (F9) the atlas image is replaced in place before the full remesh.

## Frame flow (client)

1. `stream_chunks`: collect finished generation tasks, `ChunkPipeline::update`, spawn new
   generation tasks.
2. `mesh_chunks`: despawn unloaded chunk entities, apply finished meshes (bounded), spawn
   new meshing tasks.
3. Player: cursor capture → look → move (`move_aabb`, gravity, jump).
4. Interaction (`InteractionSystems`): hotbar → raycast target → break/place → emits
   `BlockChanged` messages.
5. Scripting (after interaction): each `BlockChanged` becomes a `ScriptEvent`, dispatched
   to mods; then `ScriptHost::tick(virtual dt)` runs due scheduled callbacks. Returned
   `ScriptCommand`s are validated and applied immediately.
6. Save/load keys, HUD (frame avg/p95/max, chunk counts, pipeline queues, stale counts,
   generation/mesh/apply latencies, script timings, pending callbacks).

## Scripting integration

See [SCRIPTING_API.md](SCRIPTING_API.md) for the API. Structure:

- `ScriptHost` owns one `mlua::Lua` per mod. VMs are created with Luau's safe libraries,
  `require` removed, the engine API installed, then `Lua::sandbox(true)` freezes globals
  and libraries.
- Engine → script: `ScriptHost::dispatch(&ScriptEvent, &dyn WorldQuery)` calls subscribed
  handlers; `ScriptHost::tick(dt, &dyn WorldQuery)` advances simulation time and runs due
  scheduled callbacks.
- Script reads: `WorldQuery` is a narrow read-only trait (`block(pos) -> Option<&str>`).
  The client passes a `VoxelQuery` borrowing `&VoxelWorld` + registry; it is exposed to
  Luau only for the duration of one `ModInstance::run` through `Lua::scope`, so no
  reference outlives the call and nothing is copied. Scripts in one dispatch/tick all see
  the same world state; their commands are applied afterwards.
- Scheduler: each mod owns a deadline-ordered binary heap (`deadline, seq`) and keeps the
  callbacks in a per-VM registry table keyed by handle; entries are removed on completion,
  error, cancel, mod unload and world reload, so references are released.
- Script → engine: API functions only *queue* `ScriptCommand`s. The host returns them and
  the engine validates (block id registered, chunk loaded) and applies them. Scripts never
  hold references to Rust data, so there are no borrow conflicts across the boundary.
- Limits: a wall-clock budget per mod per event and per mod per scheduler tick (Luau
  interrupt), a heap limit per VM, a cap on commands per event/callback, on pending
  callbacks per mod, and on callbacks per tick. Failing handlers are logged with mod id, chunk name and
  line, and skipped.

## Persistence

`saves/<name>/world.toml` (format version, seed, player position) plus `chunks.bin` (edited
chunks only, with a block-name palette so runtime ids can change between sessions). Files
are written to a temp file, fsynced and renamed. See `rbvg_voxel::persistence` for the
byte layout.

Startup and F9 explicitly use `LoadPolicy::PreserveMissing`. Unknown names become solid
magenta, non-placeable placeholders in a **world-scoped registry snapshot**, not installed
content. Loading is staged and commits only after validation; invalid versions, malformed
palettes, duplicate chunks, truncation and partial saves fail rather than starting a world
that might overwrite the save. Saving writes only referenced names, preserving unknown
blocks under schema v1. Reinstalling content before startup restores its real definition.
F9 rebuilds from installed content, remaps hotbar names and resets async work and timers.
Each file is atomic individually; the pair is not a crash-atomic transaction.

## Creative selection

`rbvg_voxel::selection` owns the nine-slot selection and registry validation. The client
provides a keyboard-scrollable B panel, sorted by persistent ID, with readable names and
missing-content labels. Enter assigns the highlighted definition to the selected 1–9
slot. This is creative selection only, not a survival inventory or an item registry.

## Multiplayer readiness

- Simulation state (`VoxelWorld`, registry, script host) is plain Rust, not Bevy-specific.
- Mutations go through a single API (`VoxelWorld::set_block`) and are reported as
  messages, which is where an authoritative server will validate client requests.
- Script commands are data, so they can be produced on a server and replicated.
