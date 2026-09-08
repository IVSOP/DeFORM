//! Render-only shot feedback. Simulation snapshots carry discrete, bounded shot events.
use std::collections::{HashSet, VecDeque};

use bevy::{
    audio::{SpatialScale, Volume},
    light::NotShadowCaster,
    pbr::decal::{ForwardDecal, ForwardDecalMaterial, ForwardDecalMaterialExt},
    prelude::*,
};
use deform_core::Pubkey;
use shooter_airsoft::shooter_logic::{ShooterGameState, Shot};

use crate::client::{AppState, SceneAssets};

#[derive(Resource)]
pub struct ShotEffects {
    sound: Handle<AudioSource>,
    decal: Handle<ForwardDecalMaterial<StandardMaterial>>,
    seen: HashSet<u32>,
    history: VecDeque<u32>,
    decals: VecDeque<Entity>,
    pub recoil: f32,
    hit_flash: f32,
}
#[derive(Component)]
pub struct Weapon;
#[derive(Component)]
pub struct Crosshair;
#[derive(Component)]
pub struct CrosshairLine;
#[derive(Component)]
pub struct Impact;

/// Shared geometry for the camera weapon and weapons held by other players.
#[derive(Resource, Default)]
pub struct WeaponModel {
    parts: Vec<(Handle<Mesh>, Handle<StandardMaterial>, Transform)>,
    hands: Vec<(Handle<Mesh>, Handle<StandardMaterial>, Transform)>,
}

#[derive(Component)]
pub struct RemoteWeapon(pub Pubkey);

impl WeaponModel {
    fn attach(&self, commands: &mut Commands, parent: Entity, first_person: bool) {
        for (mesh, material, transform) in self
            .parts
            .iter()
            .chain(self.hands.iter().filter(|_| !first_person))
        {
            let mut part = commands.spawn((
                Mesh3d(mesh.clone()),
                MeshMaterial3d(material.clone()),
                *transform,
            ));
            if first_person {
                part.insert(NotShadowCaster);
            }
            let child = part.id();
            commands.entity(parent).add_child(child);
        }
    }

    pub fn spawn_remote(&self, commands: &mut Commands, player: Entity, owner: Pubkey, pitch: f32) {
        // The body supplies yaw; this shoulder pivot supplies pitch only.
        let weapon = commands
            .spawn((
                Name::new("Held airsoft AEG"),
                RemoteWeapon(owner),
                Transform::from_xyz(0.3, 0.4, -0.35).with_rotation(Quat::from_rotation_x(pitch)),
                Visibility::Inherited,
            ))
            .id();
        commands.entity(player).add_child(weapon);
        self.attach(commands, weapon, false);
    }
}

pub fn setup_feedback(
    mut commands: Commands,
    assets: Res<AssetServer>,
    scene: Res<SceneAssets>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut decals: ResMut<Assets<ForwardDecalMaterial<StandardMaterial>>>,
) {
    commands.insert_resource(ShotEffects {
        sound: assets.load("audio/aeg.wav"),
        decal: decals.add(ForwardDecalMaterial {
            base: StandardMaterial {
                base_color_texture: Some(assets.load("textures/impact.png")),
                // The extension enables pipeline blending, but StandardMaterial's
                // shader also needs Blend or it replaces texture alpha with 1.
                alpha_mode: AlphaMode::Blend,
                perceptual_roughness: 1.0,
                unlit: true,
                ..default()
            },
            extension: ForwardDecalMaterialExt {
                depth_fade_factor: 0.015,
            },
        }),
        seen: default(),
        history: default(),
        decals: default(),
        recoil: 0.0,
        hit_flash: 0.0,
    });
    // A compact original AEG silhouette, parented to the eye camera.
    let metal = materials.add(StandardMaterial {
        base_color: Color::srgb(0.065, 0.075, 0.08),
        metallic: 0.7,
        perceptual_roughness: 0.38,
        ..default()
    });
    let polymer = materials.add(StandardMaterial {
        base_color: Color::srgb(0.11, 0.10, 0.075),
        perceptual_roughness: 0.75,
        ..default()
    });
    let gun = commands
        .spawn((
            Weapon,
            Name::new("Airsoft AEG"),
            Transform::from_xyz(0.22, -0.22, -0.42),
            Visibility::Hidden,
        ))
        .id();
    commands.entity(scene.camera).add_child(gun);
    let mut model = WeaponModel::default();
    for (pos, size, mat) in [
        (Vec3::ZERO, Vec3::new(0.09, 0.11, 0.31), metal.clone()),
        (
            Vec3::new(0.0, -0.085, 0.11),
            Vec3::new(0.07, 0.16, 0.075),
            polymer.clone(),
        ),
        (
            Vec3::new(0.0, -0.13, -0.04),
            Vec3::new(0.065, 0.20, 0.095),
            metal.clone(),
        ),
        (
            Vec3::new(0.0, 0.01, -0.27),
            Vec3::new(0.08, 0.08, 0.23),
            polymer.clone(),
        ),
        (
            Vec3::new(0.0, 0.01, -0.44),
            Vec3::new(0.026, 0.026, 0.14),
            metal.clone(),
        ),
        (
            Vec3::new(0.0, 0.074, -0.03),
            Vec3::new(0.025, 0.035, 0.22),
            metal.clone(),
        ),
        (
            Vec3::new(0.0, 0.085, -0.36),
            Vec3::new(0.016, 0.07, 0.024),
            metal.clone(),
        ),
        (
            Vec3::new(0.0, -0.015, 0.25),
            Vec3::new(0.08, 0.14, 0.17),
            polymer.clone(),
        ),
    ] {
        model.parts.push((
            meshes.add(Cuboid::from_size(size)),
            mat,
            Transform::from_translation(pos),
        ));
    }
    for z in 0..7 {
        model.parts.push((
            meshes.add(Cuboid::new(0.088, 0.012, 0.012)),
            metal.clone(),
            Transform::from_xyz(0.0, 0.055, -0.17 - z as f32 * 0.029),
        ));
    }
    let gloves = materials.add(StandardMaterial {
        base_color: Color::srgb(0.18, 0.21, 0.12),
        perceptual_roughness: 0.95,
        ..default()
    });
    // A trigger hand and support hand make the shoulder-mounted weapon read as held.
    for (position, size) in [
        (Vec3::new(0.0, -0.09, 0.11), Vec3::new(0.10, 0.09, 0.10)),
        (Vec3::new(0.0, -0.045, -0.27), Vec3::new(0.11, 0.08, 0.12)),
    ] {
        model.hands.push((
            meshes.add(Cuboid::from_size(size)),
            gloves.clone(),
            Transform::from_translation(position),
        ));
    }
    model.attach(&mut commands, gun, true);
    commands.insert_resource(model);
    commands
        .spawn((
            Crosshair,
            Node {
                width: percent(100),
                height: percent(100),
                align_items: AlignItems::Center,
                justify_content: JustifyContent::Center,
                position_type: PositionType::Absolute,
                display: Display::None,
                ..default()
            },
        ))
        .with_children(|parent| {
            parent
                .spawn(Node {
                    width: px(20),
                    height: px(20),
                    ..default()
                })
                .with_children(|center| {
                    for (left, top, w, h) in
                        [(0, 9, 6, 2), (14, 9, 6, 2), (9, 0, 2, 6), (9, 14, 2, 6)]
                    {
                        center.spawn((
                            Node {
                                position_type: PositionType::Absolute,
                                left: px(left),
                                top: px(top),
                                width: px(w),
                                height: px(h),
                                ..default()
                            },
                            BackgroundColor(Color::WHITE),
                            CrosshairLine,
                        ));
                    }
                });
        });
}

impl ShotEffects {
    pub fn present(&mut self, commands: &mut Commands, state: &ShooterGameState, local: Pubkey) {
        for (id, shot) in &state.shots {
            if !self.seen.insert(*id) {
                continue;
            }
            self.history.push_back(*id);
            if self.history.len() > 512
                && let Some(id) = self.history.pop_front()
            {
                self.seen.remove(&id);
            }
            self.present_shot(commands, *id, shot, local);
        }
    }
    fn present_shot(&mut self, commands: &mut Commands, id: u32, shot: &Shot, local: Pubkey) {
        let is_local = shot.owner == local;
        commands.spawn((
            AudioPlayer::new(self.sound.clone()),
            PlaybackSettings::DESPAWN
                .with_spatial(!is_local)
                .with_spatial_scale(SpatialScale::new(0.15))
                .with_volume(Volume::Linear(if is_local { 0.65 } else { 0.9 }))
                .with_speed(0.96 + (id % 7) as f32 * 0.012),
            Transform::from_translation(shot.origin),
        ));
        if is_local {
            self.recoil = 1.0;
            if shot.hit_player {
                self.hit_flash = 0.16;
            }
        }
        if shot.hit_geometry && shot.normal.length_squared() > 0.5 {
            let e = commands
                .spawn((
                    Impact,
                    ForwardDecal,
                    MeshMaterial3d(self.decal.clone()),
                    Transform::from_translation(shot.impact + shot.normal * 0.008)
                        .with_rotation(
                            Quat::from_rotation_arc(Vec3::Y, shot.normal.normalize())
                                * Quat::from_rotation_y(id as f32 * 2.4),
                        )
                        .with_scale(Vec3::splat(0.12)),
                ))
                .id();
            self.decals.push_back(e);
            if self.decals.len() > 256
                && let Some(old) = self.decals.pop_front()
            {
                commands.entity(old).despawn();
            }
        }
    }
}

pub fn animate_feedback(
    time: Res<Time>,
    state: Res<State<AppState>>,
    mut effects: ResMut<ShotEffects>,
    mut guns: Query<(&mut Transform, &mut Visibility), With<Weapon>>,
    mut crosshairs: Query<&mut Node, With<Crosshair>>,
    mut colors: Query<&mut BackgroundColor, With<CrosshairLine>>,
) {
    let playing = *state.get() == AppState::InGame;
    effects.recoil = (effects.recoil - time.delta_secs() * 9.0).max(0.0);
    effects.hit_flash = (effects.hit_flash - time.delta_secs()).max(0.0);
    for (mut t, mut v) in &mut guns {
        *v = if playing {
            Visibility::Inherited
        } else {
            Visibility::Hidden
        };
        t.translation = Vec3::new(0.22, -0.22, -0.42 + effects.recoil * 0.045);
        t.rotation = Quat::from_rotation_x(effects.recoil * 0.035);
    }
    for mut node in &mut crosshairs {
        node.display = if playing {
            Display::Flex
        } else {
            Display::None
        };
    }
    for mut color in &mut colors {
        color.0 = if effects.hit_flash > 0.0 {
            Color::srgb(1.0, 0.3, 0.12)
        } else {
            Color::WHITE
        };
    }
}

#[cfg(test)]
mod tests {
    use shooter_airsoft::shooter_logic::ShooterInputs;

    use super::*;

    #[test]
    fn held_weapon_barrel_matches_network_aim_through_player_hierarchy() {
        for (yaw, pitch) in [(0.0, 0.0), (1.2, 0.65), (-2.4, -0.8), (3.1, 1.45)] {
            let mut input = ShooterInputs::default();
            input.set_look(yaw, pitch);
            let mut app = App::new();
            app.add_plugins(bevy::transform::TransformPlugin);
            let player = app
                .world_mut()
                .spawn(
                    Transform::from_xyz(5.0, 1.1, -3.0)
                        .with_rotation(Quat::from_rotation_y(input.yaw())),
                )
                .id();
            WeaponModel::default().spawn_remote(
                &mut app.world_mut().commands(),
                player,
                Pubkey::default(),
                input.pitch(),
            );
            app.world_mut().flush();
            app.update();
            let world = app.world_mut();
            let mut query = world.query_filtered::<&GlobalTransform, With<RemoteWeapon>>();
            let weapon = query.single(world).unwrap();
            assert!(
                weapon.forward().dot(input.look_dir()) > 0.99999,
                "barrel must point along the simulated ray for yaw {yaw}, pitch {pitch}"
            );
        }
    }
}
