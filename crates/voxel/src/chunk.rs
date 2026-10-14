//! Dense 16³ chunk storage.

use crate::coords::{CHUNK_VOLUME, LocalPos};
use crate::registry::BlockId;

/// A cubic chunk of blocks stored contiguously (8 KiB of `u16` ids).
#[derive(Clone)]
pub struct Chunk {
    blocks: Box<[BlockId; CHUNK_VOLUME]>,
    /// Incremented on every change; consumers (meshing) compare against it.
    version: u32,
    /// True once a player/script edited this chunk, i.e. it differs from generation.
    modified: bool,
}

impl Default for Chunk {
    fn default() -> Self {
        Self {
            blocks: Box::new([BlockId::AIR; CHUNK_VOLUME]),
            version: 0,
            modified: false,
        }
    }
}

impl Chunk {
    pub fn get(&self, p: LocalPos) -> BlockId {
        self.blocks[p.index()]
    }

    /// Writes a block without marking the chunk as player-modified (used by generators).
    pub fn set_generated(&mut self, p: LocalPos, id: BlockId) {
        self.blocks[p.index()] = id;
        self.version = self.version.wrapping_add(1);
    }

    /// Writes a block and marks the chunk as modified. Returns the previous block.
    pub fn set(&mut self, p: LocalPos, id: BlockId) -> BlockId {
        let old = std::mem::replace(&mut self.blocks[p.index()], id);
        if old != id {
            self.version = self.version.wrapping_add(1);
            self.modified = true;
        }
        old
    }

    pub fn version(&self) -> u32 {
        self.version
    }

    pub fn is_modified(&self) -> bool {
        self.modified
    }

    pub fn is_empty(&self) -> bool {
        self.blocks.iter().all(|b| *b == BlockId::AIR)
    }

    /// Raw storage in [`LocalPos::index`] order (used by persistence).
    pub fn blocks(&self) -> &[BlockId; CHUNK_VOLUME] {
        &self.blocks
    }

    /// Builds a chunk from raw storage; it is considered modified.
    pub fn from_blocks(blocks: Box<[BlockId; CHUNK_VOLUME]>) -> Self {
        Self {
            blocks,
            version: 0,
            modified: true,
        }
    }
}
