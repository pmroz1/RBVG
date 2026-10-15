# Roadmap

Status legend: ✅ done · 🟡 partial · ⬜ not started

## Phase 1 — Playable voxel prototype (current)

| Item | Status | Notes |
|---|---|---|
| Rust workspace | ✅ | `rbvg_voxel`, `rbvg_scripting`, `rbvg_client` |
| Bevy 3D window | ✅ | Bevy 0.20 |
| First-person camera and movement | ✅ | WASD, sprint, mouse look |
| Procedural voxel world | ✅ | Seeded fBm heightmap, trunks; streamed within 6 chunks horizontally / 2 vertically |
| Chunked block storage | ✅ | Dense 16³ `u16` chunks |
| Face-culled meshing | ✅ | Culls across chunk borders |
| Multiple block types | ✅ | grass, dirt, stone, wood from TOML |
| Break and place | ✅ | Raycast, reach 6, hotbar 1–9 |
| Gravity and voxel collision | ✅ | Axis-separated AABB sweep |
| Save and load | ✅ | F5/F9, auto-load at start, edited chunks only |
| Minimal scripting runtime with real events | ✅ | `block_broken`, `block_placed`, `world.set_block` |
| Automated tests | ✅ | Coordinates, registry, generation, world, meshing, raycast, physics, persistence, manifests, sandbox, budgets, example mod |

Known Phase 1 limitations (still open unless noted):

- ~~Generation and meshing run on the main thread with per-frame caps.~~ Done in Phase 2.
- ~~No textures~~ (generated atlas since Phase 3.7); no transparency or lighting beyond one
  directional light.
- One fixed save slot (`saves/world`); saving writes the whole palette and all edited chunks.
- ~~Scripts cannot read the world yet.~~ Done in Phase 2.
- No mod dependency resolution; mods load in id order.
- Not measured on real GPUs: only llvmpipe software rendering was used for smoke tests.

## Phase 2 — Sandbox gameplay (in progress)

| Item | Status | Notes |
|---|---|---|
| 2.1 Async terrain | ✅ | `ChunkPipeline`: bounded, nearest-first, dedup, cancel out-of-range, revision-checked results, edits win |
| 2.2 Async meshing | ✅ | Owned snapshots with border layers, 7-revision mesh keys, stale discard + re-dirty, bounded uploads |
| 2.3 World queries | ✅ | `world.get_block`, `world.is_loaded`, `world.read` capability |
| 2.4 Scheduling | ✅ | `scheduler.after` / `every` / `cancel`, simulation time, per-mod limits |
| 2.5 Integration | ✅ | `mods/regrowth`, headless end-to-end test, `stream_bench` example |

Not validated: interactive gameplay on a real GPU (only headless tests and benchmarks
were run for Phase 2).

Remaining Phase 2 items:

3. ~~Block textures~~ (generated padded atlas, 3.7); transparent material path (separate
   opaque/transparent meshes) and authored texture files remain.
4. Greedy meshing, mesh buffer pooling, frustum-aware load priority.
5. Biome registry and generator stages (height → surface → caves → ores → features).
6. ~~Hotbar backed by inventory~~ (minimal survival block inventory, D26); item registry
   and standalone items, drops as entities, inventory UI remain.
7. Crafting/recipe registry.
8. Light propagation (sky + block light), stored per chunk.
9. Basic UI framework for inventory/crafting.

## Phase 3 — Modding foundation

Manifest dependencies and version ranges, data-driven blocks/items/entities in mods,
cancellable "before" events, mod persistent state (namespaced in saves), hot reload,
more example mods, generated API docs and editor type definitions.

## Phase 4 — RPG toolkit

Entities and NPCs (ECS components, spawn rules), stats/attributes, experience, equipment,
combat with damage types, spells/projectiles, status effects, AI (behaviour trees),
dialogue and quest registries with persistent state, a sample fantasy RPG experience.

## Phase 5 — Multiplayer and creator platform

Headless `rbvg_server`, authoritative simulation, chunk/entity replication with interest
management, server-side scripts, permissions, world editing tools, experience packaging.

## Phase 3 status
Done: events with origin/depth, `before_block_break` cancellation, `interact` (E), bounded
event pump, `ui.notify`, mod dependencies and resolution, mod block files, missing-content
placeholders, `ancient_shrine` mod with headless end-to-end test.

**Not done / unverified:** mod version ranges, greedy meshing, authored texture files,
real-GPU performance. Rendering was only checked on llvmpipe under Xvfb (textured terrain
and the creative panel opened by a synthetic X key event); the checklist below and the
README's manual procedure are for a human on real hardware:
cross chunk borders while moving fast; break/place at chunk borders while meshes update;
F5 during streaming then F9; wait for regrowth / replace the block first; walk away and
return while timers pend; open B, assign `ancient_shrine:altar` to a hotbar slot, place it,
press E, expect the message and glow ring after 3 s.

### Completion milestones

| Milestone | Status | Notes |
|---|---|---|
| 3.6 Reliability | ✅ | Creative catalog; staged, lossless load policy; F9 drops old-world work (tested) |
| 3.7 Textures | ✅ | Generated padded atlas (`rbvg_voxel::atlas`), upright UVs, one shared material |
| 3.8 Gameplay validation | ✅ | Synthetic keyboard/mouse tests (creative, place/break/cancel, F5/F9); llvmpipe run |
| 3.9 Event reliability | ✅ | Bounded deferral (D25); rejected commands logged per mod; per-mod handler isolation |
| 3.10 Release quality | 🟡 | fmt/clippy/tests green; `stream_bench` run; real-GPU FPS not measured |
| 3.11 Survival inventory | ✅ | F4 mode; 9×64 block inventory; mine/place/cancel/full rules; saved by name in `world.toml` (headless tests only, no real-GPU run) |
