//! Creative selection is content-driven and independent of the renderer/input backend.

use crate::{BlockId, BlockRegistry};

pub const HOTBAR_SLOTS: usize = 9;

#[derive(Debug, Clone)]
pub struct BlockSelection {
    pub slots: [BlockId; HOTBAR_SLOTS],
    pub selected: usize,
}

impl BlockSelection {
    pub fn new(registry: &BlockRegistry) -> Self {
        let mut slots = [BlockId::AIR; HOTBAR_SLOTS];
        for (slot, (id, _)) in slots.iter_mut().zip(
            catalog(registry)
                .into_iter()
                .filter(|(_, def)| def.solid && !def.missing),
        ) {
            *slot = id;
        }
        Self { slots, selected: 0 }
    }

    pub fn current(&self) -> Option<BlockId> {
        self.slots
            .get(self.selected)
            .copied()
            .filter(|id| *id != BlockId::AIR)
    }

    /// Unavailable blocks stay visible in the catalog, but cannot be placed.
    pub fn assign(&mut self, slot: usize, id: BlockId, registry: &BlockRegistry) -> bool {
        if slot >= HOTBAR_SLOTS
            || id == BlockId::AIR
            || registry.get(id).is_none_or(|def| def.missing || !def.solid)
        {
            return false;
        }
        self.slots[slot] = id;
        true
    }
}

/// Stable alphabetical presentation, never registry insertion order.
pub fn catalog(registry: &BlockRegistry) -> Vec<(BlockId, &crate::BlockDef)> {
    let mut blocks: Vec<_> = registry
        .iter()
        .filter(|(id, _)| *id != BlockId::AIR)
        .collect();
    blocks.sort_unstable_by(|(_, a), (_, b)| a.id.cmp(&b.id));
    blocks
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_content_is_accessible_without_expanding_the_hotbar() {
        let mut registry = BlockRegistry::default();
        for i in (0..20).rev() {
            registry
                .load_toml(
                    "test",
                    &format!(
                        "format_version = 1\n[[blocks]]\nid = \"test:block_{i:02}\"\n\
                 display_name = \"Block {i}\"\ncolor = [0.5, 0.5, 0.5]"
                    ),
                )
                .unwrap();
        }
        registry
            .load_toml(
                "test",
                "format_version = 1\n[[blocks]]\nid = \"test:glass\"\n\
                 display_name = \"Glass\"\nsolid = false\ncolor = [0.5, 0.5, 0.5]",
            )
            .unwrap();
        let mut selection = BlockSelection::new(&registry);
        let entries = catalog(&registry);
        let glass = registry.id("test:glass").unwrap();
        assert_eq!(entries.len(), 21);
        assert!(entries.iter().any(|(id, _)| *id == glass));
        assert!(!selection.slots.contains(&glass));
        assert_eq!(registry.name(selection.slots[0]), "test:block_00");
        let last = registry.id("test:block_19").unwrap();
        assert!(selection.assign(8, last, &registry));
        selection.selected = 8;
        assert_eq!(selection.current(), Some(last));
        assert_eq!(selection.slots.len(), 9);
        assert!(!selection.assign(8, glass, &registry));
        let missing = registry.register_missing("removed:block").unwrap();
        assert!(
            catalog(&registry)
                .iter()
                .any(|(id, def)| *id == missing && def.missing)
        );
        assert!(!selection.assign(8, missing, &registry));
        assert!(!selection.assign(9, last, &registry));
        assert!(!selection.assign(0, BlockId::AIR, &registry));
    }
}
