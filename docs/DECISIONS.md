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

## D10 — Notification-only events in API v1
*Why:* cancellation needs a defined "before" phase and ordering rules; deferring keeps v1
honest. Planned as `before_*` events in Phase 3.

## D11 — Simple AABB collision instead of Rapier
Axis-separated sweep with sub-steps against voxels. *Why:* players only need voxel
collision now; Rapier is justified once entities need rigid-body interactions.

## D12 — Main-thread generation and meshing with per-frame caps (temporary)
*Why:* simplest correct streaming for Phase 1. First Phase 2 task is moving both to
Bevy's async compute pool.
