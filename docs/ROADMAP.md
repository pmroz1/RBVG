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

Known Phase 1 limitations:

- Generation and meshing run on the main thread with per-frame caps.
- No textures, transparency or lighting beyond one directional light; blocks use colours.
- One fixed save slot (`saves/world`); saving writes the whole palette and all edited chunks.
- Scripts cannot read the world yet (`world.get_block` is the next API addition).
- No mod dependency resolution; mods load in id order.
- Not measured on real GPUs: only llvmpipe software rendering was used for smoke tests.

## Phase 2 — Sandbox gameplay

Recommended order:

1. Move chunk generation and meshing to `AsyncComputeTaskPool` (generate → snapshot
   chunk + border → mesh off-thread → apply when versions still match).
2. `world.get_block` and `scheduler.after` script APIs.
3. Block texture array + transparent material path (separate opaque/transparent meshes).
4. Greedy meshing, mesh buffer pooling, frustum-aware load priority.
5. Biome registry and generator stages (height → surface → caves → ores → features).
6. Item registry, inventory, drops, hotbar backed by inventory.
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
