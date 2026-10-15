# RBVG — Rust voxel engine

A moddable voxel sandbox engine written in Rust (2024 edition), built on Bevy 0.20 with
Luau scripting via mlua. The long-term goal is a platform where survival games, RPGs and
scripted adventures are built from content packs and scripts, without recompiling the engine.

**Status: Phase 2 in progress (async streaming, world queries, script scheduler done).** See [docs/ROADMAP.md](docs/ROADMAP.md).

## What works today

- Procedural, seed-deterministic terrain (grass/dirt/stone, wooden trunks) in 16³ chunks,
  generated and meshed on background threads around the player, with stale-result
  detection.
- Face-culled chunk meshes, rebuilt only when a chunk (or its border) changes.
- First-person controller: mouse look, WASD, sprint, jump, gravity, voxel collision.
- Block breaking/placing with a raycast, a 9-slot hotbar and a target outline.
- Creative (default, unlimited blocks) and Survival (F4): mined blocks go into a nine-slot
  inventory of up to 64 per stack, placing consumes one, and the inventory is saved with
  the world.
- Data-driven blocks (`content/core/blocks.toml`) with namespaced ids.
- Versioned save/load storing only edited chunks.
- Sandboxed Luau mods receiving `block_broken` / `block_placed` events and issuing
  validated world commands, reading blocks (`world.get_block`) and scheduling callbacks in
  simulation time (`scheduler.after`). Examples: [`mods/example_logger`](mods/example_logger),
  [`mods/regrowth`](mods/regrowth) (broken blocks grow back after 3 s).
- Debug overlay: frame time avg/p95/max, loaded/rendered/visible chunks, pending/active
  generation and meshing jobs, stale results, generation/meshing/apply latencies, script
  timings.

## Requirements

- Rust stable ≥ 1.85 (`rustup update stable`).
- A C/C++ compiler (Luau is built from source by `mlua`).
- Linux: `sudo apt-get install libasound2-dev libudev-dev libwayland-dev libxkbcommon-dev pkg-config`
  (see the [Bevy Linux guide](https://github.com/bevyengine/bevy/blob/main/docs/linux_dependencies.md)).
- A GPU with Vulkan, Metal or DX12.

## Build and run

From the repository root (the game reads `content/` and `mods/` relative to the working
directory; set `RBVG_DATA_DIR` to run from elsewhere):

```sh
cargo run --release            # play
cargo test --workspace         # tests (no GPU needed)
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all
cargo run --release -p rbvg_voxel --example stream_bench   # headless gen+mesh benchmark
```

`RBVG_SEED=<u32>` picks the seed for a new world (default 1337).

## Controls

| Input | Action |
|---|---|
| Click | Capture mouse |
| Esc | Release mouse |
| Mouse | Look |
| W A S D | Move |
| Left Shift | Sprint |
| Space | Jump |
| Left / right click | Break / place block |
| 1–9 | Select hotbar slot |
| F4 | Toggle Creative / Survival (Creative at start) |
| B | Open/close creative block selection (releases mouse; Creative only) |
| Up / Down, Page Up / Down | Browse every registered block while creative selection is open |
| Enter | Assign highlighted block to the selected hotbar slot |
| E | Interact with the targeted block (mods decide the behavior) |
| F5 / F9 | Save / reload `saves/world` |

The save in `saves/world` is loaded automatically at startup. Delete it to start a new world.
Creative selection lists block names and persistent IDs in alphabetical order, including mod
content. Missing-content entries are visibly marked and cannot be newly placed. Close the
panel and click to recapture the mouse; there are still exactly nine hotbar slots.

In Survival the hotbar shows inventory stacks (`Stone x3`). Breaking a block adds it to the
inventory; if it does not fit, the break is refused ("Inventory full") instead of destroying
the block. Placing uses one block from the selected stack and only places into air. Blocks
whose mod is not installed stay in the inventory, but cannot be placed until it returns.

## Repository layout

```
crates/voxel      rbvg_voxel      renderer-independent world: coords, registry, chunks,
                                   generation, meshing, raycast, collision, persistence
crates/scripting  rbvg_scripting  Luau mod host: manifests, sandbox, events, commands
crates/client     rbvg_client     Bevy app (binary `rbvg`): rendering, input, player, HUD
content/core      core block definitions
mods/             mod packages (one directory per mod, each with mod.toml)
docs/             architecture, roadmap, scripting API, decision log
```

## Documentation

- [Architecture](docs/ARCHITECTURE.md)
- [Roadmap and status](docs/ROADMAP.md)
- [Scripting API](docs/SCRIPTING_API.md)
- [Decision log](docs/DECISIONS.md)

All content in this repository is original placeholder content.

## Manual gameplay procedure

Use a fresh save (move the old directory aside; do not overwrite it). Keep a copy of a
save with mod blocks for the missing-content test.

1. Spawn, click to capture, look around, walk across two chunk borders, sprint and jump.
   Check landing/collision and that unloaded terrain cannot be entered as empty space.
2. Break several blocks; place grass, dirt, stone and wood, including at a chunk border.
   Rapidly edit the same spot, walk away and return: no stale faces or missing neighbors.
3. Open B, browse beyond nine definitions, choose a slot with 1–9 and assign with Enter.
   Close B, recapture, place `ancient_shrine:altar`; target it and press E.
4. Expect “The ancient shrine awakens...”, then the glowstone ring and second message
   after three simulation seconds. Check updated meshes and observer-mod event logs.
5. With a protection mod, attempt a cancelled break: the block stays and no
   `block_broken` notification is emitted. Other listeners must still run.
6. F5 while streaming; move/edit, F9, confirm saved feet position and edited blocks.
   Timers and old-world background results must not modify the restored world.
7. Exit, disable a content mod, load/save: missing blocks remain visible and named.
   Reinstall and restart: the original definitions return. A corrupt/partial save must
   report an error, not start a new world over it.
8. F4 to Survival: mine stone (hotbar shows `Stone x1`), place it (stack disappears),
   F5, mine/place more, F9: inventory and blocks match the save. F4 back to Creative and
   place unlimited blocks.
9. Check texture faces upright, illumination across borders, target outline, UI text,
   Escape/re-capture, window focus loss and mouse capture on your actual OS/GPU.

Synthetic input tests cannot certify native cursor lock, focus, display scaling or GPU
driver behavior. Software-renderer results are not representative GPU FPS measurements.
