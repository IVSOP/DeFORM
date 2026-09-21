//! Ground-plane Vleue navmesh built once from the exported Blender solids.
//! Queries are synchronous and stateless so rollback and headless bots use the same routes.
use std::sync::OnceLock;

use avian3d::prelude::Collider;
use glam::{Quat, Vec2, Vec3};
use vleue_navigator::{NavMesh, Triangulation};

use crate::shooter_logic::{ARENA_HALF_X, ARENA_HALF_Z, PLAYER_RADIUS};

const CLEARANCE: f32 = PLAYER_RADIUS + 0.06;

struct Navigation {
    mesh: NavMesh,
    solids: Vec<(Collider, Vec3, Quat)>,
}

fn rectangle(min: Vec2, max: Vec2) -> [Vec2; 4] {
    [min, Vec2::new(max.x, min.y), max, Vec2::new(min.x, max.y)]
}

fn navigation() -> &'static Navigation {
    static NAVIGATION: OnceLock<Navigation> = OnceLock::new();
    NAVIGATION.get_or_init(|| {
        let bounds = Vec2::new(ARENA_HALF_X, ARENA_HALF_Z) - Vec2::splat(CLEARANCE);
        let mut triangulation = Triangulation::from_outer_edges(&rectangle(-bounds, bounds));
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
            // Ground navigation only: ignore the floor and overhead fixtures.
            // Conservative world-space bounds also account for rotated cuboids.
            if center.y + extents.y < 0.25 || center.y - extents.y > 2.05 {
                continue;
            }
            // Inflate the bounds conservatively and round outward to millimetres.
            // This removes Blender transform noise and keeps overlapping obstacle
            // constraints axis-aligned instead of intersecting rounded offset arcs.
            triangulation.add_obstacle(rectangle(
                ((Vec2::new(center.x - extents.x, center.z - extents.z) - Vec2::splat(CLEARANCE))
                    * 1000.0)
                    .floor()
                    / 1000.0,
                ((Vec2::new(center.x + extents.x, center.z + extents.z) + Vec2::splat(CLEARANCE))
                    * 1000.0)
                    .ceil()
                    / 1000.0,
            ));
        }
        let mut mesh = triangulation.as_navmesh();
        for _ in 0..3 {
            if mesh.merge_polygons() {
                break;
            }
        }
        // Recover endpoints slightly inside the clearance margin, without snapping
        // distant/unreachable targets to unrelated parts of the arena.
        mesh.set_search_delta(0.1).set_search_steps(8);
        Navigation {
            mesh: NavMesh::from_polyanya_mesh(mesh),
            solids,
        }
    })
}

pub fn visible(origin: Vec3, target: Vec3) -> bool {
    let ray = target - origin;
    let distance = ray.length();
    if distance < 0.001 {
        return true;
    }
    !navigation().solids.iter().any(|(c, p, r)| {
        c.cast_ray(*p, *r, origin, ray / distance, distance, true)
            .is_some()
    })
}
/// Next ground waypoint, or the origin when there is no reachable route.
pub fn waypoint(origin: Vec3, target: Vec3) -> Vec3 {
    next_waypoint(&navigation().mesh, origin, target).unwrap_or(origin)
}

fn next_waypoint(mesh: &NavMesh, origin: Vec3, target: Vec3) -> Option<Vec3> {
    let from = Vec2::new(origin.x, origin.z);
    let to = Vec2::new(target.x, target.z);
    let geometry = mesh.get();
    let start = geometry.get_closest_point(from)?.position();
    let end = geometry.get_closest_point(to)?.position();
    let path = mesh.path(start, end)?;
    // Re-enter the mesh first if collision/rollback left us outside its clearance.
    let next = if from.distance_squared(start) > 0.0001 {
        start
    } else {
        path.path
            .into_iter()
            .find(|p| p.distance_squared(from) > 0.0001)
            .unwrap_or(end)
    };
    Some(Vec3::new(next.x, origin.y, next.y))
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

    #[test]
    fn routes_clear_cover_and_preserve_height() {
        let origin = Vec3::new(0.0, 1.1, 14.4);
        let target = Vec3::new(0.0, 2.5, -14.4);
        let path = navigation()
            .mesh
            .path(Vec2::new(origin.x, origin.z), Vec2::new(target.x, target.z))
            .unwrap();
        assert!(path.path.len() > 1, "route must go around central cover");
        let mut previous = origin;
        for point in path.path {
            let next = Vec3::new(point.x, origin.y, point.y);
            // Sample the whole segment with rays around the capsule circumference.
            for step in 0..=50 {
                let center = previous.lerp(next, step as f32 / 50.0);
                for direction in [Vec3::X, -Vec3::X, Vec3::Z, -Vec3::Z] {
                    assert!(visible(center, center + direction * PLAYER_RADIUS));
                }
            }
            previous = next;
        }
        assert_eq!(waypoint(origin, target).y, origin.y);
    }

    #[test]
    fn disconnected_or_distant_endpoints_have_no_route() {
        let mesh = NavMesh::from_edge_and_obstacles(
            rectangle(Vec2::ZERO, Vec2::splat(10.0)).to_vec(),
            vec![rectangle(Vec2::new(4.0, -1.0), Vec2::new(6.0, 11.0)).to_vec()],
        );
        assert!(next_waypoint(&mesh, Vec3::new(2.0, 1.1, 5.0), Vec3::new(8.0, 1.1, 5.0)).is_none());
        let origin = Vec3::new(0.0, 1.1, 14.4);
        assert_eq!(waypoint(origin, Vec3::splat(1000.0)), origin);
        assert_eq!(waypoint(origin, origin), origin);
    }
}
