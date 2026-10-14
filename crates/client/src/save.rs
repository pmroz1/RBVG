//! F5 saves, F9 reloads the last save. The save is also loaded at startup.

use std::path::Path;
use std::sync::Arc;

use bevy::prelude::*;
use rbvg_voxel::persistence::{self, LoadPolicy, PersistError};
use rbvg_voxel::{BlockRegistry, TerrainGenerator, VoxelWorld};

use crate::DataPaths;
use crate::interaction::{BlockChanged, CreativeMenu, Hotbar, InteractionSystems, Target};
use crate::player::{EYE_HEIGHT, Player};
use crate::scripting::{InteractRequest, ScriptingSystems, Scripts};
use crate::voxel::{Blocks, ChunkEntities, GameWorld, Pipeline};

/// Installed content only; missing definitions belong to the current world's snapshot.
#[derive(Resource)]
pub struct ContentBlocks(pub Arc<BlockRegistry>);

pub struct SavePlugin;

impl Plugin for SavePlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(
            Update,
            save_or_load
                .after(InteractionSystems)
                .after(ScriptingSystems),
        );
    }
}

fn new_world(registry: &BlockRegistry, seed: u32) -> Result<VoxelWorld, String> {
    let generator = TerrainGenerator::new(seed, registry).map_err(|e| e.to_string())?;
    Ok(VoxelWorld::new(seed, std::sync::Arc::new(generator)))
}

/// Loads the save in `dir`, or creates a new world with `seed` if there is none.
/// Returns the world and the saved player feet position.
pub fn load_or_create(
    dir: &Path,
    registry: &mut BlockRegistry,
    seed: u32,
) -> Result<(VoxelWorld, Option<[f32; 3]>), String> {
    let absent = match std::fs::read_dir(dir) {
        Ok(mut entries) => entries
            .next()
            .transpose()
            .map_err(|e| format!("cannot inspect save {}: {e}", dir.display()))?
            .is_none(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => true,
        Err(e) => return Err(format!("cannot inspect save {}: {e}", dir.display())),
    };
    if absent {
        info!(
            "no save at {}, generating world with seed {seed}",
            dir.display()
        );
        return Ok((new_world(registry, seed)?, None));
    }
    match load(dir, registry, LoadPolicy::PreserveMissing) {
        Ok((staged, world, feet)) => {
            *registry = staged;
            info!("loaded world from {}", dir.display());
            Ok((world, feet))
        }
        // Refuse to start rather than risk overwriting a save we could not read.
        Err(e) => Err(format!(
            "cannot load save {}: {e} (move it away to start a new world)",
            dir.display()
        )),
    }
}

fn load(
    dir: &Path,
    content: &BlockRegistry,
    policy: LoadPolicy,
) -> Result<(BlockRegistry, VoxelWorld, Option<[f32; 3]>), PersistError> {
    let meta = persistence::load_meta(dir)?;
    let mut staged = content.clone();
    let chunks = persistence::load_chunks_with_policy(dir, &mut staged, policy)?;
    let (world, feet) = build(&staged, meta, chunks)?;
    Ok((staged, world, feet))
}

fn build(
    registry: &BlockRegistry,
    meta: persistence::WorldMeta,
    chunks: Vec<(rbvg_voxel::coords::ChunkPos, rbvg_voxel::chunk::Chunk)>,
) -> Result<(VoxelWorld, Option<[f32; 3]>), PersistError> {
    let generator = TerrainGenerator::new(meta.seed, registry)
        .map_err(|e| PersistError::UnknownBlock(e.to_string()))?;
    let mut world = VoxelWorld::new(meta.seed, std::sync::Arc::new(generator));
    for (pos, chunk) in chunks {
        world.insert_saved(pos, chunk);
    }
    Ok((world, meta.player_position))
}

#[allow(clippy::too_many_arguments)]
fn save_or_load(
    mut commands: Commands,
    keys: Res<ButtonInput<KeyCode>>,
    paths: Res<DataPaths>,
    content: Res<ContentBlocks>,
    mut blocks: ResMut<Blocks>,
    mut world: ResMut<GameWorld>,
    mut entities: ResMut<ChunkEntities>,
    mut pipeline: ResMut<Pipeline>,
    mut scripts: ResMut<Scripts>,
    mut changes: ResMut<Messages<BlockChanged>>,
    mut interactions: ResMut<Messages<InteractRequest>>,
    mut target: ResMut<Target>,
    mut hotbar: ResMut<Hotbar>,
    mut menu: ResMut<CreativeMenu>,
    player: Single<(&mut Transform, &mut Player)>,
) {
    let (mut transform, mut player) = player.into_inner();
    if keys.just_pressed(KeyCode::F5) {
        let feet = Player::feet(transform.translation).to_array();
        match persistence::save_world(&paths.save_dir, &world.0, &blocks.0, Some(feet)) {
            Ok(()) => info!("saved world to {}", paths.save_dir.display()),
            Err(e) => error!("save failed: {e}"),
        }
    } else if keys.just_pressed(KeyCode::F9) {
        match load(&paths.save_dir, &content.0, LoadPolicy::PreserveMissing) {
            Ok((registry, loaded, feet)) => {
                hotbar.slots = hotbar.slots.map(|id| {
                    registry
                        .id(blocks.0.name(id))
                        .unwrap_or(rbvg_voxel::BlockId::AIR)
                });
                menu.index = 0;
                world.0 = loaded;
                blocks.0 = Arc::new(registry);
                // In-flight chunk work and scheduled script callbacks belong to the old
                // world; late results are rejected by ticket.
                pipeline.0.reset();
                scripts.0.clear_scheduled();
                changes.clear();
                interactions.clear();
                target.0 = None;
                for (_, e) in entities.0.drain() {
                    commands.entity(e).despawn();
                }
                if let Some(feet) = feet {
                    transform.translation = Vec3::from_array(feet) + Vec3::Y * EYE_HEIGHT;
                }
                player.velocity = Vec3::ZERO;
                player.on_ground = false;
                info!("reloaded world from {}", paths.save_dir.display());
            }
            Err(e) => error!("load failed: {e}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rbvg_scripting::{ModManifest, ScriptHost, ScriptHostConfig};
    use rbvg_voxel::streaming::{ChunkPipeline, StreamConfig};
    use rbvg_voxel::{BlockId, BlockPos, ChunkPos};

    const MOD_BLOCK: &str = r#"
            format_version = 1
            [[blocks]]
            id = "example:glass"
            display_name = "Glass"
            solid = false
            color = [0.2, 0.4, 0.6]
        "#;
    const POS: BlockPos = BlockPos::new(2, 70, 2);

    fn test_dir(name: &str) -> std::path::PathBuf {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/test-data")
            .join(format!("client_save_{name}_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn content() -> BlockRegistry {
        let mut registry = BlockRegistry::default();
        registry
            .load_toml("core", include_str!("../../../content/core/blocks.toml"))
            .unwrap();
        registry
    }

    fn save_mod_world(dir: &Path) {
        let mut registry = content();
        registry.load_toml("example", MOD_BLOCK).unwrap();
        let mut world = new_world(&registry, 7).unwrap();
        world.load_chunk(POS.chunk());
        world
            .set_block(POS, registry.id("example:glass").unwrap())
            .unwrap();
        persistence::save_world(dir, &world, &registry, Some([1.0, 80.0, 3.0])).unwrap();
    }

    fn reload_app(dir: &Path, registry: BlockRegistry, world: VoxelWorld) -> App {
        let mut scripts = ScriptHost::new(ScriptHostConfig::default());
        let manifest =
            ModManifest::load(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../../mods/regrowth"))
                .unwrap();
        scripts
            .load_mod_source(&manifest, "scheduler.after(60, function() end)")
            .unwrap();
        assert_eq!(scripts.scheduled_count(), 1);
        let mut keys = ButtonInput::<KeyCode>::default();
        keys.press(KeyCode::F9);
        let hotbar = Hotbar(rbvg_voxel::selection::BlockSelection::new(&registry));
        let mut app = App::new();
        app.insert_resource(DataPaths {
            save_dir: dir.to_owned(),
        })
        .insert_resource(ContentBlocks(Arc::new(content())))
        .insert_resource(Blocks(Arc::new(registry)))
        .insert_resource(hotbar)
        .insert_resource(CreativeMenu {
            open: true,
            index: 20,
        })
        .insert_resource(GameWorld(world))
        .insert_resource(Pipeline(ChunkPipeline::new(StreamConfig::default())))
        .insert_resource(ChunkEntities::default())
        .insert_resource(Scripts(scripts))
        .insert_resource(keys)
        .insert_resource(Target(Some(rbvg_voxel::raycast::RayHit {
            block: POS,
            normal: [0, 1, 0],
            distance: 1.0,
        })))
        .add_message::<BlockChanged>()
        .add_message::<InteractRequest>()
        .add_plugins(SavePlugin);
        app.world_mut().spawn((
            Transform::from_xyz(30.0, 90.0, 30.0),
            Player {
                velocity: Vec3::ONE,
                on_ground: true,
            },
        ));
        app.world_mut()
            .resource_mut::<Messages<BlockChanged>>()
            .write(BlockChanged {
                pos: POS,
                old: BlockId::AIR,
                new: BlockId(1),
            });
        app.world_mut()
            .resource_mut::<Messages<InteractRequest>>()
            .write(InteractRequest {
                pos: POS,
                block: BlockId(1),
            });
        app
    }

    #[test]
    fn startup_and_f9_preserve_missing_and_drop_old_world_work() {
        let dir = test_dir("reload");
        save_mod_world(&dir);
        let mut registry = content();
        let (mut world, feet) = load_or_create(&dir, &mut registry, 99).unwrap();
        assert_eq!(feet, Some([1.0, 80.0, 3.0]));
        assert_eq!(world.seed(), 7);
        let missing = registry.id("example:glass").unwrap();
        assert!(registry.get(missing).unwrap().missing);
        world.load_chunk(POS.chunk());
        assert_eq!(world.get_block(POS), missing);
        registry.register_missing("old:stale").unwrap();
        let mut app = reload_app(&dir, registry, world);
        let mut pipeline = app.world_mut().remove_resource::<Pipeline>().unwrap();
        let late = pipeline
            .0
            .update(
                &mut app.world_mut().resource_mut::<GameWorld>().0,
                POS.chunk(),
            )
            .into_iter()
            .next()
            .unwrap()
            .run();
        app.insert_resource(pipeline);
        let entity = app.world_mut().spawn_empty().id();
        app.world_mut()
            .resource_mut::<ChunkEntities>()
            .0
            .insert(POS.chunk(), entity);
        app.update();

        let blocks = &app.world().resource::<Blocks>().0;
        assert!(
            blocks
                .get(blocks.id("example:glass").unwrap())
                .unwrap()
                .missing
        );
        assert!(blocks.id("old:stale").is_none());
        assert!(app.world().resource::<ChunkEntities>().0.is_empty());
        assert!(app.world().get_entity(entity).is_err());
        assert!(app.world().resource::<Target>().0.is_none());
        assert!(app.world().resource::<Messages<BlockChanged>>().is_empty());
        assert!(
            app.world()
                .resource::<Messages<InteractRequest>>()
                .is_empty()
        );
        assert_eq!(app.world().resource::<Scripts>().0.scheduled_count(), 0);
        assert_eq!(app.world().resource::<CreativeMenu>().index, 0);
        let mut players = app.world_mut().query::<(&Transform, &Player)>();
        let (transform, player) = players.single(app.world()).unwrap();
        assert_eq!(
            transform.translation,
            Vec3::new(1.0, 80.0 + EYE_HEIGHT, 3.0)
        );
        assert_eq!(player.velocity, Vec3::ZERO);
        assert!(!player.on_ground);
        let mut pipeline = app.world_mut().remove_resource::<Pipeline>().unwrap();
        assert!(
            !pipeline
                .0
                .finish_generation(&mut app.world_mut().resource_mut::<GameWorld>().0, late)
        );
        app.insert_resource(pipeline);

        let clean = new_world(&content(), 99).unwrap();
        persistence::save_world(&dir, &clean, &content(), None).unwrap();
        let mut keys = app.world_mut().resource_mut::<ButtonInput<KeyCode>>();
        keys.reset_all();
        keys.press(KeyCode::F9);
        app.update();
        assert_eq!(app.world().resource::<GameWorld>().0.seed(), 99);
        assert!(
            !app.world()
                .resource::<Blocks>()
                .0
                .iter()
                .any(|(_, def)| def.missing)
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn failed_f9_leaves_world_registry_and_pending_work_untouched() {
        let dir = test_dir("failed_reload");
        save_mod_world(&dir);
        let chunks = std::fs::read(dir.join("chunks.bin")).unwrap();
        std::fs::write(dir.join("chunks.bin"), &chunks[..chunks.len() - 1]).unwrap();
        let mut registry = content();
        registry.register_missing("old:kept").unwrap();
        let mut world = new_world(&registry, 99).unwrap();
        world.load_chunk(POS.chunk());
        let stone = registry.id("core:stone").unwrap();
        world.set_block(POS, stone).unwrap();
        let revision = world.revision(POS.chunk());
        let mut app = reload_app(&dir, registry, world);
        let before_registry = app.world().resource::<Blocks>().0.clone();
        let before_slots = app.world().resource::<Hotbar>().slots;
        let mut pipeline = app.world_mut().remove_resource::<Pipeline>().unwrap();
        let job = pipeline
            .0
            .update(
                &mut app.world_mut().resource_mut::<GameWorld>().0,
                ChunkPos::new(0, 4, 0),
            )
            .into_iter()
            .next()
            .unwrap();
        app.insert_resource(pipeline);
        app.update();
        assert!(Arc::ptr_eq(
            &before_registry,
            &app.world().resource::<Blocks>().0
        ));
        let world = &app.world().resource::<GameWorld>().0;
        assert_eq!(world.seed(), 99);
        assert_eq!(world.revision(POS.chunk()), revision);
        assert_eq!(world.get_block(POS), stone);
        assert!(
            app.world()
                .resource::<Pipeline>()
                .0
                .is_current_generation(job.pos, job.ticket)
        );
        assert_eq!(app.world().resource::<Scripts>().0.scheduled_count(), 1);
        assert!(app.world().resource::<Target>().0.is_some());
        assert_eq!(app.world().resource::<Messages<BlockChanged>>().len(), 1);
        assert_eq!(app.world().resource::<Messages<InteractRequest>>().len(), 1);
        assert_eq!(app.world().resource::<CreativeMenu>().index, 20);
        assert_eq!(app.world().resource::<Hotbar>().slots, before_slots);
        let mut players = app.world_mut().query::<(&Transform, &Player)>();
        let (transform, player) = players.single(app.world()).unwrap();
        assert_eq!(transform.translation, Vec3::new(30.0, 90.0, 30.0));
        assert_eq!(player.velocity, Vec3::ONE);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn f9_remaps_hotbar_names_and_preserves_the_selected_slot() {
        let dir = test_dir("hotbar");
        save_mod_world(&dir);
        let mut old_registry = BlockRegistry::default();
        old_registry.load_toml("example", MOD_BLOCK).unwrap();
        old_registry
            .load_toml("core", include_str!("../../../content/core/blocks.toml"))
            .unwrap();
        let stone = old_registry.id("core:stone").unwrap();
        let wood = old_registry.id("core:wood").unwrap();
        let stale = old_registry.register_missing("old:stale").unwrap();
        assert_ne!(stone, content().id("core:stone").unwrap());
        let world = new_world(&old_registry, 99).unwrap();
        let mut app = reload_app(&dir, old_registry, world);
        {
            let mut hotbar = app.world_mut().resource_mut::<Hotbar>();
            hotbar.slots[0] = stone;
            hotbar.slots[1] = wood;
            hotbar.slots[2] = stale;
            hotbar.slots[3] = BlockId(u16::MAX);
            hotbar.selected = 7;
        }
        app.update();
        let registry = &app.world().resource::<Blocks>().0;
        let hotbar = app.world().resource::<Hotbar>();
        assert_eq!(hotbar.slots[0], registry.id("core:stone").unwrap());
        assert_eq!(hotbar.slots[1], registry.id("core:wood").unwrap());
        assert_eq!(hotbar.slots[2], BlockId::AIR);
        assert_eq!(hotbar.slots[3], BlockId::AIR);
        assert_eq!(hotbar.selected, 7);
        assert_eq!(app.world().resource::<CreativeMenu>().index, 0);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn f5_includes_same_frame_scheduled_script_edits() {
        let dir = test_dir("script_order");
        let registry = content();
        let wood = registry.id("core:wood").unwrap();
        let mut world = new_world(&registry, 99).unwrap();
        world.load_chunk(POS.chunk());
        let mut app = reload_app(&dir, registry, world);
        let manifest =
            ModManifest::load(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../../mods/regrowth"))
                .unwrap();
        let mut scripts = ScriptHost::new(ScriptHostConfig::default());
        scripts
            .load_mod_source(
                &manifest,
                "scheduler.after(0, function() world.set_block({ x = 2, y = 70, z = 2 }, 'core:wood') end)",
            )
            .unwrap();
        app.insert_resource(Scripts(scripts))
            .insert_resource(Time::<()>::default())
            .add_plugins(crate::scripting::ScriptingPlugin);
        {
            let mut keys = app.world_mut().resource_mut::<ButtonInput<KeyCode>>();
            keys.reset_all();
            keys.press(KeyCode::F5);
        }
        app.update();
        assert_eq!(app.world().resource::<GameWorld>().0.get_block(POS), wood);
        let chunks = persistence::load_chunks(&dir, &app.world().resource::<Blocks>().0).unwrap();
        let chunk = &chunks
            .iter()
            .find(|(pos, _)| *pos == POS.chunk())
            .unwrap()
            .1;
        assert_eq!(chunk.get(POS.local()), wood);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn startup_refuses_partial_or_invalid_saves_without_registering_placeholders() {
        let dir = test_dir("refusal");
        save_mod_world(&dir);
        let valid_meta = std::fs::read(dir.join("world.toml")).unwrap();
        let valid_chunks = std::fs::read(dir.join("chunks.bin")).unwrap();
        let mut registry = content();
        let before = registry.len();
        std::fs::remove_file(dir.join("chunks.bin")).unwrap();
        assert!(load_or_create(&dir, &mut registry, 99).is_err());
        std::fs::write(dir.join("chunks.bin"), &valid_chunks).unwrap();
        std::fs::remove_file(dir.join("world.toml")).unwrap();
        assert!(load_or_create(&dir, &mut registry, 99).is_err());
        for meta in [
            "not TOML",
            "format_version = 2\nseed = 7",
            "format_version = 1\nseed = 7\nplayer_position = [nan, 0.0, 0.0]",
        ] {
            std::fs::write(dir.join("world.toml"), meta).unwrap();
            assert!(load_or_create(&dir, &mut registry, 99).is_err());
        }
        std::fs::write(dir.join("world.toml"), valid_meta).unwrap();
        std::fs::write(
            dir.join("chunks.bin"),
            &valid_chunks[..valid_chunks.len() - 1],
        )
        .unwrap();
        assert!(load_or_create(&dir, &mut registry, 99).is_err());
        assert_eq!(registry.len(), before);
        assert!(registry.id("example:glass").is_none());

        // Even a failure building terrain after a valid decode must not publish placeholders.
        std::fs::write(dir.join("chunks.bin"), valid_chunks).unwrap();
        let mut no_terrain = BlockRegistry::default();
        assert!(load_or_create(&dir, &mut no_terrain, 99).is_err());
        assert_eq!(no_terrain.len(), 1);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn startup_creates_only_for_absent_or_empty_save_slots() {
        let dir = test_dir("empty");
        let mut registry = content();
        let absent = dir.join("absent");
        for slot in [&dir, &absent] {
            let (world, feet) = load_or_create(slot, &mut registry, 99).unwrap();
            assert_eq!(world.seed(), 99);
            assert_eq!(feet, None);
        }
        std::fs::write(dir.join("chunks.tmp"), b"interrupted save").unwrap();
        assert!(load_or_create(&dir, &mut registry, 99).is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
