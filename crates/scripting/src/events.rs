//! Data crossing the engine/script boundary.

use rbvg_voxel::BlockPos;

/// Who caused an event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EventOrigin {
    /// A player action (break, place, interact).
    Player,
    /// An engine/system action.
    Engine,
    /// A script command, applied on behalf of the named mod.
    Mod(String),
}

impl EventOrigin {
    /// Value of `event.origin` in scripts: `"player"`, `"engine"` or `"mod"`.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Player => "player",
            Self::Engine => "engine",
            Self::Mod(_) => "mod",
        }
    }
}

/// Engine → script events.
///
/// `block_broken`/`block_placed` are notifications dispatched after the change happened.
/// `before_block_break` is dispatched before the edit is committed and is cancellable
/// (`event.cancel()`). `interact` reports a player pressing the interact key on a block.
#[derive(Debug, Clone, PartialEq)]
pub enum ScriptEvent {
    /// A block was removed. `block` is the namespaced id of the removed block.
    BlockBroken { pos: BlockPos, block: String },
    /// A block was placed.
    BlockPlaced { pos: BlockPos, block: String },
    /// A player is about to break a block. Cancellable.
    BeforeBlockBreak { pos: BlockPos, block: String },
    /// The player interacted with the targeted block.
    Interact { pos: BlockPos, block: String },
}

impl ScriptEvent {
    /// Every event name scripts may subscribe to.
    pub const NAMES: &[&str] = &[
        "block_broken",
        "block_placed",
        "before_block_break",
        "interact",
    ];

    pub fn name(&self) -> &'static str {
        match self {
            Self::BlockBroken { .. } => "block_broken",
            Self::BlockPlaced { .. } => "block_placed",
            Self::BeforeBlockBreak { .. } => "before_block_break",
            Self::Interact { .. } => "interact",
        }
    }

    pub fn cancellable(&self) -> bool {
        matches!(self, Self::BeforeBlockBreak { .. })
    }

    pub fn pos(&self) -> BlockPos {
        match self {
            Self::BlockBroken { pos, .. }
            | Self::BlockPlaced { pos, .. }
            | Self::BeforeBlockBreak { pos, .. }
            | Self::Interact { pos, .. } => *pos,
        }
    }

    pub fn block(&self) -> &str {
        match self {
            Self::BlockBroken { block, .. }
            | Self::BlockPlaced { block, .. }
            | Self::BeforeBlockBreak { block, .. }
            | Self::Interact { block, .. } => block,
        }
    }
}

/// An event plus its provenance. `depth` is 0 for player/engine events and grows by one
/// for every wave of script-caused events; the pump drops events beyond its limit.
#[derive(Debug, Clone, PartialEq)]
pub struct Envelope {
    pub event: ScriptEvent,
    pub origin: EventOrigin,
    pub depth: u32,
}

impl Envelope {
    pub fn player(event: ScriptEvent) -> Self {
        Self {
            event,
            origin: EventOrigin::Player,
            depth: 0,
        }
    }
}

/// Script → engine requests. Queued during dispatch and applied by the engine afterwards;
/// the engine re-validates every field (block ids, loaded chunks) before applying.
#[derive(Debug, Clone, PartialEq)]
pub enum ScriptCommand {
    SetBlock {
        mod_id: String,
        pos: BlockPos,
        block: String,
    },
    /// Show a short message to the player.
    Notify { mod_id: String, message: String },
}

impl ScriptCommand {
    pub fn mod_id(&self) -> &str {
        match self {
            Self::SetBlock { mod_id, .. } | Self::Notify { mod_id, .. } => mod_id,
        }
    }
}
