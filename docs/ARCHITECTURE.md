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
- `VoxelWorld` keeps loaded chunks in a `HashMap<ChunkPos, Chunk>`, plus a *stash* of
  edited chunks that are not resident (unloaded or restored from a save before being
  streamed in), and a *dirty set* of chunks whose mesh is stale. Edits on a chunk border
  also dirty the neighbour. Blocks are never ECS entities; one entity per non-empty
  rendered chunk.

## World generation

`WorldGenerator` (`Send + Sync`, `fn generate(&self, ChunkPos) -> Chunk`) is the extension
point. `TerrainGenerator` is a 4-octave fBm Perlin heightmap with grass/dirt/stone layers
and hashed tree trunks. Every block is a pure function of `(seed, x, y, z)`, so chunks can
be generated in any order and on any thread, and only edits need saving.

## Meshing and rendering

- `meshing::mesh_chunk` emits one quad per block face whose neighbour is not solid,
  looking across chunk borders through the world (unloaded neighbours count as air). It
  writes into a caller-owned `ChunkMesh`, so buffers are reused between chunks.
- Output is per-vertex block ids; the client maps them to colours. Swapping in a greedy
  mesher or a texture-array material only changes this function / the client mapping.
- The client remeshes up to 12 dirty chunks per frame, nearest first, and keeps one
  entity per chunk with a single shared `StandardMaterial` (vertex colours).

## Frame flow (client)

1. `stream_chunks`: unload chunks beyond radius + 1, load up to 8 missing chunks nearest
   the player.
2. `mesh_dirty_chunks`: rebuild meshes for dirty chunks.
3. Player: cursor capture → look → move (`move_aabb`, gravity, jump).
4. Interaction (`InteractionSystems`): hotbar → raycast target → break/place → emits
   `BlockChanged` messages.
5. Scripting (after interaction): each `BlockChanged` becomes a `ScriptEvent`, dispatched
   to mods; returned `ScriptCommand`s are validated and applied immediately.
6. Save/load keys, HUD.

## Scripting integration

See [SCRIPTING_API.md](SCRIPTING_API.md) for the API. Structure:

- `ScriptHost` owns one `mlua::Lua` per mod. VMs are created with Luau's safe libraries,
  `require` removed, the engine API installed, then `Lua::sandbox(true)` freezes globals
  and libraries.
- Engine → script: `ScriptHost::dispatch(&ScriptEvent)` calls subscribed handlers.
- Script → engine: API functions only *queue* `ScriptCommand`s. The host returns them and
  the engine validates (block id registered, chunk loaded) and applies them. Scripts never
  hold references to Rust data, so there are no borrow conflicts across the boundary.
- Limits: a wall-clock budget per mod per event (Luau interrupt), a heap limit per VM,
  and a cap on commands per event. Failing handlers are logged with mod id, chunk name and
  line, and skipped.

## Persistence

`saves/<name>/world.toml` (format version, seed, player position) plus `chunks.bin` (edited
chunks only, with a block-name palette so runtime ids can change between sessions). Files
are written to a temp file, fsynced and renamed. See `rbvg_voxel::persistence` for the
byte layout.

## Multiplayer readiness

- Simulation state (`VoxelWorld`, registry, script host) is plain Rust, not Bevy-specific.
- Mutations go through a single API (`VoxelWorld::set_block`) and are reported as
  messages, which is where an authoritative server will validate client requests.
- Script commands are data, so they can be produced on a server and replicated.
