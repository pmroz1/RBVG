//! Chunk map with loading, unloading, edits and remesh tracking.

use std::collections::{HashMap, HashSet};

use thiserror::Error;

use crate::chunk::Chunk;
use crate::coords::{BlockPos, CHUNK_SIZE, ChunkPos};
use crate::generation::WorldGenerator;
use crate::registry::BlockId;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum WorldError {
    #[error("chunk {0:?} is not loaded")]
    ChunkNotLoaded(ChunkPos),
}

/// Neighbour offsets along each axis, used for boundary bookkeeping.
const NEIGHBOURS: [(i32, i32, i32); 6] = [
    (1, 0, 0),
    (-1, 0, 0),
    (0, 1, 0),
    (0, -1, 0),
    (0, 0, 1),
    (0, 0, -1),
];

/// The block world. Unloaded positions read as air.
pub struct VoxelWorld {
    seed: u32,
    generator: Box<dyn WorldGenerator>,
    chunks: HashMap<ChunkPos, Chunk>,
    /// Edited chunks that are not resident (unloaded, or restored from a save before
    /// being streamed in). They take priority over the generator when loaded.
    stash: HashMap<ChunkPos, Chunk>,
    /// Loaded chunks whose mesh is stale.
    dirty: HashSet<ChunkPos>,
}

impl VoxelWorld {
    pub fn new(seed: u32, generator: Box<dyn WorldGenerator>) -> Self {
        Self {
            seed,
            generator,
            chunks: HashMap::new(),
            stash: HashMap::new(),
            dirty: HashSet::new(),
        }
    }

    pub fn seed(&self) -> u32 {
        self.seed
    }

    pub fn is_loaded(&self, pos: ChunkPos) -> bool {
        self.chunks.contains_key(&pos)
    }

    pub fn chunk(&self, pos: ChunkPos) -> Option<&Chunk> {
        self.chunks.get(&pos)
    }

    pub fn loaded_count(&self) -> usize {
        self.chunks.len()
    }

    pub fn loaded_positions(&self) -> impl Iterator<Item = ChunkPos> + '_ {
        self.chunks.keys().copied()
    }

    /// Loads a chunk from the stash or the generator. Returns false if already loaded.
    pub fn load_chunk(&mut self, pos: ChunkPos) -> bool {
        if self.chunks.contains_key(&pos) {
            return false;
        }
        let chunk = self
            .stash
            .remove(&pos)
            .unwrap_or_else(|| self.generator.generate(pos));
        self.chunks.insert(pos, chunk);
        self.mark_with_neighbours(pos);
        true
    }

    /// Unloads a chunk, keeping it in memory if it was edited. Returns false if not loaded.
    pub fn unload_chunk(&mut self, pos: ChunkPos) -> bool {
        let Some(chunk) = self.chunks.remove(&pos) else {
            return false;
        };
        if chunk.is_modified() {
            self.stash.insert(pos, chunk);
        }
        self.dirty.remove(&pos);
        for (dx, dy, dz) in NEIGHBOURS {
            self.mark_dirty(pos.offset(dx, dy, dz));
        }
        true
    }

    /// Installs an edited chunk (e.g. from a save), replacing any resident copy.
    pub fn insert_saved(&mut self, pos: ChunkPos, chunk: Chunk) {
        if let Some(slot) = self.chunks.get_mut(&pos) {
            *slot = chunk;
            self.mark_with_neighbours(pos);
        } else {
            self.stash.insert(pos, chunk);
        }
    }

    pub fn get_block(&self, p: BlockPos) -> BlockId {
        self.chunks
            .get(&p.chunk())
            .map_or(BlockId::AIR, |c| c.get(p.local()))
    }

    /// Sets a block in a loaded chunk, returning the previous block.
    pub fn set_block(&mut self, p: BlockPos, id: BlockId) -> Result<BlockId, WorldError> {
        let cpos = p.chunk();
        let chunk = self
            .chunks
            .get_mut(&cpos)
            .ok_or(WorldError::ChunkNotLoaded(cpos))?;
        let local = p.local();
        let old = chunk.set(local, id);
        if old != id {
            self.mark_dirty(cpos);
            // Faces of the neighbouring chunk may become visible/hidden.
            let edge = |v: u8| match i32::from(v) {
                0 => -1,
                v if v == CHUNK_SIZE - 1 => 1,
                _ => 0,
            };
            let (ex, ey, ez) = (edge(local.x), edge(local.y), edge(local.z));
            if ex != 0 {
                self.mark_dirty(cpos.offset(ex, 0, 0));
            }
            if ey != 0 {
                self.mark_dirty(cpos.offset(0, ey, 0));
            }
            if ez != 0 {
                self.mark_dirty(cpos.offset(0, 0, ez));
            }
        }
        Ok(old)
    }

    /// Every edited chunk, resident or stashed. This is what persistence saves.
    pub fn modified_chunks(&self) -> impl Iterator<Item = (ChunkPos, &Chunk)> {
        self.chunks
            .iter()
            .filter(|(_, c)| c.is_modified())
            .chain(self.stash.iter())
            .map(|(p, c)| (*p, c))
    }

    pub fn dirty_count(&self) -> usize {
        self.dirty.len()
    }

    /// Removes and returns up to `max` dirty chunks, nearest to `center` first.
    pub fn take_dirty_nearest(&mut self, center: ChunkPos, max: usize) -> Vec<ChunkPos> {
        let mut all: Vec<ChunkPos> = self.dirty.iter().copied().collect();
        all.sort_unstable_by_key(|p| {
            let (dx, dy, dz) = (p.x - center.x, p.y - center.y, p.z - center.z);
            dx * dx + dy * dy + dz * dz
        });
        all.truncate(max);
        for p in &all {
            self.dirty.remove(p);
        }
        all
    }

    fn mark_dirty(&mut self, pos: ChunkPos) {
        if self.chunks.contains_key(&pos) {
            self.dirty.insert(pos);
        }
    }

    fn mark_with_neighbours(&mut self, pos: ChunkPos) {
        self.mark_dirty(pos);
        for (dx, dy, dz) in NEIGHBOURS {
            self.mark_dirty(pos.offset(dx, dy, dz));
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Generator that fills everything below y=0 with block 1.
    pub(crate) struct Flat;
    impl WorldGenerator for Flat {
        fn generate(&self, pos: ChunkPos) -> Chunk {
            let mut c = Chunk::default();
            if pos.y < 0 {
                for i in 0..CHUNK_SIZE as u8 {
                    for j in 0..CHUNK_SIZE as u8 {
                        for k in 0..CHUNK_SIZE as u8 {
                            c.set_generated(crate::LocalPos::new(i, j, k), BlockId(1));
                        }
                    }
                }
            }
            c
        }
    }

    pub(crate) fn flat_world(radius: i32) -> VoxelWorld {
        let mut w = VoxelWorld::new(0, Box::new(Flat));
        for x in -radius..radius {
            for y in -radius..radius {
                for z in -radius..radius {
                    w.load_chunk(ChunkPos::new(x, y, z));
                }
            }
        }
        w
    }

    #[test]
    fn set_and_get_across_negative_coordinates() {
        let mut w = flat_world(1);
        let p = BlockPos::new(-1, -1, -1);
        assert_eq!(w.get_block(p), BlockId(1));
        assert_eq!(w.set_block(p, BlockId::AIR), Ok(BlockId(1)));
        assert_eq!(w.get_block(p), BlockId::AIR);
        assert_eq!(
            w.set_block(BlockPos::new(100, 0, 0), BlockId(1)),
            Err(WorldError::ChunkNotLoaded(ChunkPos::new(6, 0, 0)))
        );
    }

    #[test]
    fn boundary_edit_dirties_neighbour() {
        let mut w = flat_world(1);
        w.take_dirty_nearest(ChunkPos::new(0, 0, 0), usize::MAX);
        w.set_block(BlockPos::new(0, -1, 5), BlockId::AIR).unwrap();
        let mut dirty = w.take_dirty_nearest(ChunkPos::new(0, 0, 0), usize::MAX);
        dirty.sort();
        // x=0 is the chunk's low X edge, y=-1 is the top layer of chunk y=-1.
        assert_eq!(
            dirty,
            vec![
                ChunkPos::new(-1, -1, 0),
                ChunkPos::new(0, -1, 0),
                ChunkPos::new(0, 0, 0)
            ]
        );
    }

    #[test]
    fn edits_survive_unload_and_reload() {
        let mut w = flat_world(1);
        let p = BlockPos::new(3, -2, 3);
        w.set_block(p, BlockId::AIR).unwrap();
        assert!(w.unload_chunk(p.chunk()));
        assert_eq!(w.get_block(p), BlockId::AIR, "unloaded reads as air");
        assert_eq!(w.modified_chunks().count(), 1);
        assert!(w.load_chunk(p.chunk()));
        assert_eq!(w.get_block(p), BlockId::AIR);
        assert_eq!(w.get_block(p.offset(1, 0, 0)), BlockId(1));
    }
}
