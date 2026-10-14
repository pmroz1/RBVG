//! Bridges player block edits to Luau mods, ticks the script scheduler, and applies the
//! commands scripts return.
//!
//! Timing contract: events from frame N are dispatched in frame N after the edit is
//! applied; then the scheduler advances by the frame's virtual-time delta (zero while
//! paused) and runs due callbacks. Commands are applied right after each dispatch/tick,
//! in the same frame, and are visible to every later system. Scripts read the world as
//! it was when the dispatch/tick started. Script-made edits raise `block_placed`/`block_broken`
//! events with origin `mod`; they are dispatched in later waves by `rbvg_scripting::pump`,
//! bounded by depth and event limits (see docs/SCRIPTING_API.md).

use std::time::Duration;

use bevy::prelude::*;
use rbvg_scripting::{
    CommandApplier, Envelope, EventOrigin, PumpLimits, ScriptCommand, ScriptEvent, ScriptHost,
    WorldQuery, pump,
};
use rbvg_voxel::BlockId;
use rbvg_voxel::BlockPos;
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

/// A script asked to show this message (`ui.notify`).
#[derive(Message, Debug, Clone)]
pub struct Notification(pub String);

/// The player pressed the interact key while targeting `pos`.
#[derive(Message, Debug, Clone, Copy)]
pub struct InteractRequest {
    pub pos: BlockPos,
}

pub struct ScriptingPlugin;

impl Plugin for ScriptingPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<ScriptStats>()
            .add_message::<Notification>()
            .add_message::<InteractRequest>()
            .add_systems(
                Update,
                (dispatch_player_events, tick_scheduler)
                    .chain()
                    .after(crate::interaction::InteractionSystems),
            );
    }
}

/// Engine side of the event pump: validates and applies script commands.
pub struct ClientApplier<'a> {
    pub blocks: &'a Blocks,
    pub world: &'a mut GameWorld,
    pub player: Aabb,
    pub notes: Vec<String>,
}

impl WorldQuery for ClientApplier<'_> {
    fn block(&self, pos: BlockPos) -> Option<&str> {
        self.world
            .0
            .block_if_loaded(pos)
            .map(|id| self.blocks.0.name(id))
    }
}

impl CommandApplier for ClientApplier<'_> {
    fn query(&self) -> &dyn WorldQuery {
        self
    }

    fn apply(&mut self, cmd: ScriptCommand) -> Option<ScriptEvent> {
        match cmd {
            ScriptCommand::Notify { message, .. } => {
                self.notes.push(message);
                None
            }
            ScriptCommand::SetBlock { mod_id, pos, block } => {
                let Some(id) = self.blocks.0.id(&block) else {
                    warn!(mod_id, "world.set_block: unknown block `{block}`");
                    return None;
                };
                // Same rule as player placement: never put a solid block inside the player.
                if self.blocks.0.is_solid(id) && self.player.intersects(|p| p == pos) {
                    warn!(mod_id, "world.set_block: {pos:?} is occupied by the player");
                    return None;
                }
                match self.world.0.set_block(pos, id) {
                    // No-op edits raise no event.
                    Ok(old) if old == id => None,
                    Ok(old) => Some(if id == BlockId::AIR {
                        ScriptEvent::BlockBroken {
                            pos,
                            block: self.blocks.0.name(old).to_owned(),
                        }
                    } else {
                        ScriptEvent::BlockPlaced {
                            pos,
                            block: block.clone(),
                        }
                    }),
                    Err(e) => {
                        warn!(mod_id, "world.set_block: {e}");
                        None
                    }
                }
            }
        }
    }
}

/// Runs `events`/`commands` through the bounded pump and returns the notifications.
#[allow(clippy::too_many_arguments)]
pub fn run_pump(
    scripts: &Scripts,
    blocks: &Blocks,
    world: &mut GameWorld,
    player: Aabb,
    stats: &mut ScriptStats,
    events: Vec<Envelope>,
    commands: Vec<ScriptCommand>,
) -> Vec<Notification> {
    let mut applier = ClientApplier {
        blocks,
        world,
        player,
        notes: Vec::new(),
    };
    let started = std::time::Instant::now();
    let s = pump(
        &scripts.0,
        &mut applier,
        events,
        commands,
        PumpLimits::default(),
    );
    stats.last_dispatch = started.elapsed();
    stats.errors += s.errors;
    applier.notes.into_iter().map(Notification).collect()
}

fn dispatch_player_events(
    mut changes: MessageReader<BlockChanged>,
    mut interactions: MessageReader<InteractRequest>,
    scripts: Res<Scripts>,
    blocks: Res<Blocks>,
    mut world: ResMut<GameWorld>,
    mut stats: ResMut<ScriptStats>,
    mut notes: MessageWriter<Notification>,
    player: Single<&Transform, With<Player>>,
) {
    let mut events = Vec::new();
    for change in changes.read() {
        events.push(Envelope::player(if change.new == BlockId::AIR {
            ScriptEvent::BlockBroken {
                pos: change.pos,
                block: blocks.0.name(change.old).to_owned(),
            }
        } else {
            ScriptEvent::BlockPlaced {
                pos: change.pos,
                block: blocks.0.name(change.new).to_owned(),
            }
        }));
    }
    for i in interactions.read() {
        // Unloaded chunks yield no event rather than being force-loaded.
        if let Some(id) = world.0.block_if_loaded(i.pos) {
            events.push(Envelope {
                event: ScriptEvent::Interact {
                    pos: i.pos,
                    block: blocks.0.name(id).to_owned(),
                },
                origin: EventOrigin::Player,
                depth: 0,
            });
        }
    }
    if events.is_empty() {
        return;
    }
    let aabb = Player::aabb(player.translation);
    for n in run_pump(
        &scripts,
        &blocks,
        &mut world,
        aabb,
        &mut stats,
        events,
        vec![],
    ) {
        notes.write(n);
    }
}

fn tick_scheduler(
    time: Res<Time>,
    mut scripts: ResMut<Scripts>,
    blocks: Res<Blocks>,
    mut world: ResMut<GameWorld>,
    mut stats: ResMut<ScriptStats>,
    mut notes: MessageWriter<Notification>,
    player: Single<&Transform, With<Player>>,
) {
    let (commands, s) = {
        let applier = ClientApplier {
            blocks: &blocks,
            world: &mut world,
            player: Player::aabb(player.translation),
            notes: Vec::new(),
        };
        scripts.0.tick(time.delta(), applier.query())
    };
    stats.last_tick = s.duration;
    stats.errors += s.errors;
    stats.scheduled = scripts.0.scheduled_count();
    if commands.is_empty() {
        return;
    }
    let aabb = Player::aabb(player.translation);
    for n in run_pump(
        &scripts,
        &blocks,
        &mut world,
        aabb,
        &mut stats,
        vec![],
        commands,
    ) {
        notes.write(n);
    }
}
