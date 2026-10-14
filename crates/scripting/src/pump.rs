//! Bounded, wave-based event propagation.
//!
//! Timing contract: the commands a dispatch (or scheduler tick) returns are applied in
//! order; every applied command that changed the world yields an event with origin
//! `mod` and `depth + 1`. Those events are *not* run recursively: they are queued and
//! dispatched together in the next wave, after the current wave's commands were all
//! applied, so handlers always see a consistent world. Waves repeat until the queue is
//! empty or a limit is hit; leftovers are dropped and counted (never deferred, so a
//! cascade cannot outlive the frame).

use crate::events::{Envelope, EventOrigin, ScriptCommand, ScriptEvent};
use crate::host::ScriptHost;
use crate::query::WorldQuery;

/// Engine side of the pump: world access plus command application.
pub trait CommandApplier {
    fn query(&self) -> &dyn WorldQuery;
    /// Validates and applies one command. Returns the resulting event, or `None` for
    /// rejected or no-op commands (and commands that are not world changes).
    fn apply(&mut self, cmd: ScriptCommand) -> Option<ScriptEvent>;
}

#[derive(Debug, Clone, Copy)]
pub struct PumpLimits {
    /// Maximum event depth (player/engine events are depth 0).
    pub max_depth: u32,
    /// Maximum events dispatched per pump call, across all waves.
    pub max_events: usize,
    /// Maximum script commands applied per pump call.
    pub max_commands: usize,
}

impl Default for PumpLimits {
    fn default() -> Self {
        Self {
            max_depth: 8,
            max_events: 256,
            max_commands: 1024,
        }
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct PumpStats {
    pub events: usize,
    pub commands: usize,
    pub waves: u32,
    /// Events dropped by the depth or event limits.
    pub dropped_events: usize,
    /// Commands dropped by the command limit.
    pub dropped_commands: usize,
    pub errors: usize,
}

/// Applies `commands` (from a scheduler tick or an earlier dispatch), then dispatches
/// `events` and everything they cause, wave by wave, within `limits`.
pub fn pump(
    host: &ScriptHost,
    applier: &mut impl CommandApplier,
    events: Vec<Envelope>,
    commands: Vec<ScriptCommand>,
    limits: PumpLimits,
) -> PumpStats {
    let mut stats = PumpStats::default();
    let mut next = Vec::new();
    apply_all(applier, commands, 1, &limits, &mut stats, &mut next);
    let mut wave = events;
    wave.append(&mut next);
    while !wave.is_empty() {
        stats.waves += 1;
        let mut pending = Vec::new();
        for env in wave {
            if env.depth > limits.max_depth || stats.events >= limits.max_events {
                stats.dropped_events += 1;
                continue;
            }
            stats.events += 1;
            let (cmds, s) = host.dispatch_envelope(&env, applier.query());
            stats.errors += s.errors;
            apply_all(
                applier,
                cmds,
                env.depth + 1,
                &limits,
                &mut stats,
                &mut pending,
            );
        }
        wave = pending;
    }
    if stats.dropped_events + stats.dropped_commands > 0 {
        tracing::warn!(
            dropped_events = stats.dropped_events,
            dropped_commands = stats.dropped_commands,
            "script event cascade hit its limits and was truncated"
        );
    }
    stats
}

fn apply_all(
    applier: &mut impl CommandApplier,
    commands: Vec<ScriptCommand>,
    depth: u32,
    limits: &PumpLimits,
    stats: &mut PumpStats,
    out: &mut Vec<Envelope>,
) {
    for cmd in commands {
        if stats.commands >= limits.max_commands {
            stats.dropped_commands += 1;
            continue;
        }
        stats.commands += 1;
        let origin = EventOrigin::Mod(cmd.mod_id().to_owned());
        if let Some(event) = applier.apply(cmd) {
            out.push(Envelope {
                event,
                origin,
                depth,
            });
        }
    }
}
