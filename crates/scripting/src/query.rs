//! Read-only world access for scripts.

use rbvg_voxel::{BlockPos, BlockRegistry, VoxelWorld};

/// Narrow, read-only view of the world handed to the script host for the duration of one
/// dispatch or scheduler tick. Scripts can only reach it through validated API functions
/// while the host is running them; they never hold a reference to it.
pub trait WorldQuery {
    /// Namespaced id of the block at `pos`, or `None` if its chunk is not loaded.
    /// Must not generate or load chunks.
    fn block(&self, pos: BlockPos) -> Option<&str>;
}

/// [`WorldQuery`] over the engine's voxel world and block registry.
pub struct VoxelQuery<'a> {
    pub world: &'a VoxelWorld,
    pub registry: &'a BlockRegistry,
}

impl WorldQuery for VoxelQuery<'_> {
    fn block(&self, pos: BlockPos) -> Option<&str> {
        self.world
            .block_if_loaded(pos)
            .map(|id| self.registry.name(id))
    }
}

/// A world with nothing loaded (every query returns `None`).
pub struct NoWorld;

impl WorldQuery for NoWorld {
    fn block(&self, _: BlockPos) -> Option<&str> {
        None
    }
}
