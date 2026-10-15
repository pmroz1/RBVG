//! Survival block inventory: nine slots of stacked placeable blocks.
//!
//! Every mutation is all-or-nothing, so counts never go negative, overflow a stack or
//! lose blocks. Runtime ids are only held in memory; saves use names (see `persistence`).

use thiserror::Error;

use crate::selection::HOTBAR_SLOTS;
use crate::{BlockId, BlockRegistry};

pub const STACK_LIMIT: u16 = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stack {
    pub block: BlockId,
    /// Always `1..=STACK_LIMIT`; empty stacks are `None` slots.
    pub count: u16,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum InventoryError {
    #[error("block {0:?} cannot be stored (only registered solid blocks)")]
    NotStorable(BlockId),
    #[error("not enough inventory space")]
    Full,
    #[error("slot {0} does not hold enough blocks")]
    NotEnough(usize),
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BlockInventory {
    pub(crate) slots: [Option<Stack>; HOTBAR_SLOTS],
}

impl BlockInventory {
    pub fn slots(&self) -> &[Option<Stack>; HOTBAR_SLOTS] {
        &self.slots
    }

    pub fn get(&self, slot: usize) -> Option<Stack> {
        self.slots.get(slot).copied().flatten()
    }

    /// Total number of `block` across all stacks.
    pub fn count(&self, block: BlockId) -> u32 {
        self.slots
            .iter()
            .flatten()
            .filter(|s| s.block == block)
            .map(|s| u32::from(s.count))
            .sum()
    }

    /// How many more `block` fit, merging into existing stacks and empty slots.
    pub fn space_for(&self, block: BlockId) -> u32 {
        self.slots
            .iter()
            .map(|slot| match slot {
                None => u32::from(STACK_LIMIT),
                Some(s) if s.block == block => u32::from(STACK_LIMIT - s.count),
                Some(_) => 0,
            })
            .sum()
    }

    /// Adds `count` blocks, filling matching stacks first, then empty slots in order.
    /// Fails without changes if the block is not storable or does not fit entirely.
    pub fn add(
        &mut self,
        block: BlockId,
        count: u16,
        registry: &BlockRegistry,
    ) -> Result<(), InventoryError> {
        if block == BlockId::AIR || !registry.is_solid(block) {
            return Err(InventoryError::NotStorable(block));
        }
        if self.space_for(block) < u32::from(count) {
            return Err(InventoryError::Full);
        }
        let mut left = count;
        for s in self.slots.iter_mut().flatten().filter(|s| s.block == block) {
            let n = left.min(STACK_LIMIT - s.count);
            s.count += n;
            left -= n;
        }
        for slot in self.slots.iter_mut().filter(|s| s.is_none()) {
            if left == 0 {
                break;
            }
            let n = left.min(STACK_LIMIT);
            *slot = Some(Stack { block, count: n });
            left -= n;
        }
        debug_assert_eq!(left, 0);
        Ok(())
    }

    /// Removes `count` (≥ 1) blocks from `slot`, emptying it at zero. Returns the block.
    pub fn remove(&mut self, slot: usize, count: u16) -> Result<BlockId, InventoryError> {
        let entry = self
            .slots
            .get_mut(slot)
            .ok_or(InventoryError::NotEnough(slot))?;
        let Some(s) = entry.as_mut().filter(|s| count > 0 && s.count >= count) else {
            return Err(InventoryError::NotEnough(slot));
        };
        let block = s.block;
        s.count -= count;
        if s.count == 0 {
            *entry = None;
        }
        Ok(block)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::tests::test_registry;

    #[test]
    fn merges_splits_and_respects_capacity() {
        let mut reg = test_registry();
        let stone = reg.id("core:stone").unwrap();
        let dirt = reg.id("core:dirt").unwrap();
        let mut inv = BlockInventory::default();
        assert!(inv.slots().iter().all(Option::is_none));
        assert_eq!(inv.space_for(stone), 9 * 64);

        inv.add(stone, 60, &reg).unwrap();
        inv.add(dirt, 1, &reg).unwrap();
        inv.add(stone, 10, &reg).unwrap();
        // Merged into slot 0 up to the limit, overflow into the first empty slot.
        assert_eq!(
            inv.get(0),
            Some(Stack {
                block: stone,
                count: 64
            })
        );
        assert_eq!(
            inv.get(1),
            Some(Stack {
                block: dirt,
                count: 1
            })
        );
        assert_eq!(
            inv.get(2),
            Some(Stack {
                block: stone,
                count: 6
            })
        );
        assert_eq!(inv.count(stone), 70);

        assert_eq!(inv.remove(0, 4), Ok(stone));
        assert_eq!(inv.get(0).unwrap().count, 60);
        assert_eq!(inv.remove(1, 2), Err(InventoryError::NotEnough(1)));
        assert_eq!(inv.remove(1, 0), Err(InventoryError::NotEnough(1)));
        assert_eq!(inv.remove(1, 1), Ok(dirt));
        assert_eq!(inv.get(1), None);
        assert_eq!(inv.remove(1, 1), Err(InventoryError::NotEnough(1)));
        assert_eq!(inv.remove(9, 1), Err(InventoryError::NotEnough(9)));
        // Partial stacks are topped up before an empty slot is used.
        inv.add(stone, 5, &reg).unwrap();
        assert_eq!(inv.get(0).unwrap().count, 64);
        assert_eq!(inv.get(1), None);
        assert_eq!(inv.get(2).unwrap().count, 7);

        // Fill everything: an add that does not fit entirely changes nothing.
        let wood = reg.id("core:wood").unwrap();
        for _ in 0..7 {
            inv.add(wood, 64, &reg).unwrap();
        }
        inv.add(stone, 57, &reg).unwrap();
        assert_eq!(inv.space_for(stone), 0);
        let before = inv.clone();
        assert_eq!(inv.add(stone, 1, &reg), Err(InventoryError::Full));
        assert_eq!(inv.add(dirt, 1, &reg), Err(InventoryError::Full));
        assert_eq!(inv, before);
        assert_eq!(inv.add(wood, u16::MAX, &reg), Err(InventoryError::Full));

        let mut empty = BlockInventory::default();
        assert_eq!(
            empty.add(BlockId::AIR, 1, &reg),
            Err(InventoryError::NotStorable(BlockId::AIR))
        );
        assert_eq!(
            empty.add(BlockId(999), 1, &reg),
            Err(InventoryError::NotStorable(BlockId(999)))
        );
        reg.load_toml(
            "test",
            "format_version = 1\n[[blocks]]\nid = \"test:glass\"\n\
             display_name = \"Glass\"\nsolid = false\ncolor = [0.5, 0.5, 0.5]",
        )
        .unwrap();
        let glass = reg.id("test:glass").unwrap();
        assert_eq!(
            empty.add(glass, 1, &reg),
            Err(InventoryError::NotStorable(glass))
        );
        empty.add(stone, 9 * 64, &reg).unwrap();
        assert!(empty.slots().iter().all(|s| s.unwrap().count == 64));
    }
}
