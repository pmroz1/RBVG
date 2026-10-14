//! Axis-aligned box movement against solid voxels.
//!
//! shortcut: discrete axis-by-axis sweeps with small sub-steps, enough for player-speed
//! bodies; switch to a swept/continuous solver (or Rapier) for fast projectiles.

use crate::coords::BlockPos;

/// Gap kept between a box and the face it rests against, so it is not "inside" the block.
const SKIN: f32 = 1e-3;
/// Maximum movement per sub-step; below one block so nothing can tunnel.
const MAX_STEP: f32 = 0.45;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Aabb {
    pub min: [f32; 3],
    pub max: [f32; 3],
}

impl Aabb {
    /// Box with its bottom-centre at `feet`.
    pub fn from_feet(feet: [f32; 3], half_width: f32, height: f32) -> Self {
        Self {
            min: [feet[0] - half_width, feet[1], feet[2] - half_width],
            max: [feet[0] + half_width, feet[1] + height, feet[2] + half_width],
        }
    }

    fn translate(&mut self, axis: usize, d: f32) {
        self.min[axis] += d;
        self.max[axis] += d;
    }

    /// Calls `f` for every block cell the box overlaps.
    fn any_cell(&self, mut f: impl FnMut(BlockPos) -> bool) -> bool {
        let lo = |a: usize| self.min[a].floor() as i32;
        let hi = |a: usize| self.max[a].ceil() as i32 - 1;
        for y in lo(1)..=hi(1) {
            for z in lo(2)..=hi(2) {
                for x in lo(0)..=hi(0) {
                    if f(BlockPos::new(x, y, z)) {
                        return true;
                    }
                }
            }
        }
        false
    }

    /// Whether the box overlaps any solid cell.
    pub fn intersects(&self, is_solid: impl FnMut(BlockPos) -> bool) -> bool {
        self.any_cell(is_solid)
    }
}

/// Result of [`move_aabb`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MoveResult {
    pub aabb: Aabb,
    /// Per axis: whether movement was blocked (use `blocked[1] && delta.y < 0` for grounded).
    pub blocked: [bool; 3],
}

/// Moves `aabb` by `delta`, stopping flush against solid blocks. Resolves Y first so
/// walking on the ground does not snag on floor seams.
pub fn move_aabb(
    mut aabb: Aabb,
    delta: [f32; 3],
    mut is_solid: impl FnMut(BlockPos) -> bool,
) -> MoveResult {
    let mut blocked = [false; 3];
    for axis in [1, 0, 2] {
        let mut remaining = delta[axis];
        while remaining != 0.0 && !blocked[axis] {
            let d = remaining.clamp(-MAX_STEP, MAX_STEP);
            remaining -= d;
            aabb.translate(axis, d);
            if aabb.intersects(&mut is_solid) {
                // Snap flush against the block face we entered.
                let snapped = if d > 0.0 {
                    aabb.max[axis].ceil() - 1.0 - SKIN - aabb.max[axis]
                } else {
                    aabb.min[axis].floor() + 1.0 + SKIN - aabb.min[axis]
                };
                aabb.translate(axis, snapped);
                blocked[axis] = true;
            }
        }
    }
    MoveResult { aabb, blocked }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn floor(p: BlockPos) -> bool {
        p.y < 0
    }

    #[test]
    fn falls_onto_floor_and_stops() {
        let a = Aabb::from_feet([0.5, 3.0, 0.5], 0.3, 1.8);
        let r = move_aabb(a, [0.0, -10.0, 0.0], floor);
        assert!(r.blocked[1]);
        assert!((r.aabb.min[1] - SKIN).abs() < 1e-4, "{:?}", r.aabb);
    }

    #[test]
    fn slides_along_wall() {
        let wall = |p: BlockPos| p.y < 0 || p.x >= 2;
        let a = Aabb::from_feet([0.5, 0.01, 0.5], 0.3, 1.8);
        let r = move_aabb(a, [5.0, 0.0, 1.0], wall);
        assert!(r.blocked[0] && !r.blocked[2]);
        assert!(r.aabb.max[0] < 2.0 && r.aabb.max[0] > 1.99);
        assert!((r.aabb.min[2] - 1.2).abs() < 1e-4);
    }

    #[test]
    fn negative_coordinates_collide() {
        let wall = |p: BlockPos| p.x <= -3;
        let a = Aabb::from_feet([-0.5, 0.0, -0.5], 0.3, 1.8);
        let r = move_aabb(a, [-10.0, 0.0, 0.0], wall);
        assert!(r.blocked[0]);
        assert!(r.aabb.min[0] > -2.0 && r.aabb.min[0] < -1.99);
    }
}
