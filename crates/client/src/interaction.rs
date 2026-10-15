//! Block targeting, breaking, placing and the hotbar.

use bevy::prelude::*;
use bevy::window::CursorOptions;
use rbvg_voxel::raycast::{RayHit, raycast};
use rbvg_voxel::selection::{BlockSelection, catalog};
use rbvg_voxel::{BlockId, BlockPos};

use crate::player::{Player, cursor_captured};
use crate::scripting::{
    DeferredEvents, InteractRequest, Notification, ScriptStats, Scripts, run_pump,
};
use crate::voxel::{Blocks, GameWorld};
use rbvg_scripting::{ScriptEvent, VoxelQuery};

/// A block was changed by a player. Emitted after the world was updated.
#[derive(Message, Debug, Clone, Copy)]
pub struct BlockChanged {
    pub pos: BlockPos,
    pub old: BlockId,
    pub new: BlockId,
}

#[derive(Resource)]
pub struct InteractionSettings {
    /// Maximum reach in blocks.
    pub reach: f32,
}

/// Selectable blocks and the current selection.
#[derive(Resource, Deref, DerefMut)]
pub struct Hotbar(pub BlockSelection);

#[derive(Resource, Default)]
pub struct CreativeMenu {
    pub open: bool,
    pub index: usize,
}

/// The block under the crosshair this frame.
#[derive(Resource, Default)]
pub struct Target(pub Option<RayHit>);

pub struct InteractionPlugin;

/// System set containing block edits; consumers of [`BlockChanged`] run after it.
#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub struct InteractionSystems;

impl Plugin for InteractionPlugin {
    fn build(&self, app: &mut App) {
        app.add_message::<BlockChanged>()
            .insert_resource(InteractionSettings { reach: 6.0 })
            .init_resource::<Target>()
            .init_resource::<CreativeMenu>()
            .add_systems(Startup, init_hotbar)
            .add_systems(
                Update,
                (
                    select_slot,
                    creative_input,
                    update_target,
                    interact,
                    edit_blocks,
                    draw_target,
                )
                    .chain()
                    .in_set(InteractionSystems),
            );
    }
}

fn init_hotbar(mut commands: Commands, blocks: Res<Blocks>) {
    commands.insert_resource(Hotbar(BlockSelection::new(&blocks.0)));
}

const SLOT_KEYS: [KeyCode; 9] = [
    KeyCode::Digit1,
    KeyCode::Digit2,
    KeyCode::Digit3,
    KeyCode::Digit4,
    KeyCode::Digit5,
    KeyCode::Digit6,
    KeyCode::Digit7,
    KeyCode::Digit8,
    KeyCode::Digit9,
];

fn select_slot(keys: Res<ButtonInput<KeyCode>>, mut hotbar: ResMut<Hotbar>) {
    if let Some(i) = SLOT_KEYS.iter().position(|k| keys.just_pressed(*k))
        && i < hotbar.slots.len()
    {
        hotbar.selected = i;
    }
}

fn creative_input(
    keys: Res<ButtonInput<KeyCode>>,
    mut menu: ResMut<CreativeMenu>,
    mut hotbar: ResMut<Hotbar>,
    blocks: Res<Blocks>,
    mut cursor: Single<&mut CursorOptions>,
) {
    if keys.just_pressed(KeyCode::KeyB) {
        menu.open = !menu.open;
        cursor.visible = true;
        cursor.grab_mode = bevy::window::CursorGrabMode::None;
    }
    if keys.just_pressed(KeyCode::Escape) {
        menu.open = false;
    }
    if !menu.open {
        return;
    }
    if !blocks.is_changed()
        && ![
            KeyCode::ArrowDown,
            KeyCode::ArrowUp,
            KeyCode::PageDown,
            KeyCode::PageUp,
            KeyCode::Enter,
        ]
        .iter()
        .any(|key| keys.just_pressed(*key))
    {
        return;
    }
    let entries = catalog(&blocks.0);
    if entries.is_empty() {
        menu.index = 0;
        return;
    }
    let last = entries.len() - 1;
    menu.index = menu.index.min(last);
    if keys.just_pressed(KeyCode::ArrowDown) {
        menu.index = (menu.index + 1).min(last);
    }
    if keys.just_pressed(KeyCode::ArrowUp) {
        menu.index = menu.index.saturating_sub(1);
    }
    if keys.just_pressed(KeyCode::PageDown) {
        menu.index = (menu.index + 12).min(last);
    }
    if keys.just_pressed(KeyCode::PageUp) {
        menu.index = menu.index.saturating_sub(12);
    }
    if keys.just_pressed(KeyCode::Enter) {
        let slot = hotbar.selected;
        hotbar.assign(slot, entries[menu.index].0, &blocks.0);
    }
}

fn update_target(
    mut target: ResMut<Target>,
    settings: Res<InteractionSettings>,
    world: Res<GameWorld>,
    blocks: Res<Blocks>,
    camera: Single<&Transform, With<Player>>,
) {
    target.0 = raycast(
        camera.translation.to_array(),
        camera.forward().to_array(),
        settings.reach,
        |p| blocks.0.is_solid(world.0.get_block(p)),
    );
}

/// E: tell scripts the player interacted with the targeted block. The engine attaches no
/// meaning to it.
fn interact(
    keys: Res<ButtonInput<KeyCode>>,
    cursor: Single<Ref<CursorOptions>>,
    target: Res<Target>,
    world: Res<GameWorld>,
    mut requests: MessageWriter<InteractRequest>,
) {
    if cursor_captured(&cursor)
        && keys.just_pressed(KeyCode::KeyE)
        && let Some(hit) = target.0
        && let Some(block) = world.0.block_if_loaded(hit.block)
    {
        requests.write(InteractRequest {
            pos: hit.block,
            block,
        });
    }
}

#[allow(clippy::too_many_arguments)]
fn edit_blocks(
    mouse: Res<ButtonInput<MouseButton>>,
    cursor: Single<Ref<CursorOptions>>,
    target: Res<Target>,
    hotbar: Res<Hotbar>,
    blocks: Res<Blocks>,
    mut world: ResMut<GameWorld>,
    camera: Single<&Transform, With<Player>>,
    mut changed: MessageWriter<BlockChanged>,
    scripts: Res<Scripts>,
    mut stats: ResMut<ScriptStats>,
    mut deferred: ResMut<DeferredEvents>,
    mut notes: MessageWriter<Notification>,
) {
    // Skip while uncaptured, and on the frame the capturing click happens.
    if !cursor_captured(&cursor) || cursor.is_changed() {
        return;
    }
    let Some(hit) = target.0 else { return };
    let (pos, new) = if mouse.just_pressed(MouseButton::Left) {
        (hit.block, BlockId::AIR)
    } else if mouse.just_pressed(MouseButton::Right) {
        let Some(block) = hotbar.current() else {
            return;
        };
        if blocks
            .0
            .get(block)
            .is_none_or(|def| def.missing || !def.solid)
        {
            return;
        }
        let pos = hit.adjacent();
        // Never place a solid block inside the player.
        if blocks.0.is_solid(block) && Player::aabb(camera.translation).intersects(|p| p == pos) {
            return;
        }
        (pos, block)
    } else {
        return;
    };
    if new == BlockId::AIR {
        // Cancellable: scripts see the block before it is removed.
        let event = ScriptEvent::BeforeBlockBreak {
            pos,
            block: blocks.0.name(world.0.get_block(pos)).to_owned(),
        };
        let (cmds, s) = scripts.0.dispatch(
            &event,
            &VoxelQuery {
                world: &world.0,
                registry: &blocks.0,
            },
        );
        stats.errors += s.errors;
        let aabb = Player::aabb(camera.translation);
        let ns = run_pump(
            &scripts,
            &blocks,
            &mut world,
            aabb,
            &mut stats,
            &mut deferred,
            vec![],
            cmds,
        );
        for n in ns {
            notes.write(n);
        }
        if s.cancelled {
            return;
        }
    }
    match world.0.set_block(pos, new) {
        Ok(old) if old != new => {
            changed.write(BlockChanged { pos, old, new });
        }
        Ok(_) => {}
        Err(e) => warn!("block edit rejected: {e}"),
    }
}

fn draw_target(target: Res<Target>, mut gizmos: Gizmos) {
    if let Some(hit) = target.0 {
        let b = hit.block;
        let center = Vec3::new(b.x as f32, b.y as f32, b.z as f32) + Vec3::splat(0.5);
        gizmos.cube(
            Transform::from_translation(center).with_scale(Vec3::splat(1.005)),
            Color::BLACK,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{path::Path, sync::Arc};

    use crate::scripting::ScriptingPlugin;
    use rbvg_scripting::{ModManifest, ScriptHost, ScriptHostConfig};
    use rbvg_voxel::{TerrainGenerator, VoxelWorld};

    #[test]
    fn creative_keyboard_selects_mod_content_for_any_slot() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let mut registry = crate::load_content(&root).unwrap();
        ModManifest::load(&root.join("mods/ancient_shrine"))
            .unwrap()
            .register_blocks(&mut registry)
            .unwrap();
        let altar = registry.id("ancient_shrine:altar").unwrap();
        for i in 0..20 {
            registry
                .load_toml(
                    "test",
                    &format!(
                        "format_version = 1\n[[blocks]]\nid = \"test:block_{i:02}\"\n\
                         display_name = \"Test {i}\"\ncolor = [0.5, 0.5, 0.5]"
                    ),
                )
                .unwrap();
        }
        let page_two_block = catalog(&registry)[12].0;
        let mut keys = ButtonInput::default();
        keys.press(KeyCode::KeyB);
        let mut app = App::new();
        app.insert_resource(Hotbar(BlockSelection::new(&registry)))
            .insert_resource(Blocks(Arc::new(registry)))
            .insert_resource(keys)
            .init_resource::<CreativeMenu>()
            .add_systems(Update, (select_slot, creative_input).chain());
        let cursor = app
            .world_mut()
            .spawn(CursorOptions {
                grab_mode: bevy::window::CursorGrabMode::Locked,
                ..default()
            })
            .id();
        app.update();
        assert!(app.world().resource::<CreativeMenu>().open);
        assert!(!cursor_captured(
            app.world().get::<CursorOptions>(cursor).unwrap()
        ));
        let mut keys = app.world_mut().resource_mut::<ButtonInput<KeyCode>>();
        keys.reset_all();
        keys.press(KeyCode::Digit9);
        keys.press(KeyCode::Enter);
        app.update();
        let hotbar = app.world().resource::<Hotbar>();
        assert_eq!(hotbar.selected, 8);
        assert_eq!(hotbar.current(), Some(altar));
        let mut keys = app.world_mut().resource_mut::<ButtonInput<KeyCode>>();
        keys.reset_all();
        keys.press(KeyCode::PageDown);
        keys.press(KeyCode::Digit3);
        keys.press(KeyCode::Enter);
        app.update();
        assert_eq!(app.world().resource::<CreativeMenu>().index, 12);
        assert_eq!(
            app.world().resource::<Hotbar>().current(),
            Some(page_two_block)
        );
        assert_eq!(app.world().resource::<Hotbar>().slots[8], altar);
    }

    /// Synthetic mouse input through the real raycast → edit → script pipeline.
    #[test]
    fn mouse_clicks_break_place_and_respect_cancellation() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let mut registry = crate::load_content(&root).unwrap();
        registry
            .load_toml(
                "test",
                "format_version = 1\n[[blocks]]\nid = \"test:glass\"\n\
                 display_name = \"Glass\"\nsolid = false\ncolor = [0.5, 0.5, 0.5]",
            )
            .unwrap();
        let (stone, wood) = (
            registry.id("core:stone").unwrap(),
            registry.id("core:wood").unwrap(),
        );
        let glass = registry.id("test:glass").unwrap();
        let manifest = ModManifest::load(&root.join("mods/ancient_shrine")).unwrap();
        let mut host = ScriptHost::new(ScriptHostConfig::default());
        host.load_mod_source(
            &manifest,
            r#"
            events.subscribe("before_block_break", function(e)
                if e.block == "core:wood" then e.cancel() end
            end)
            events.subscribe("block_broken", function(e) ui.notify("broken " .. e.block) end)
            events.subscribe("block_placed", function(e) ui.notify("placed " .. e.block) end)
            "#,
        )
        .unwrap();
        // Floating in empty sky, looking straight down at one block 4.5 below the eye.
        let ground = BlockPos::new(0, 195, 0);
        let mut world = VoxelWorld::new(7, Arc::new(TerrainGenerator::new(7, &registry).unwrap()));
        world.load_chunk(ground.chunk());
        world.set_block(ground, stone).unwrap();
        let mut hotbar = Hotbar(BlockSelection::new(&registry));
        hotbar.slots[0] = wood;
        hotbar.selected = 0;
        let mut app = App::new();
        app.insert_resource(GameWorld(world))
            .insert_resource(Blocks(Arc::new(registry)))
            .insert_resource(Scripts(host))
            .insert_resource(hotbar)
            .insert_resource(InteractionSettings { reach: 6.0 })
            .init_resource::<Target>()
            .init_resource::<ButtonInput<MouseButton>>()
            .insert_resource(Time::<()>::default())
            .add_message::<BlockChanged>()
            .add_plugins(ScriptingPlugin)
            .add_systems(
                Update,
                (update_target, edit_blocks)
                    .chain()
                    .in_set(InteractionSystems),
            );
        app.world_mut().spawn(CursorOptions {
            grab_mode: bevy::window::CursorGrabMode::Locked,
            ..default()
        });
        app.world_mut().spawn((
            Player::default(),
            Transform::from_xyz(0.5, 199.5, 0.5).looking_to(Vec3::NEG_Y, Vec3::Z),
        ));
        // The frame the cursor gets captured never edits.
        app.update();
        let click = |app: &mut App, button: MouseButton| {
            let mut mouse = app.world_mut().resource_mut::<ButtonInput<MouseButton>>();
            mouse.reset_all();
            mouse.press(button);
            app.update();
            let notes: Vec<String> = app
                .world_mut()
                .resource_mut::<Messages<Notification>>()
                .drain()
                .map(|n| n.0)
                .collect();
            notes
        };
        let above = BlockPos::new(0, 196, 0);
        let block = |app: &App, pos: BlockPos| app.world().resource::<GameWorld>().0.get_block(pos);

        app.world_mut().resource_mut::<Hotbar>().slots[0] = glass;
        assert!(click(&mut app, MouseButton::Right).is_empty());
        assert_eq!(block(&app, above), BlockId::AIR);
        app.world_mut().resource_mut::<Hotbar>().slots[0] = wood;
        assert_eq!(click(&mut app, MouseButton::Right), ["placed core:wood"]);
        assert_eq!(block(&app, above), wood);
        // Wood is protected: the break is cancelled and no notification is raised.
        assert!(click(&mut app, MouseButton::Left).is_empty());
        assert_eq!(block(&app, above), wood);
        app.world_mut()
            .resource_mut::<GameWorld>()
            .0
            .set_block(above, BlockId::AIR)
            .unwrap();
        assert_eq!(click(&mut app, MouseButton::Left), ["broken core:stone"]);
        assert_eq!(block(&app, ground), BlockId::AIR);
        // Nothing within reach any more: clicks are ignored.
        assert!(click(&mut app, MouseButton::Right).is_empty());
        assert!(app.world().resource::<Target>().0.is_none());
    }

    #[test]
    fn interaction_keeps_the_target_block_after_a_same_frame_edit() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let mut registry = crate::load_content(&root).unwrap();
        let manifest = ModManifest::load(&root.join("mods/ancient_shrine")).unwrap();
        manifest.register_blocks(&mut registry).unwrap();
        let mut host = ScriptHost::new(ScriptHostConfig::default());
        host.load_mod_source(
            &manifest,
            r#"events.subscribe("interact", function(e) ui.notify(e.block) end)"#,
        )
        .unwrap();
        let mut world = VoxelWorld::new(7, Arc::new(TerrainGenerator::new(7, &registry).unwrap()));
        let pos = BlockPos::new(0, 64, 0);
        world.load_chunk(pos.chunk());
        world
            .set_block(pos, registry.id("ancient_shrine:altar").unwrap())
            .unwrap();
        let mut keys = ButtonInput::default();
        keys.press(KeyCode::KeyE);
        let mut app = App::new();
        app.insert_resource(GameWorld(world))
            .insert_resource(Blocks(Arc::new(registry)))
            .insert_resource(Scripts(host))
            .insert_resource(keys)
            .insert_resource(Time::<()>::default())
            .insert_resource(Target(Some(RayHit {
                block: pos,
                normal: [0, 1, 0],
                distance: 1.0,
            })))
            .add_message::<BlockChanged>()
            .add_plugins(ScriptingPlugin)
            .add_systems(
                Update,
                (interact, move |mut world: ResMut<GameWorld>| {
                    world.0.set_block(pos, BlockId::AIR).unwrap();
                })
                    .chain()
                    .in_set(InteractionSystems),
            );
        app.world_mut().spawn(CursorOptions {
            grab_mode: bevy::window::CursorGrabMode::Locked,
            ..default()
        });
        app.world_mut()
            .spawn((Player::default(), Transform::default()));
        app.update();
        assert_eq!(
            app.world().resource::<GameWorld>().0.get_block(pos),
            BlockId::AIR
        );
        let notes: Vec<_> = app
            .world_mut()
            .resource_mut::<Messages<Notification>>()
            .drain()
            .map(|n| n.0)
            .collect();
        assert_eq!(notes, ["ancient_shrine:altar"]);
    }

    #[test]
    fn before_break_overflow_waits_until_the_next_frame() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let registry = crate::load_content(&root).unwrap();
        let manifest = ModManifest::load(&root.join("mods/ancient_shrine")).unwrap();
        let mut host = ScriptHost::new(ScriptHostConfig::default());
        host.load_mod_source(
            &manifest,
            r#"
            events.subscribe("before_block_break", function()
                for y = 128, 143 do
                    for z = 0, 15 do
                        world.set_block({ x = 0, y = y, z = z }, "core:stone")
                    end
                end
            end)
            events.subscribe("block_placed", function(e)
                if e.position.x == 0 then
                    world.set_block({ x = 1, y = e.position.y, z = e.position.z }, e.block)
                end
                ui.notify("placed")
            end)
            "#,
        )
        .unwrap();
        let ground = BlockPos::new(0, 195, 0);
        let mut world = VoxelWorld::new(7, Arc::new(TerrainGenerator::new(7, &registry).unwrap()));
        world.load_chunk(ground.chunk());
        world.load_chunk(BlockPos::new(0, 128, 0).chunk());
        world
            .set_block(ground, registry.id("core:stone").unwrap())
            .unwrap();
        let hotbar = Hotbar(BlockSelection::new(&registry));
        let mut app = App::new();
        app.insert_resource(GameWorld(world))
            .insert_resource(Blocks(Arc::new(registry)))
            .insert_resource(Scripts(host))
            .insert_resource(hotbar)
            .insert_resource(InteractionSettings { reach: 6.0 })
            .init_resource::<Target>()
            .init_resource::<ButtonInput<MouseButton>>()
            .insert_resource(Time::<()>::default())
            .add_message::<BlockChanged>()
            .add_plugins(ScriptingPlugin)
            .add_systems(
                Update,
                (update_target, edit_blocks)
                    .chain()
                    .in_set(InteractionSystems),
            );
        app.world_mut().spawn(CursorOptions {
            grab_mode: bevy::window::CursorGrabMode::Locked,
            ..default()
        });
        app.world_mut().spawn((
            Player::default(),
            Transform::from_xyz(0.5, 199.5, 0.5).looking_to(Vec3::NEG_Y, Vec3::Z),
        ));
        app.update();

        let mut mouse = app.world_mut().resource_mut::<ButtonInput<MouseButton>>();
        mouse.reset_all();
        mouse.press(MouseButton::Left);
        app.update();
        assert_eq!(
            app.world().resource::<GameWorld>().0.get_block(ground),
            BlockId::AIR
        );
        let first_frame_notifications = app
            .world_mut()
            .resource_mut::<Messages<Notification>>()
            .drain()
            .count();
        assert_eq!(first_frame_notifications, 256);
        let deferred = &app
            .world()
            .resource::<crate::scripting::DeferredEvents>()
            .pending;
        assert_eq!(deferred.len(), 256);
        assert!(deferred.iter().all(|event| event.depth == 2));
        assert!(matches!(
            deferred.first().map(|event| &event.event),
            Some(ScriptEvent::BlockPlaced { pos, .. })
                if *pos == BlockPos::new(1, 128, 0)
        ));
        assert!(matches!(
            deferred.last().map(|event| &event.event),
            Some(ScriptEvent::BlockPlaced { pos, .. })
                if *pos == BlockPos::new(1, 143, 15)
        ));

        app.world_mut()
            .resource_mut::<ButtonInput<MouseButton>>()
            .reset_all();
        app.update();
        let next_frame_notifications = app
            .world_mut()
            .resource_mut::<Messages<Notification>>()
            .drain()
            .count();
        assert_eq!(next_frame_notifications, 256);
        assert!(
            app.world()
                .resource::<crate::scripting::DeferredEvents>()
                .pending
                .is_empty()
        );
    }
}
