//! Block targeting, breaking, placing and the hotbar.

use bevy::prelude::*;
use bevy::window::CursorOptions;
use rbvg_voxel::raycast::{RayHit, raycast};
use rbvg_voxel::{BlockId, BlockPos};

use crate::player::{Player, cursor_captured};
use crate::scripting::{InteractRequest, Notification, ScriptStats, Scripts, run_pump};
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
#[derive(Resource)]
pub struct Hotbar {
    pub slots: Vec<BlockId>,
    pub selected: usize,
}

impl Hotbar {
    pub fn current(&self) -> Option<BlockId> {
        self.slots.get(self.selected).copied()
    }
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
            .add_systems(Startup, init_hotbar)
            .add_systems(
                Update,
                (
                    select_slot,
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
    let slots = blocks
        .0
        .iter()
        .filter(|(_, d)| d.solid)
        .map(|(id, _)| id)
        .take(9)
        .collect();
    commands.insert_resource(Hotbar { slots, selected: 0 });
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
    mut requests: MessageWriter<InteractRequest>,
) {
    if cursor_captured(&cursor)
        && keys.just_pressed(KeyCode::KeyE)
        && let Some(hit) = target.0
    {
        requests.write(InteractRequest { pos: hit.block });
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
