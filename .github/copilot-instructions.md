# Copilot instructions — RBVG

RBVG is a Rust (2024) voxel engine meant to become a platform for script- and data-driven
games (sandbox, RPG, adventure). The voxel world is the foundation; scripting turns it
into a platform. Keep that vision in every change.

## Before changing code

- Read `docs/ARCHITECTURE.md`, `docs/ROADMAP.md`, `docs/DECISIONS.md` and the code you touch.
- Make the smallest change that fully delivers a working, tested increment. No placeholder
  code that pretends to work; no scaffolding for future phases.
- Verify crate APIs against the versions in `Cargo.lock` (Bevy 0.20, mlua 0.12) — do not
  write code from memory of older Bevy versions.

## Architecture rules

- `rbvg_voxel` and `rbvg_scripting` must not depend on Bevy. Simulation must run headless.
- Never make individual blocks ECS entities. Chunks are dense 16³ `BlockId(u16)` arrays.
- Content uses namespaced ids (`namespace:name`, `[a-z0-9_]`). Runtime numeric ids are not
  stable and must never be persisted; persist names (see the palette in `persistence.rs`).
- Use `div_euclid`/`rem_euclid` for world↔chunk conversions (negative coordinates).
- World generators implement `WorldGenerator` and stay deterministic per seed.
- World edits go through `VoxelWorld::set_block` so dirty tracking, revisions and events
  stay correct.
- Chunk streaming/meshing decisions live in `rbvg_voxel::streaming::ChunkPipeline`; the client
  only runs its jobs on `AsyncComputeTaskPool`. Jobs take owned inputs; validate every
  async result against tickets and chunk revisions before applying. Never block on a task
  in a frame system.
- Keep rendering out of gameplay logic; the client only translates data to Bevy.

## Scripting rules

- One sandboxed Luau VM per mod. Scripts never receive native pointers, ECS references,
  filesystem, network or OS access.
- Script API functions validate arguments and queue `ScriptCommand`s; the engine applies
  them after dispatch. Document visibility/timing for any new API.
- Privileged APIs require a manifest capability.
- Every script execution path runs under the budget (`ModInstance::run`). Script errors are
  logged with mod id and line, and never crash the engine.
- Update `docs/SCRIPTING_API.md` and bump `API_VERSION` for breaking changes. State whether
  each event is cancellable.
- Do not run script code per voxel or per frame without a clear budget.
- World reads go through the scoped `WorldQuery` (`world.read` capability); never hand
  scripts mutable or long-lived world access. Scheduled callbacks run via `ScriptHost::tick`
  in simulation time and are never persisted.

## Quality bar

- Errors: typed errors (`thiserror`) in libraries, no panics on recoverable failures, never
  silently ignore errors. Validate all external/mod data.
- Avoid allocations in hot loops (meshing, generation, collision); reuse buffers.
- No `unsafe` without a documented justification.
- Add unit tests for new core logic and integration tests for workflows.
- Before finishing: `cargo fmt --all`, `cargo clippy --workspace --all-targets -- -D warnings`,
  `cargo test --workspace`. Never claim results you did not run.
- Keep README, ROADMAP status and DECISIONS in sync with the implementation; record new
  trade-offs in `docs/DECISIONS.md`.
- Multiplayer is future work, but never trust a client for damage, inventory or world edits;
  keep mutations behind validated APIs.
- No copyrighted assets or code from existing games.
