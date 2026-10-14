//! Data crossing the engine/script boundary.

use rbvg_voxel::BlockPos;

/// Engine → script notifications. All Phase 1 events are notification-only: they are
/// dispatched after the change happened and cannot be cancelled.
#[derive(Debug, Clone, PartialEq)]
pub enum ScriptEvent {
    /// A player broke a block. `block` is the namespaced id of the removed block.
    BlockBroken { pos: BlockPos, block: String },
    /// A player placed a block.
    BlockPlaced { pos: BlockPos, block: String },
}

impl ScriptEvent {
    /// Every event name scripts may subscribe to.
    pub const NAMES: &[&str] = &["block_broken", "block_placed"];

    pub fn name(&self) -> &'static str {
        match self {
            Self::BlockBroken { .. } => "block_broken",
            Self::BlockPlaced { .. } => "block_placed",
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
}
