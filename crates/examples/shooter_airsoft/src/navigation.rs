//! Small deterministic ground navigation grid derived from the exported Blender solids.
//! Bots use it only to reach line of sight; players still use full Avian collision.
use std::{collections::VecDeque, sync::OnceLock};

use avian3d::prelude::Collider;
use glam::{Quat, Vec3};

use crate::shooter_logic::PLAYER_RADIUS;
const WIDTH: usize = 47;
const DEPTH: usize = 71;
const CELLS: usize = WIDTH * DEPTH;
const STEP: f32 = 0.5;
struct Navigation {
    blocked: Vec<bool>,
    solids: Vec<(Collider, Vec3, Quat)>,
}
fn grid() -> &'static Navigation {
    static GRID: OnceLock<Navigation> = OnceLock::new();
    GRID.get_or_init(|| {
        let mut blocked = vec![false; CELLS];
        let mut solids = Vec::new();
        for (center, size, rotation) in crate::arena_data::SOLIDS {
            let center = Vec3::from_array(*center);
            let half = Vec3::from_array(*size) * 0.5;
            let rotation = Quat::from_array(*rotation).normalize();
            let extents = (rotation * Vec3::X).abs() * half.x
                + (rotation * Vec3::Y).abs() * half.y
                + (rotation * Vec3::Z).abs() * half.z;
            solids.push((
                Collider::cuboid(size[0], size[1], size[2]),
                center,
                rotation,
            ));
            if center.y + extents.y < 0.25 || center.y - extents.y > 2.05 {
                continue;
            }
            for (i, cell) in blocked.iter_mut().enumerate() {
                let p = position(i);
                if (p.x - center.x).abs() < extents.x + PLAYER_RADIUS + 0.06
                    && (p.z - center.z).abs() < extents.z + PLAYER_RADIUS + 0.06
                {
                    *cell = true;
                }
            }
        }
        Navigation { blocked, solids }
    })
}
fn position(i: usize) -> Vec3 {
    Vec3::new(
        (i % WIDTH) as f32 * STEP - 11.5,
        0.0,
        (i / WIDTH) as f32 * STEP - 17.5,
    )
}
fn nearest(p: Vec3, nav: &Navigation) -> usize {
    (0..CELLS)
        .filter(|i| !nav.blocked[*i])
        .min_by(|a, b| {
            position(*a)
                .distance_squared(Vec3::new(p.x, 0.0, p.z))
                .total_cmp(&position(*b).distance_squared(Vec3::new(p.x, 0.0, p.z)))
        })
        .unwrap_or(0)
}
pub fn visible(origin: Vec3, target: Vec3) -> bool {
    let ray = target - origin;
    let distance = ray.length();
    if distance < 0.001 {
        return true;
    }
    !grid().solids.iter().any(|(c, p, r)| {
        c.cast_ray(*p, *r, origin, ray / distance, distance, true)
            .is_some()
    })
}
pub fn waypoint(origin: Vec3, target: Vec3) -> Vec3 {
    let nav = grid();
    let start = nearest(origin, nav);
    let end = nearest(target, nav);
    if start == end {
        return target;
    }
    let mut previous = vec![usize::MAX; CELLS];
    previous[start] = start;
    let mut queue = VecDeque::from([start]);
    while let Some(i) = queue.pop_front() {
        if i == end {
            break;
        }
        for next in [
            i.checked_sub(WIDTH),
            (i + WIDTH < CELLS).then_some(i + WIDTH),
            (i % WIDTH > 0).then(|| i - 1),
            (i % WIDTH + 1 < WIDTH).then_some(i + 1),
        ]
        .into_iter()
        .flatten()
        {
            if !nav.blocked[next] && previous[next] == usize::MAX {
                previous[next] = i;
                queue.push_back(next);
            }
        }
    }
    if previous[end] == usize::MAX {
        return origin;
    }
    let mut next = end;
    while previous[next] != start {
        next = previous[next];
    }
    position(next).with_y(origin.y)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn opposite_spawns_have_a_walkable_route() {
        let mut current = Vec3::new(0.0, 1.1, 14.4);
        let goal = Vec3::new(0.0, 1.1, -14.4);
        assert!(!visible(current + Vec3::Y * 0.6, goal + Vec3::Y * 0.6));
        for _ in 0..200 {
            current = waypoint(current, goal);
            if current.distance(goal) < 0.6 {
                return;
            }
        }
        panic!("bot route did not connect the spawn zones: {current:?}");
    }
}
