//! Bridges player block edits to Luau mods and applies the commands they return.
//!
//! Timing contract: events from frame N are dispatched in frame N after the edit is
//! applied; commands queued by scripts are applied immediately after dispatch, in the
//! same frame, and are visible to every later system. Script-made edits do not raise
//! further script events (prevents feedback loops).

use std::time::Duration;

use bevy::prelude::*;
use rbvg_scripting::{ScriptCommand, ScriptEvent, ScriptHost};
use rbvg_voxel::BlockId;

use crate::interaction::BlockChanged;
use crate::voxel::{Blocks, GameWorld};

#[derive(Resource)]
pub struct Scripts(pub ScriptHost);

#[derive(Resource, Default)]
pub struct ScriptStats {
    pub last_dispatch: Duration,
    pub errors: usize,
}

pub struct ScriptingPlugin;

impl Plugin for ScriptingPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<ScriptStats>().add_systems(
            Update,
            dispatch_block_events.after(crate::interaction::InteractionSystems),
        );
    }
}

fn dispatch_block_events(
    mut changes: MessageReader<BlockChanged>,
    scripts: Res<Scripts>,
    blocks: Res<Blocks>,
    mut world: ResMut<GameWorld>,
    mut stats: ResMut<ScriptStats>,
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
        let (commands, s) = scripts.0.dispatch(&event);
        stats.last_dispatch = s.duration;
        stats.errors += s.errors;
        for cmd in commands {
            apply(cmd, &blocks, &mut world);
        }
    }
}

/// Validates and applies one script command. Invalid commands are logged and dropped.
fn apply(cmd: ScriptCommand, blocks: &Blocks, world: &mut GameWorld) {
    match cmd {
        ScriptCommand::SetBlock { mod_id, pos, block } => {
            let Some(id) = blocks.0.id(&block) else {
                warn!(mod_id, "world.set_block: unknown block `{block}`");
                return;
            };
            if let Err(e) = world.0.set_block(pos, id) {
                warn!(mod_id, "world.set_block: {e}");
            }
        }
    }
}
