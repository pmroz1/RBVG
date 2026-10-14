//! Voxel ray casting (Amanatides & Woo grid traversal).

use crate::coords::BlockPos;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RayHit {
    /// The solid block that was hit.
    pub block: BlockPos,
    /// Unit normal of the face that was entered; `block + normal` is the placement cell.
    /// Zero if the ray started inside a solid block.
    pub normal: [i32; 3],
    pub distance: f32,
}

impl RayHit {
    pub fn adjacent(&self) -> BlockPos {
        self.block
            .offset(self.normal[0], self.normal[1], self.normal[2])
    }
}

/// Walks the grid from `origin` along `dir` (need not be normalised) up to `max_dist`,
/// returning the first block for which `is_solid` holds.
pub fn raycast(
    origin: [f32; 3],
    dir: [f32; 3],
    max_dist: f32,
    mut is_solid: impl FnMut(BlockPos) -> bool,
) -> Option<RayHit> {
    let len = (dir[0] * dir[0] + dir[1] * dir[1] + dir[2] * dir[2]).sqrt();
    if len == 0.0 || !len.is_finite() {
        return None;
    }
    let d = [dir[0] / len, dir[1] / len, dir[2] / len];
    let mut cell = [
        origin[0].floor() as i32,
        origin[1].floor() as i32,
        origin[2].floor() as i32,
    ];
    let mut step = [0i32; 3];
    let mut t_max = [f32::INFINITY; 3];
    let mut t_delta = [f32::INFINITY; 3];
    for a in 0..3 {
        if d[a] > 0.0 {
            step[a] = 1;
            t_max[a] = (cell[a] as f32 + 1.0 - origin[a]) / d[a];
            t_delta[a] = 1.0 / d[a];
        } else if d[a] < 0.0 {
            step[a] = -1;
            t_max[a] = (origin[a] - cell[a] as f32) / -d[a];
            t_delta[a] = -1.0 / d[a];
        }
    }
    let mut normal = [0i32; 3];
    let mut distance = 0.0;
    loop {
        let pos = BlockPos::new(cell[0], cell[1], cell[2]);
        if is_solid(pos) {
            return Some(RayHit {
                block: pos,
                normal,
                distance,
            });
        }
        let a = if t_max[0] < t_max[1] {
            if t_max[0] < t_max[2] { 0 } else { 2 }
        } else if t_max[1] < t_max[2] {
            1
        } else {
            2
        };
        distance = t_max[a];
        if distance > max_dist {
            return None;
        }
        cell[a] += step[a];
        t_max[a] += t_delta[a];
        normal = [0; 3];
        normal[a] = -step[a];
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hits_floor_with_up_normal() {
        let hit = raycast([0.5, 5.5, 0.5], [0.0, -1.0, 0.0], 10.0, |p| p.y < 0).unwrap();
        assert_eq!(hit.block, BlockPos::new(0, -1, 0));
        assert_eq!(hit.normal, [0, 1, 0]);
        assert_eq!(hit.adjacent(), BlockPos::new(0, 0, 0));
        assert!((hit.distance - 5.5).abs() < 1e-5);
    }

    #[test]
    fn respects_max_distance_and_negative_axes() {
        assert!(raycast([0.5, 5.5, 0.5], [0.0, -1.0, 0.0], 3.0, |p| p.y < 0).is_none());
        let hit = raycast([0.5, 0.5, 0.5], [-1.0, 0.0, -0.2], 10.0, |p| p.x <= -3).unwrap();
        assert_eq!(hit.block.x, -3);
        assert_eq!(hit.normal, [1, 0, 0]);
    }

    #[test]
    fn zero_direction_is_none() {
        assert!(raycast([0.0; 3], [0.0; 3], 10.0, |_| true).is_none());
    }
}
