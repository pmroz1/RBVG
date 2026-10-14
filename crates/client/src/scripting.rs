//! Bridges player block edits to Luau mods, ticks the script scheduler, and applies the
//! commands scripts return.
//!
//! Timing contract: events from frame N are dispatched in frame N after the edit is
//! applied; then the scheduler advances by the frame's virtual-time delta (zero while
//! paused) and runs due callbacks. Commands are applied right after each dispatch/tick,
//! in the same frame, and are visible to every later system. Scripts read the world as
//! it was when the dispatch/tick started. Script-made edits do not raise further script
//! events (prevents feedback loops).

use std::time::Duration;

use bevy::prelude::*;
use rbvg_scripting::{ScriptCommand, ScriptEvent, ScriptHost, VoxelQuery};
use rbvg_voxel::BlockId;
use rbvg_voxel::physics::Aabb;

use crate::interaction::BlockChanged;
use crate::player::Player;
use crate::voxel::{Blocks, GameWorld};

#[derive(Resource)]
pub struct Scripts(pub ScriptHost);

#[derive(Resource, Default)]
pub struct ScriptStats {
    pub last_dispatch: Duration,
    pub last_tick: Duration,
    pub scheduled: usize,
    pub errors: usize,
}

pub struct ScriptingPlugin;

impl Plugin for ScriptingPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<ScriptStats>().add_systems(
            Update,
            (dispatch_block_events, tick_scheduler)
                .chain()
                .after(crate::interaction::InteractionSystems),
        );
    }
}

fn dispatch_block_events(
    mut changes: MessageReader<BlockChanged>,
    scripts: Res<Scripts>,
    blocks: Res<Blocks>,
    mut world: ResMut<GameWorld>,
    mut stats: ResMut<ScriptStats>,
    player: Single<&Transform, With<Player>>,
) {
    for change in changes.read() {
        let event = if change.new == BlockId::AIR {
            ScriptEvent::BlockBroken {
                pos: change.pos,
                block: blocks.0.name(change.old).to_owned(),
            }
        } else {
            ScriptEvent::BlockPlaced {
                pos: change.pos,
                block: blocks.0.name(change.new).to_owned(),
            }
        };
        let query = VoxelQuery {
            world: &world.0,
            registry: &blocks.0,
        };
        let (commands, s) = scripts.0.dispatch(&event, &query);
        stats.last_dispatch = s.duration;
        stats.errors += s.errors;
        for cmd in commands {
            apply(cmd, &blocks, &mut world, Player::aabb(player.translation));
        }
    }
}

fn tick_scheduler(
    time: Res<Time>,
    mut scripts: ResMut<Scripts>,
    blocks: Res<Blocks>,
    mut world: ResMut<GameWorld>,
    mut stats: ResMut<ScriptStats>,
    player: Single<&Transform, With<Player>>,
) {
    let query = VoxelQuery {
        world: &world.0,
        registry: &blocks.0,
    };
    let (commands, s) = scripts.0.tick(time.delta(), &query);
    stats.last_tick = s.duration;
    stats.errors += s.errors;
    stats.scheduled = scripts.0.scheduled_count();
    for cmd in commands {
        apply(cmd, &blocks, &mut world, Player::aabb(player.translation));
    }
}

/// Validates and applies one script command. Invalid commands are logged and dropped.
fn apply(cmd: ScriptCommand, blocks: &Blocks, world: &mut GameWorld, player: Aabb) {
    match cmd {
        ScriptCommand::SetBlock { mod_id, pos, block } => {
            let Some(id) = blocks.0.id(&block) else {
                warn!(mod_id, "world.set_block: unknown block `{block}`");
                return;
            };
            // Same rule as player placement: never put a solid block inside the player.
            if blocks.0.is_solid(id) && player.intersects(|p| p == pos) {
                warn!(mod_id, "world.set_block: {pos:?} is occupied by the player");
                return;
            }
            if let Err(e) = world.0.set_block(pos, id) {
                warn!(mod_id, "world.set_block: {e}");
            }
        }
    }
}
