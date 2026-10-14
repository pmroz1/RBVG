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
| F5 / F9 | Save / reload `saves/world` |

The save in `saves/world` is loaded automatically at startup. Delete it to start a new world.

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
