//! Bounded, cosmetic blood effects driven by authoritative flesh-hit events.
use std::collections::{HashSet, VecDeque};

use avian3d::prelude::*;
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

pub struct BloodPlugin;
impl Plugin for BloodPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(PhysicsPlugins::default())
            .insert_resource(Time::<Fixed>::from_hz(60.0))
            .add_systems(Startup, (setup, setup_colliders))
            .add_systems(
                FixedPostUpdate,
                splat_on_contact.after(PhysicsSystems::Last),
            )
            .add_systems(OnExit(crate::client::AppState::InGame), clear);
    }
}

#[derive(Component)]
pub struct Wound;
#[derive(Component)]
pub struct BloodStain;
#[derive(Component)]
pub struct Droplet {
    age: f32,
    seed: u32,
}

#[derive(Resource, Default)]
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

// This is the render app's cosmetic physics world. Gameplay has its own
// rollback world; droplets cannot push players or alter authoritative outcomes.
#[derive(Component)]
struct BloodSurface;
const SURFACE_LAYER: u32 = 1;
const DROPLET_LAYER: u32 = 2;

fn setup_colliders(mut commands: Commands) {
    for (center, size, rotation) in arena_data::SOLIDS {
        commands.spawn((
            BloodSurface,
            RigidBody::Static,
            Collider::cuboid(size[0], size[1], size[2]),
            CollisionLayers::from_bits(SURFACE_LAYER, DROPLET_LAYER),
            SpeculativeMargin(0.0),
            Transform::from_translation(Vec3::from_array(*center))
                .with_rotation(Quat::from_array(*rotation).normalize()),
        ));
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
                        Droplet { age: 0.0, seed },
                        RigidBody::Dynamic,
                        Collider::sphere(1.0), // scaled to the visible droplet radius below
                        LinearVelocity(velocity),
                        CollisionLayers::from_bits(DROPLET_LAYER, SURFACE_LAYER),
                        SpeculativeMargin(0.0),
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
        let size = (0.10 + random(seed) * 0.16) * 1.5;
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

fn splat_on_contact(
    mut commands: Commands,
    time: Res<Time<Fixed>>,
    collisions: Collisions,
    surfaces: Query<&Position, With<BloodSurface>>,
    mut effects: ResMut<BloodEffects>,
    mut particles: Query<(Entity, &mut Droplet)>,
) {
    for (entity, mut droplet) in &mut particles {
        let hit = collisions.collisions_with(entity).find_map(|pair| {
            let first = pair.collider1 == entity;
            let surface = if first {
                pair.collider2
            } else {
                pair.collider1
            };
            let center = surfaces.get(surface).ok()?.0;
            pair.manifolds.iter().find_map(|manifold| {
                // Discrete surface contact only; no ray casts or swept CCD.
                let point = manifold
                    .points
                    .iter()
                    .find(|point| point.penetration >= 0.0)?;
                let normal = if first {
                    -manifold.normal
                } else {
                    manifold.normal
                };
                let anchor = if first { point.anchor2 } else { point.anchor1 };
                Some((center + anchor, normal))
            })
        });
        if let Some((position, normal)) = hit {
            effects.stain(&mut commands, position, normal, &mut droplet.seed);
        }
        droplet.age += time.delta_secs();
        if hit.is_some() || droplet.age >= 1.5 {
            commands.entity(entity).despawn();
            effects.particles.retain(|particle| *particle != entity);
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
    fn avian_spheres_fall_and_create_surface_aligned_decals_on_contact() {
        for (surface_rotation, start, velocity, gravity) in [
            (Quat::IDENTITY, Vec3::Y * 0.3, Vec3::ZERO, 1.0),
            (
                Quat::from_rotation_z(std::f32::consts::FRAC_PI_2),
                Vec3::NEG_X * 0.3,
                Vec3::X,
                0.0,
            ),
        ] {
            let mut app = App::new();
            app.add_plugins((
                MinimalPlugins,
                AssetPlugin::default(),
                TransformPlugin,
                PhysicsPlugins::default(),
            ))
            .init_asset::<Mesh>()
            .init_asset::<bevy::shader::Shader>()
            .add_plugins(bevy::pbr::decal::ForwardDecalPlugin)
            .init_resource::<BloodEffects>()
            .insert_resource(Time::<Fixed>::from_hz(60.0))
            .insert_resource(bevy::time::TimeUpdateStrategy::ManualDuration(
                std::time::Duration::from_secs_f64(1.0 / 60.0),
            ))
            .add_systems(
                FixedPostUpdate,
                splat_on_contact.after(PhysicsSystems::Last),
            );
            app.world_mut().spawn((
                BloodSurface,
                RigidBody::Static,
                Collider::cuboid(4.0, 0.1, 4.0),
                CollisionLayers::from_bits(SURFACE_LAYER, DROPLET_LAYER),
                SpeculativeMargin(0.0),
                Transform::from_rotation(surface_rotation),
            ));
            let particle = app
                .world_mut()
                .spawn((
                    Droplet { age: 0.0, seed: 93 },
                    RigidBody::Dynamic,
                    Collider::sphere(1.0),
                    LinearVelocity(velocity),
                    GravityScale(gravity),
                    CollisionLayers::from_bits(DROPLET_LAYER, SURFACE_LAYER),
                    SpeculativeMargin(0.0),
                    Transform::from_translation(start).with_scale(Vec3::splat(0.02)),
                ))
                .id();
            app.world_mut()
                .resource_mut::<BloodEffects>()
                .particles
                .push_back(particle);
            app.finish();
            app.cleanup();
            for _ in 0..60 {
                app.update();
            }
            let world = app.world_mut();
            assert!(world.get_entity(particle).is_err());
            let mut query = world.query_filtered::<&Transform, With<BloodStain>>();
            let stains: Vec<_> = query.iter(world).collect();
            assert_eq!(stains.len(), 1, "first contact emits exactly one decal");
            let normal = surface_rotation * Vec3::Y;
            assert!((stains[0].translation.dot(normal) - 0.054).abs() < 0.002);
            assert!((stains[0].rotation * Vec3::Y).dot(normal) > 0.999);
            assert!(world.resource::<BloodEffects>().particles.is_empty());
        }
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
