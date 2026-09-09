//! Bounded, cosmetic blood effects driven by authoritative flesh-hit events.
use std::collections::{HashSet, VecDeque};

use avian3d::prelude::Collider;
use bevy::{
    asset::RenderAssetUsages,
    light::NotShadowCaster,
    mesh::{Indices, PrimitiveTopology},
    pbr::decal::{ForwardDecal, ForwardDecalMaterial, ForwardDecalMaterialExt},
    prelude::*,
    render::render_resource::{Extent3d, TextureDimension, TextureFormat},
};
use deform_core::Pubkey;
use shooter_airsoft::{
    arena_data,
    shooter_logic::{PLAYER_CAPSULE_LENGTH, PLAYER_RADIUS, ShooterGameState},
};

const PARTICLE_LIMIT: usize = 96;
const STAIN_LIMIT: usize = 128;
const WOUNDS_PER_PLAYER: usize = 32; // entry + exit marks for sixteen hits
const STEP: f32 = 1.0 / 60.0;

pub struct BloodPlugin;
impl Plugin for BloodPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, setup)
            .add_systems(Update, animate)
            .add_systems(OnExit(crate::client::AppState::InGame), clear);
    }
}

#[derive(Component)]
pub struct Wound;
#[derive(Component)]
pub struct BloodStain;
#[derive(Component)]
pub struct Droplet {
    velocity: Vec3,
    accumulator: f32,
    age: f32,
    seed: u32,
    source: Option<Vec3>,
}

#[derive(Resource)]
pub struct BloodEffects {
    material: Handle<StandardMaterial>,
    stain: Handle<ForwardDecalMaterial<StandardMaterial>>,
    drop_mesh: Handle<Mesh>,
    drop_material: Handle<StandardMaterial>,
    round: Option<u32>,
    seen: HashSet<u32>,
    history: VecDeque<u32>,
    wounds: VecDeque<(Pubkey, Entity)>,
    stains: VecDeque<Entity>,
    particles: VecDeque<Entity>,
}

struct Solid {
    collider: Collider,
    center: Vec3,
    rotation: Quat,
    min: Vec3,
    max: Vec3,
}
#[derive(Resource)]
struct BloodCollision(Vec<Solid>);
impl Default for BloodCollision {
    fn default() -> Self {
        Self(
            arena_data::SOLIDS
                .iter()
                .map(|(center, size, rotation)| {
                    let center = Vec3::from_array(*center);
                    let size = Vec3::from_array(*size);
                    let rotation = Quat::from_array(*rotation).normalize();
                    let half = size * 0.5;
                    let extent = (rotation * Vec3::X).abs() * half.x
                        + (rotation * Vec3::Y).abs() * half.y
                        + (rotation * Vec3::Z).abs() * half.z;
                    Solid {
                        collider: Collider::cuboid(size.x, size.y, size.z),
                        center,
                        rotation,
                        min: center - extent,
                        max: center + extent,
                    }
                })
                .collect(),
        )
    }
}
impl BloodCollision {
    fn cast(&self, start: Vec3, end: Vec3) -> Option<(Vec3, Vec3)> {
        let delta = end - start;
        let length = delta.length();
        if length < 1e-6 {
            return None;
        }
        let direction = delta / length;
        let mut nearest = length;
        let mut hit = None;
        let min = start.min(end);
        let max = start.max(end);
        for solid in &self.0 {
            // Most segments touch no boxes. Avoid a physics query for those.
            if min.cmpgt(solid.max).any() || max.cmplt(solid.min).any() {
                continue;
            }
            if let Some((distance, normal)) = solid.collider.cast_ray(
                solid.center,
                solid.rotation,
                start,
                direction,
                nearest,
                true,
            ) {
                nearest = distance;
                hit = Some((
                    start + direction * distance,
                    normal.try_normalize().unwrap_or(-direction),
                ));
            }
        }
        hit
    }
}

fn random(seed: &mut u32) -> f32 {
    *seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
    (*seed >> 8) as f32 / 16777216.0
}

/// Small original procedural texture, matching the existing code-authored impact effect.
fn splatter_texture() -> Image {
    let size = 128;
    let mut data = vec![0; size * size * 4];
    let mut seed = 731_u32;
    let satellites: Vec<_> = (0..22)
        .map(|_| {
            let angle = random(&mut seed) * std::f32::consts::TAU;
            let distance = 0.32 + random(&mut seed) * 0.43;
            (
                Vec2::new(angle.cos(), angle.sin()) * distance,
                0.014 + random(&mut seed) * 0.055,
            )
        })
        .collect();
    for y in 0..size {
        for x in 0..size {
            let p = Vec2::new(x as f32 + 0.5, y as f32 + 0.5) / size as f32 * 2.0 - Vec2::ONE;
            let angle = p.y.atan2(p.x);
            let radius = 0.31 + 0.065 * (angle * 7.0).sin() + 0.04 * (angle * 13.0 + 1.3).sin();
            let mut alpha = ((radius - p.length()) / 0.035).clamp(0.0, 1.0);
            for (center, r) in &satellites {
                alpha = alpha.max(((r - p.distance(*center)) / 0.012).clamp(0.0, 1.0));
            }
            let i = (y * size + x) * 4;
            let shade = (p.length() * 35.0) as u8;
            data[i..i + 4].copy_from_slice(&[72 + shade, 3, 7, (alpha * 238.0) as u8]);
        }
    }
    Image::new(
        Extent3d {
            width: size as u32,
            height: size as u32,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        data,
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::default(),
    )
}

fn setup(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut images: ResMut<Assets<Image>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut decals: ResMut<Assets<ForwardDecalMaterial<StandardMaterial>>>,
) {
    let texture = images.add(splatter_texture());
    let material = StandardMaterial {
        base_color_texture: Some(texture),
        alpha_mode: AlphaMode::Blend,
        unlit: true,
        perceptual_roughness: 0.6,
        ..default()
    };
    commands.insert_resource(BloodEffects {
        material: materials.add(material.clone()),
        stain: decals.add(ForwardDecalMaterial {
            base: material,
            extension: ForwardDecalMaterialExt {
                depth_fade_factor: 0.012,
            },
        }),
        drop_mesh: meshes.add(Sphere::new(1.0).mesh().ico(1).unwrap()),
        drop_material: materials.add(StandardMaterial {
            base_color: Color::srgb(0.32, 0.005, 0.015),
            unlit: true,
            ..default()
        }),
        round: None,
        seen: default(),
        history: default(),
        wounds: default(),
        stains: default(),
        particles: default(),
    });
    commands.insert_resource(BloodCollision::default());
}

fn capsule_normal(point: Vec3) -> Vec3 {
    let axis = Vec3::new(
        0.0,
        point
            .y
            .clamp(-PLAYER_CAPSULE_LENGTH * 0.5, PLAYER_CAPSULE_LENGTH * 0.5),
        0.0,
    );
    (point - axis).try_normalize().unwrap_or(Vec3::Z)
}

/// Project a subdivided patch onto the actual capsule surface, not a floating quad.
fn wound_mesh(point: Vec3, normal: Vec3, roll: f32) -> Mesh {
    let basis = Quat::from_rotation_arc(Vec3::Z, normal.normalize()) * Quat::from_rotation_z(roll);
    let mut positions = Vec::new();
    let mut normals = Vec::new();
    let mut uv = Vec::new();
    let mut indices = Vec::new();
    const N: u32 = 8;
    for y in 0..=N {
        for x in 0..=N {
            let tex = Vec2::new(x as f32, y as f32) / N as f32;
            let p = point + basis * ((tex - Vec2::splat(0.5)) * 0.23).extend(0.0);
            let axis = Vec3::new(
                0.0,
                p.y.clamp(-PLAYER_CAPSULE_LENGTH * 0.5, PLAYER_CAPSULE_LENGTH * 0.5),
                0.0,
            );
            let n = (p - axis).try_normalize().unwrap_or(normal);
            positions.push((axis + n * (PLAYER_RADIUS + 0.003)).to_array());
            normals.push(n.to_array());
            uv.push(tex.to_array());
            if x < N && y < N {
                let i = y * (N + 1) + x;
                indices.extend_from_slice(&[i, i + 1, i + N + 2, i, i + N + 2, i + N + 1]);
            }
        }
    }
    Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::default(),
    )
    .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, positions)
    .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, normals)
    .with_inserted_attribute(Mesh::ATTRIBUTE_UV_0, uv)
    .with_inserted_indices(Indices::U32(indices))
}

impl BloodEffects {
    fn reset(&mut self, commands: &mut Commands) {
        for (_, e) in self.wounds.drain(..) {
            commands.entity(e).try_despawn();
        }
        for e in self.stains.drain(..).chain(self.particles.drain(..)) {
            commands.entity(e).try_despawn();
        }
        self.seen.clear();
        self.history.clear();
    }

    pub fn present(
        &mut self,
        commands: &mut Commands,
        meshes: &mut Assets<Mesh>,
        state: &ShooterGameState,
        players: &crate::client::PlayerEntities,
    ) {
        if self.round != Some(state.round) {
            self.reset(commands);
            self.round = Some(state.round);
        }
        for (id, shot) in &state.shots {
            if !self.seen.insert(*id) {
                continue;
            }
            self.history.push_back(*id);
            if self.history.len() > 512 {
                self.seen.remove(&self.history.pop_front().unwrap());
            }
            let Some(hit) = &shot.body_hit else {
                continue;
            };
            let Some(parent) = players.0.get(&hit.player) else {
                continue;
            };
            let mut seed = id.wrapping_add(93);
            for position in [hit.local_entry, hit.local_exit] {
                let normal = capsule_normal(position);
                let entity = commands
                    .spawn((
                        Wound,
                        Name::new("Blood wound"),
                        Mesh3d(meshes.add(wound_mesh(
                            position,
                            normal,
                            random(&mut seed) * std::f32::consts::TAU,
                        ))),
                        MeshMaterial3d(self.material.clone()),
                        Transform::default(),
                        NotShadowCaster,
                    ))
                    .id();
                commands.entity(*parent).add_child(entity);
                self.wounds.push_back((hit.player, entity));
            }
            while self
                .wounds
                .iter()
                .filter(|(owner, _)| *owner == hit.player)
                .count()
                > WOUNDS_PER_PLAYER
            {
                let index = self
                    .wounds
                    .iter()
                    .position(|(owner, _)| *owner == hit.player)
                    .unwrap();
                commands
                    .entity(self.wounds.remove(index).unwrap().1)
                    .try_despawn();
            }
            let Some(body) = state.players.get(&hit.player) else {
                continue;
            };
            let rotation = if body.death.is_some() {
                Quat::from_array(body.body_rotation)
            } else {
                Quat::from_rotation_y((-body.look_xz.x).atan2(-body.look_xz.y))
            };
            let entry = body.pos + rotation * hit.local_entry;
            let exit = body.pos + rotation * hit.local_exit;
            let forward = (exit - entry).normalize_or_zero();
            for i in 0..12 {
                // Forward exit spray, with a smaller burst outward from the entrance.
                let (origin, direction) = if i < 9 {
                    (exit, forward)
                } else {
                    (entry, rotation * capsule_normal(hit.local_entry))
                };
                let spread = Vec3::new(
                    random(&mut seed) - 0.5,
                    random(&mut seed) - 0.5,
                    random(&mut seed) - 0.5,
                ) * 2.3;
                let velocity =
                    body.vel + direction * (3.0 + random(&mut seed) * 3.5) + spread + Vec3::Y * 0.4;
                let entity = commands
                    .spawn((
                        Droplet {
                            velocity,
                            accumulator: 0.0,
                            age: 0.0,
                            seed,
                            source: Some(entry),
                        },
                        Mesh3d(self.drop_mesh.clone()),
                        MeshMaterial3d(self.drop_material.clone()),
                        Transform::from_translation(origin + direction * 0.008)
                            .with_scale(Vec3::splat(0.012 + random(&mut seed) * 0.012)),
                        NotShadowCaster,
                    ))
                    .id();
                self.particles.push_back(entity);
            }
            while self.particles.len() > PARTICLE_LIMIT {
                commands
                    .entity(self.particles.pop_front().unwrap())
                    .try_despawn();
            }
        }
    }

    fn stain(&mut self, commands: &mut Commands, position: Vec3, normal: Vec3, seed: &mut u32) {
        let size = 0.10 + random(seed) * 0.16;
        let entity = commands
            .spawn((
                BloodStain,
                ForwardDecal,
                MeshMaterial3d(self.stain.clone()),
                Transform::from_translation(position + normal * 0.004)
                    .with_rotation(
                        Quat::from_rotation_arc(Vec3::Y, normal)
                            * Quat::from_rotation_y(random(seed) * std::f32::consts::TAU),
                    )
                    .with_scale(Vec3::splat(size)),
                NotShadowCaster,
            ))
            .id();
        self.stains.push_back(entity);
        while self.stains.len() > STAIN_LIMIT {
            commands
                .entity(self.stains.pop_front().unwrap())
                .try_despawn();
        }
    }
}

fn animate(
    mut commands: Commands,
    time: Res<Time>,
    collision: Res<BloodCollision>,
    mut effects: ResMut<BloodEffects>,
    mut particles: Query<(Entity, &mut Transform, &mut Droplet)>,
) {
    for (entity, mut transform, mut droplet) in &mut particles {
        // A body can briefly overlap a wall during correction: do not emit through it.
        if let Some(source) = droplet.source.take()
            && let Some((position, normal)) = collision.cast(source, transform.translation)
        {
            effects.stain(&mut commands, position, normal, &mut droplet.seed);
            commands.entity(entity).despawn();
            continue;
        }
        droplet.accumulator += time.delta_secs().min(0.1);
        while droplet.accumulator >= STEP {
            droplet.accumulator -= STEP;
            droplet.age += STEP;
            let end = transform.translation
                + droplet.velocity * STEP
                + Vec3::NEG_Y * (4.905 * STEP * STEP);
            if let Some((position, normal)) = collision.cast(transform.translation, end) {
                effects.stain(&mut commands, position, normal, &mut droplet.seed);
                commands.entity(entity).despawn();
                break;
            }
            transform.translation = end;
            droplet.velocity.y -= 9.81 * STEP;
            if droplet.age >= 1.5 {
                commands.entity(entity).despawn();
                break;
            }
        }
    }
}

fn clear(mut commands: Commands, mut effects: ResMut<BloodEffects>) {
    effects.reset(&mut commands);
    effects.round = None;
}

#[cfg(test)]
mod tests {
    use bevy::ecs::world::CommandQueue;
    use shooter_airsoft::shooter_logic::{BodyHit, PlayerState, Shot};

    use super::*;

    #[test]
    fn swept_droplets_hit_nearest_thin_plywood_and_floor() {
        let thin = |x| Solid {
            collider: Collider::cuboid(0.02, 3.0, 3.0),
            center: Vec3::new(x, 0.0, 0.0),
            rotation: Quat::IDENTITY,
            min: Vec3::new(x - 0.01, -1.5, -1.5),
            max: Vec3::new(x + 0.01, 1.5, 1.5),
        };
        let collision = BloodCollision(vec![thin(2.0), thin(0.0)]);
        let (p, n) = collision.cast(Vec3::NEG_X, Vec3::X * 3.0).unwrap();
        assert!((p.x + 0.01).abs() < 0.00001);
        assert!(n.dot(Vec3::NEG_X) > 0.999);
        assert!(
            collision
                .cast(Vec3::new(-1.0, 4.0, 0.0), Vec3::new(3.0, 4.0, 0.0))
                .is_none()
        );
        let (p, n) = BloodCollision::default()
            .cast(Vec3::new(0.0, 2.0, 14.4), Vec3::new(0.0, -1.0, 14.4))
            .unwrap();
        assert!(p.y.abs() < 0.2 && n.y > 0.99);
    }

    #[test]
    fn wound_mesh_follows_capsule_including_rounded_ends() {
        for point in [
            Vec3::X * PLAYER_RADIUS,
            Vec3::Y * (PLAYER_CAPSULE_LENGTH * 0.5 + PLAYER_RADIUS),
            Vec3::new(0.2, -PLAYER_CAPSULE_LENGTH * 0.5 - 0.2, 0.2),
        ] {
            let mesh = wound_mesh(point, capsule_normal(point), 1.4);
            let positions = mesh
                .attribute(Mesh::ATTRIBUTE_POSITION)
                .unwrap()
                .as_float3()
                .unwrap();
            for p in positions {
                let p = Vec3::from_array(*p);
                let axis = Vec3::new(
                    0.0,
                    p.y.clamp(-PLAYER_CAPSULE_LENGTH * 0.5, PLAYER_CAPSULE_LENGTH * 0.5),
                    0.0,
                );
                assert!((p.distance(axis) - PLAYER_RADIUS - 0.003).abs() < 0.0001);
            }
        }
    }

    #[test]
    fn events_deduplicate_effects_are_bounded_and_reset_cleans_children() {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, AssetPlugin::default()))
            .init_asset::<Mesh>()
            .init_asset::<bevy::shader::Shader>()
            .add_plugins(bevy::pbr::decal::ForwardDecalPlugin);
        let mut world = std::mem::take(app.world_mut());
        let parent = world.spawn(Transform::default()).id();
        let player = Pubkey::new_from_array([2; 32]);
        let players =
            crate::client::PlayerEntities(std::collections::HashMap::from([(player, parent)]));
        let mut state = ShooterGameState::default();
        state.players.insert(player, PlayerState::default());
        let mut meshes = Assets::<Mesh>::default();
        let mut effects = BloodEffects {
            material: default(),
            stain: default(),
            drop_mesh: default(),
            drop_material: default(),
            round: None,
            seen: default(),
            history: default(),
            wounds: default(),
            stains: default(),
            particles: default(),
        };
        let mut queue = CommandQueue::default();
        for id in 0..80 {
            state.shots.clear();
            state.shots.insert(
                id,
                Shot {
                    body_hit: Some(BodyHit {
                        player,
                        local_entry: Vec3::X * PLAYER_RADIUS,
                        local_exit: Vec3::NEG_X * PLAYER_RADIUS,
                    }),
                    ..default()
                },
            );
            effects.present(
                &mut Commands::new(&mut queue, &world),
                &mut meshes,
                &state,
                &players,
            );
            // A replayed network snapshot must not emit the effects twice.
            effects.present(
                &mut Commands::new(&mut queue, &world),
                &mut meshes,
                &state,
                &players,
            );
            queue.apply(&mut world);
            assert_eq!(
                world
                    .query_filtered::<Entity, With<Wound>>()
                    .iter(&world)
                    .count(),
                ((id as usize + 1) * 2).min(WOUNDS_PER_PLAYER)
            );
            assert_eq!(
                world
                    .query_filtered::<Entity, With<Droplet>>()
                    .iter(&world)
                    .count(),
                ((id as usize + 1) * 12).min(PARTICLE_LIMIT)
            );
        }
        for _ in 0..160 {
            effects.stain(
                &mut Commands::new(&mut queue, &world),
                Vec3::ZERO,
                Vec3::Y,
                &mut 123,
            );
        }
        queue.apply(&mut world);
        assert_eq!(
            world
                .query_filtered::<Entity, With<BloodStain>>()
                .iter(&world)
                .count(),
            STAIN_LIMIT
        );
        state.round += 1;
        state.shots.clear();
        effects.present(
            &mut Commands::new(&mut queue, &world),
            &mut meshes,
            &state,
            &players,
        );
        queue.apply(&mut world);
        assert_eq!(
            world
                .query_filtered::<Entity, With<Wound>>()
                .iter(&world)
                .count(),
            0
        );
        assert_eq!(
            world
                .query_filtered::<Entity, With<Droplet>>()
                .iter(&world)
                .count(),
            0
        );
        assert_eq!(
            world
                .query_filtered::<Entity, With<BloodStain>>()
                .iter(&world)
                .count(),
            0
        );
        assert!(
            world.get_entity(parent).is_ok(),
            "reset must preserve the player"
        );
    }
}
