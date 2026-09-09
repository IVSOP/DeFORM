//! Opt-in rendered verification, separate from the pure simulation tests.
use bevy::prelude::*;
use deform_core::accounts::lobby::LobbyState;

use crate::client::{CurrentInputs, LaunchOptions, LocalPlayer, MultiplayerClient};
#[derive(Resource, Default)]
pub struct SmokeProgress {
    frames: u32,
    captured: bool,
    saw_shot: bool,
    start: Option<Vec3>,
    impact_preview: Option<Transform>,
}

#[allow(clippy::too_many_arguments)]
pub fn smoke_update(
    mut commands: Commands,
    assets: Res<AssetServer>,
    scene: Res<crate::client::SceneAssets>,
    audio: Res<Assets<AudioSource>>,
    time: Res<Time>,
    options: Res<LaunchOptions>,
    mut progress: ResMut<SmokeProgress>,
    client: Option<Res<MultiplayerClient>>,
    local: Option<Res<LocalPlayer>>,
    mut inputs: ResMut<CurrentInputs>,
    meshes: Query<&Mesh3d>,
    lightmaps: Query<&bevy::pbr::Lightmap>,
    images: Res<Assets<Image>>,
    impacts: Query<&GlobalTransform, With<crate::effects::Impact>>,
    mut exit: MessageWriter<AppExit>,
) {
    if !options.smoke_test || std::env::var_os("AIRSOFT_ROUND_SMOKE").is_some() {
        return;
    }
    if let Some(bevy::asset::LoadState::Failed(error)) = assets.get_load_state(scene.level.id()) {
        panic!("arena asset failed to load: {error}");
    }
    progress.frames += 1;
    if progress.frames > 3600 || time.elapsed_secs() > 90.0 {
        panic!("render smoke timed out waiting for loaded arena and shot feedback");
    }
    if meshes.iter().count() < 500 || audio.is_empty() {
        return;
    }
    if lightmaps.iter().count() < 700
        || lightmaps
            .iter()
            .any(|bake| !images.contains(bake.image.id()))
    {
        return;
    }
    let (Some(client), Some(local)) = (client, local) else {
        return;
    };
    let snapshot = client.0.read_state().expect("read smoke state");
    if let LobbyState::Ongoing(ongoing) = &snapshot.lobby.state {
        let state = &ongoing.tick_info.game_state;
        if let Some(me) = state.players.get(&local.0) {
            progress.start.get_or_insert(me.pos);
            assert!(
                (0.5..3.0).contains(&me.pos.y),
                "player must stand on warehouse floor"
            );
        }
        progress.saw_shot |= !state.shots.is_empty();
    }
    drop(snapshot);
    // The normal input sampler runs too; submit this explicit sample after it.
    inputs.0.fire = true;
    client.0.set_inputs(inputs.0.clone()).expect("smoke fire");
    if progress.frames > 180 && progress.saw_shot && !impacts.is_empty() && !progress.captured {
        commands
            .spawn(bevy::render::view::screenshot::Screenshot::primary_window())
            .observe(bevy::render::view::screenshot::save_to_disk(
                "/tmp/airsoft-smoke.png",
            ));
        progress.impact_preview = impacts
            .iter()
            .next()
            .map(GlobalTransform::compute_transform);
        progress.captured = true;
        progress.frames = 0;
        info!(
            "AIRSOFT SMOKE: GLB and lightmaps loaded, grounded player, hitscan events and impact decals verified"
        );
    }
    if progress.captured && progress.frames > 210 {
        exit.write(AppExit::Success);
    }
}

/// Inspect the held weapon, team headbands, and both spawn signs after the first-person capture.
/// This changes only the test camera, and never runs during ordinary play.
pub fn preview_remote_weapon(
    mut commands: Commands,
    options: Res<LaunchOptions>,
    progress: Res<SmokeProgress>,
    weapons: Query<(&ChildOf, &InheritedVisibility), With<crate::effects::RemoteWeapon>>,
    bodies: Query<&GlobalTransform, With<crate::client::PlayerCapsule>>,
    bands: Query<&ChildOf, With<crate::client::TeamHeadband>>,
    mut cameras: Query<&mut Transform, With<Camera3d>>,
) {
    if !options.smoke_test || !progress.captured {
        return;
    }
    assert_eq!(
        bands.iter().count(),
        bodies.iter().count(),
        "each player needs a headband"
    );
    for band in &bands {
        assert!(
            bodies.contains(band.parent()),
            "headband must follow its player"
        );
    }
    if progress.frames > 120 {
        let impact = progress.impact_preview.expect("smoke must create a decal");
        let normal = impact.rotation * Vec3::Y;
        let up = if normal.y.abs() < 0.95 {
            Vec3::Y
        } else {
            Vec3::Z
        };
        let tangent = normal.cross(up).normalize();
        let (offset, capture_frame, path) = if progress.frames <= 165 {
            (normal * 0.55, 150, "/tmp/airsoft-decal-front.png")
        } else {
            (
                normal * 0.55 + tangent * 0.15,
                195,
                "/tmp/airsoft-decal-angle.png",
            )
        };
        for mut camera in &mut cameras {
            *camera = Transform::from_translation(impact.translation + offset)
                .looking_at(impact.translation, up);
        }
        if progress.frames == capture_frame {
            commands
                .spawn(bevy::render::view::screenshot::Screenshot::primary_window())
                .observe(bevy::render::view::screenshot::save_to_disk(path));
        }
        return;
    }
    if progress.frames > 45 {
        let (position, target, capture_frame, path) = if progress.frames <= 80 {
            (
                Vec3::new(0.0, 2.8, 11.0),
                Vec3::new(0.0, 3.2, 17.8),
                70,
                "/tmp/airsoft-sign-a.png",
            )
        } else {
            (
                Vec3::new(0.0, 2.3, -10.0),
                Vec3::new(0.0, 2.0, -15.9),
                105,
                "/tmp/airsoft-sign-b.png",
            )
        };
        for mut camera in &mut cameras {
            *camera = Transform::from_translation(position).looking_at(target, Vec3::Y);
        }
        if progress.frames == capture_frame {
            commands
                .spawn(bevy::render::view::screenshot::Screenshot::primary_window())
                .observe(bevy::render::view::screenshot::save_to_disk(path));
        }
        return;
    }
    let Some((parent, _)) = weapons.iter().find(|(_, visible)| visible.get()) else {
        panic!("remote player must have a held gun");
    };
    let body = bodies
        .get(parent.parent())
        .expect("gun is parented to its player");
    for mut camera in &mut cameras {
        *camera = Transform::from_translation(body.translation() + Vec3::new(2.0, 0.85, -1.0))
            .looking_at(body.translation() + Vec3::new(0.15, 0.3, -0.3), Vec3::Y);
    }
    if progress.frames == 30 {
        commands
            .spawn(bevy::render::view::screenshot::Screenshot::primary_window())
            .observe(bevy::render::view::screenshot::save_to_disk(
                "/tmp/airsoft-remote-weapon.png",
            ));
    }
}
