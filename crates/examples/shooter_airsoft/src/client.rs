use std::{collections::HashMap, path::PathBuf};

use bevy::{
    ecs::message::MessageReader,
    input::mouse::MouseMotion,
    prelude::*,
    window::{CursorGrabMode, CursorOptions, PrimaryWindow},
};
use bevy_egui::{EguiPlugin, EguiPrimaryContextPass};
use bevy_egui_notify::EguiToastsPlugin;
use deform_core::{
    DeformClient, Pubkey,
    accounts::lobby::{
        Lobby, LobbyMetadata, LobbyState, Network, PlayerStatus, Web2Server,
        not_started::LobbyNotStarted,
    },
};
use deform_offline::new_offline_client;
use deform_quic::netem::FakeNetwork;
use shooter_airsoft::{shooter_logic::*, solana::anchor_client::ShooterAnchorClient};
use solana_sdk::{signature::read_keypair_file, signer::Signer};
use tokio_util::sync::CancellationToken;

use crate::menu::{MenuState, egui_in_game, egui_in_menu};

/// Optional keypair path from `--wallet`, consumed by [`setup`] to pre-load the
/// menu's keypair so the CLI can skip the in-app "Load" step.
#[derive(Resource, Default)]
pub struct WalletArg(pub Option<PathBuf>);

/// Our player's network latency (ms), copied each frame from the active backend's
/// shared stats ([`update_state`]) so egui can display it without locking the backend.
#[derive(Resource, Default)]
pub struct NetStats {
    pub ping_ms: f64,
}

/// The camera's orientation, owned entirely by the local mouse. This is the point
/// of the example's input design: a first-person camera driven by tick-rate game
/// state would feel awful no matter how good the smoothing is. So the mouse turns
/// this every render frame with zero latency, rollbacks never touch it, and each
/// frame [`update_inputs`] quantizes it into [`ShooterInputs`] as "I am looking
/// this way" — which is all the simulation ever learns about the camera.
#[derive(Resource, Default)]
pub struct CameraOrientation {
    pub yaw: f32,
    pub pitch: f32,
}

/// The inputs being composed this frame, pushed to the backend by [`mouse_look`]
/// after every mouse movement and again by [`send_inputs`] at the end of the frame.
#[derive(Resource, Default)]
pub struct CurrentInputs(pub ShooterInputs);

/// Which pubkey is "me" — used to place the first-person camera and hide my own
/// capsule.
#[derive(Resource)]
pub struct LocalPlayer(pub Pubkey);

/// When on, our inputs come from the offline bot instead of the keyboard and mouse.
#[derive(Resource, Default)]
pub struct BotEnabled(pub bool);

#[derive(Resource)]
#[repr(transparent)]
pub struct MultiplayerClient(pub DeformClient<ShooterGame>);

#[derive(Resource)]
#[repr(transparent)]
pub struct BackendCancellationToken(pub CancellationToken);

/// Mesh/material handles for entities spawned mid-game, plus the camera entity.
#[derive(Resource)]
pub struct SceneAssets {
    pub level: Handle<Gltf>,
    pub camera: Entity,
    pub player_mesh: Handle<Mesh>,
    pub player_material: Handle<StandardMaterial>,
    pub outline_material: Handle<crate::killcam::OutlineMaterial>,
    pub headband_mesh: Handle<Mesh>,
    pub team_materials: [Handle<StandardMaterial>; 2],
}

/// Player capsules are spawned/despawned dynamically by diffing the game
/// state each frame, keyed the same way the state is.
#[derive(Resource, Default)]
pub struct PlayerEntities(pub HashMap<Pubkey, Entity>);

#[derive(Component)]
pub struct PlayerCapsule(pub Pubkey);

#[derive(Component)]
pub struct TeamHeadband;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default, States)]
pub enum AppState {
    #[default]
    MainMenu,
    InGame,
}

const MOUSE_SENSITIVITY: f32 = 0.002;

pub fn run_game(wallet: Option<PathBuf>, offline: bool, smoke_test: bool) {
    let mut app = App::new();
    app.insert_resource(WalletArg(wallet))
        .add_plugins(
            DefaultPlugins
                .set(AssetPlugin {
                    file_path: std::env::var("AIRSOFT_ASSET_DIR")
                        .unwrap_or_else(|_| concat!(env!("CARGO_MANIFEST_DIR"), "/assets").into()),
                    ..default()
                })
                .set(WindowPlugin {
                    primary_window: Some(Window {
                        title: "BOMB HOUSE / Airsoft".into(),
                        resolution: (1280, 800).into(),
                        resizable: !smoke_test,
                        ..default()
                    }),
                    ..default()
                }),
        )
        .add_plugins(MaterialPlugin::<crate::killcam::OutlineMaterial>::default())
        .add_plugins(crate::lighting::ArenaLightingPlugin)
        .init_resource::<crate::killcam::PlayerView>()
        .add_plugins(bevy::diagnostic::FrameTimeDiagnosticsPlugin::default())
        .add_plugins(EguiPlugin::default())
        .add_plugins(EguiToastsPlugin::default())
        .init_state::<AppState>()
        .init_resource::<NetStats>()
        .init_resource::<CameraOrientation>()
        .init_resource::<CurrentInputs>()
        .init_resource::<BotEnabled>()
        .init_resource::<PlayerEntities>()
        .insert_resource(LaunchOptions {
            offline,
            smoke_test,
        })
        .init_resource::<crate::smoke::SmokeProgress>()
        .add_systems(
            Startup,
            (setup, crate::effects::setup_feedback, auto_start).chain(),
        )
        .add_systems(
            Update,
            (
                crate::effects::animate_feedback,
                screenshot_key,
                crate::smoke::smoke_update.after(send_inputs),
            ),
        )
        .add_systems(
            Update,
            (cursor_grab, update_inputs, mouse_look, send_inputs)
                .chain()
                .run_if(in_state(AppState::InGame)),
        )
        .add_systems(
            EguiPrimaryContextPass,
            (
                egui_in_menu.run_if(in_state(AppState::MainMenu)),
                egui_in_game.run_if(in_state(AppState::InGame)),
            ),
        )
        .add_systems(OnEnter(AppState::InGame), grab_cursor_on_enter)
        .add_systems(
            PostUpdate,
            update_state
                .before(bevy::transform::TransformSystems::Propagate)
                .run_if(in_state(AppState::InGame)),
        )
        .add_systems(
            PostUpdate,
            crate::smoke::preview_remote_weapon
                .after(update_state)
                .before(bevy::transform::TransformSystems::Propagate),
        )
        .add_systems(PreUpdate, crate::round_smoke::advance)
        .add_systems(
            PostUpdate,
            crate::round_smoke::preview_corpse
                .after(update_state)
                .before(bevy::transform::TransformSystems::Propagate),
        )
        .add_systems(Update, on_app_exit);
    if std::env::var_os("AIRSOFT_PERF_PROBE").is_some() {
        app.add_plugins(bevy::render::diagnostic::RenderDiagnosticsPlugin)
            .init_resource::<crate::perf_probe::PerfProbe>()
            .add_systems(Update, crate::perf_probe::sample);
    }
    app.run();
}

#[derive(Clone)]
pub struct NetworkPreset {
    pub name: &'static str,
    pub rpc_url: &'static str,
}

/// The QUIC server address a `Network::Web2` lobby plays on.
///
/// The lobby records only *which* server (`Web2Server`), the same way it records a
/// region for the ephemeral rollup -- resolving that to an address is a client-side
/// concern, so the deployed host never has to live in this repo:
///
/// 1. `$DEFORM_SERVER_ADDR`
/// 2. `server.addr` in the working directory
/// 3. `server.addr` next to this crate -- written by `deploy.sh` on every deploy
///
/// All three are gitignored. Empty means the remote is not configured on this
/// machine, which the menu reports rather than silently dialing localhost.
pub fn web2_server_addr(server: &Web2Server) -> String {
    match server {
        Web2Server::Localhost => "127.0.0.1:4433".to_string(),
        Web2Server::Remote => std::env::var("DEFORM_SERVER_ADDR")
            .ok()
            .or_else(|| std::fs::read_to_string("server.addr").ok())
            .or_else(|| {
                std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/server.addr")).ok()
            })
            .map(|addr| addr.trim().to_string())
            .unwrap_or_default(),
    }
}

pub const NETWORK_PRESETS: &[NetworkPreset] = &[
    NetworkPreset {
        name: "Localhost",
        rpc_url: "http://127.0.0.1:8899",
    },
    NetworkPreset {
        name: "Devnet",
        rpc_url: "https://api.devnet.solana.com",
    },
    NetworkPreset {
        name: "Mainnet",
        rpc_url: "https://api.mainnet-beta.solana.com",
    },
];

pub fn scan_json_files() -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(".") else {
        return Vec::new();
    };
    let mut files: Vec<String> = entries
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            if name.ends_with(".json") {
                Some(name)
            } else {
                None
            }
        })
        .collect();
    files.sort();
    files
}

pub fn setup(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut outlines: ResMut<Assets<crate::killcam::OutlineMaterial>>,
    wallet: Res<WalletArg>,
    assets: Res<AssetServer>,
) -> Result<()> {
    // Camera looks at the arena from above until a match starts; then
    // [`update_state`] snaps it to the local player's eyes each frame.
    let camera = commands
        .spawn((
            Camera3d::default(),
            Projection::Perspective(PerspectiveProjection {
                fov: 80.0_f32.to_radians(),
                near: 0.04,
                ..default()
            }),
            Transform::from_xyz(0.6, 2.0, 14.4).looking_at(Vec3::new(0.6, 1.7, 0.0), Vec3::Y),
            Msaa::Sample4,
            bevy::core_pipeline::prepass::DepthPrepass,
            bevy::post_process::bloom::Bloom::NATURAL,
            SpatialListener::new(0.2),
            AmbientLight {
                color: Color::WHITE,
                brightness: 220.0,
                affects_lightmapped_meshes: false,
            },
        ))
        .id();

    commands.spawn((
        Name::new("Bomb house / Blender export"),
        WorldAssetRoot(assets.load(GltfAssetLabel::Scene(0).from_asset("levels/bomb_house.glb"))),
    ));

    commands.insert_resource(SceneAssets {
        level: assets.load("levels/bomb_house.glb"),
        camera,
        player_mesh: meshes.add(Capsule3d::new(PLAYER_RADIUS, PLAYER_CAPSULE_LENGTH)),
        outline_material: outlines.add(crate::killcam::OutlineMaterial {}),
        headband_mesh: meshes.add(Torus::new(0.30, 0.41)),
        // Match the Blender spawn stripes: Team A blue, Team B orange.
        team_materials: [
            Color::linear_rgb(0.035, 0.24, 0.42),
            Color::linear_rgb(0.67, 0.18, 0.045),
        ]
        .map(|base_color| {
            materials.add(StandardMaterial {
                base_color,
                perceptual_roughness: 0.95,
                ..default()
            })
        }),
        player_material: materials.add(StandardMaterial {
            base_color: Color::srgb(0.2, 0.7, 0.3),
            ..default()
        }),
    });

    // Pre-load the keypair from `--wallet` if given, otherwise leave the menu on
    // its manual "Load" flow. A bad path is non-fatal: we log and fall back.
    let keypair_files = scan_json_files();
    let mut selected_keypair_idx = 0;
    let mut keypair = None;
    if let Some(path) = &wallet.0 {
        match read_keypair_file(path) {
            Ok(kp) => {
                info!("Loaded wallet {}: {}", path.display(), kp.pubkey());
                if let Some(idx) = path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .and_then(|name| keypair_files.iter().position(|f| f == name))
                {
                    selected_keypair_idx = idx;
                }
                keypair = Some(std::sync::Arc::new(kp));
            }
            Err(e) => error!("Failed to load wallet {}: {e}", path.display()),
        }
    }

    commands.insert_resource(MenuState {
        fake_network: None,
        keypair_files,
        selected_keypair_idx,
        keypair,
        selected_preset_idx: 0,
        rpc_client: None,
        program_client: ShooterAnchorClient,
        network: Network::Web2(Web2Server::Localhost),
        lobby_id: 0,
        lobby_id_text: "0".into(),
        lobby_data: None,
        skip_cert_verify: true,
    });

    Ok(())
}

pub(crate) fn make_backend_lobby(main_player: Pubkey, bot_player: Pubkey) -> Lobby<ShooterGame> {
    let mut player_status = std::collections::BTreeMap::new();
    for pk in [main_player, bot_player] {
        player_status.insert(pk, PlayerStatus::Ready);
    }

    Lobby {
        metadata: LobbyMetadata {
            id: 0,
            creator: main_player,
            network: Network::Web2(Web2Server::Localhost),
            bump: 0,
        },
        state: LobbyState::NotStarted(LobbyNotStarted { player_status }),
    }
}

pub fn start_offline(
    commands: &mut Commands,
    main_player: Pubkey,
    visual_tick_micros: u64,
    bot_active: bool,
) -> Result<()> {
    let bot_player = Pubkey::new_from_array([255; 32]);
    let lobby = make_backend_lobby(main_player, bot_player);
    initialize_aim(commands, &lobby, main_player);

    let cancellation_token = CancellationToken::new();
    commands.insert_resource(BackendCancellationToken(cancellation_token.clone()));

    let client = new_offline_client::<ShooterGame>(
        main_player,
        lobby,
        if bot_active {
            shooter_bot
        } else {
            |_, _, _| ShooterInputs::default()
        },
        visual_tick_micros,
        cancellation_token,
    )?;
    commands.insert_resource(MultiplayerClient(client));
    commands.insert_resource(LocalPlayer(main_player));

    Ok(())
}

pub fn start_online(
    commands: &mut Commands,
    lobby: Lobby<ShooterGame>,
    main_player: Pubkey,
    server_addr: &str,
    skip_cert_verify: bool,
    fake_network: Option<FakeNetwork>,
    visual_tick_micros: u64,
) -> Result<()> {
    initialize_aim(commands, &lobby, main_player);
    let cancellation_token = CancellationToken::new();
    commands.insert_resource(BackendCancellationToken(cancellation_token.clone()));

    let client = deform_quic::new_quic_client::<ShooterQuicLogic>(
        server_addr.to_string(),
        server_addr
            .split(':')
            .next()
            .unwrap_or(server_addr)
            .to_string(),
        lobby,
        main_player,
        skip_cert_verify,
        visual_tick_micros,
        NoAuth,
        cancellation_token,
        fake_network,
    )?;
    commands.insert_resource(MultiplayerClient(client));
    commands.insert_resource(LocalPlayer(main_player));

    Ok(())
}

pub fn grab_cursor_on_enter(mut cursor: Single<&mut CursorOptions, With<PrimaryWindow>>) {
    cursor.grab_mode = CursorGrabMode::Locked;
    cursor.visible = false;
}

/// Escape releases the cursor (so the egui windows can be used); clicking into
/// the window grabs it again for mouse-look.
pub fn cursor_grab(
    mut cursor: Single<&mut CursorOptions, With<PrimaryWindow>>,
    keys: Res<ButtonInput<KeyCode>>,
    mouse: Res<ButtonInput<MouseButton>>,
) {
    if keys.just_pressed(KeyCode::Escape) {
        cursor.grab_mode = CursorGrabMode::None;
        cursor.visible = true;
    } else if mouse.just_pressed(MouseButton::Left) && cursor.grab_mode == CursorGrabMode::None {
        cursor.grab_mode = CursorGrabMode::Locked;
        cursor.visible = false;
    }
}

/// Straight from the mouse to the camera — no netcode in between, see
/// [`CameraOrientation`] — and, on the way past, one input sample per movement.
///
/// The raw [`MouseMotion`] stream is read rather than `AccumulatedMouseMotion`
/// because a mouse reports far faster than the game renders: at a 1000 Hz polling
/// rate a 60 Hz tick sees on the order of sixteen movements. Pushing after each one
/// means whichever movement the tick boundary falls between is the aim the
/// simulation uses, instead of an end-of-frame value that can be a whole frame
/// stale. Only one sample per tick survives — [`ShooterInputs::merge`] decides
/// which — so the extra pushes cost a channel send, not bandwidth.
#[allow(clippy::too_many_arguments)]
pub fn mouse_look(
    client: ResMut<MultiplayerClient>,
    mut current: ResMut<CurrentInputs>,
    mut motion: MessageReader<MouseMotion>,
    mut orientation: ResMut<CameraOrientation>,
    cursor: Single<&CursorOptions, With<PrimaryWindow>>,
    scene: Res<SceneAssets>,
    mut transforms: Query<&mut Transform>,
    bot: Res<BotEnabled>,
    view: Res<crate::killcam::PlayerView>,
) -> Result<()> {
    if view.dead || bot.0 || cursor.grab_mode == CursorGrabMode::None {
        motion.clear();
        return Ok(());
    }

    for motion in motion.read() {
        orientation.yaw -= motion.delta.x * MOUSE_SENSITIVITY;
        orientation.pitch =
            (orientation.pitch - motion.delta.y * MOUSE_SENSITIVITY).clamp(-1.54, 1.54); // just short of straight up/down

        current.0.set_look(orientation.yaw, orientation.pitch);
        client.0.set_inputs(current.0.clone())?;
    }

    if let Ok(mut camera_transform) = transforms.get_mut(scene.camera) {
        camera_transform.rotation =
            Quat::from_euler(EulerRot::YXZ, orientation.yaw, orientation.pitch, 0.0);
    }

    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub fn update_inputs(
    mut current: ResMut<CurrentInputs>,
    orientation: Res<CameraOrientation>,
    keys: Res<ButtonInput<KeyCode>>,
    mouse: Res<ButtonInput<MouseButton>>,
    cursor: Single<&CursorOptions, With<PrimaryWindow>>,
    bot: Res<BotEnabled>,
    client: Res<MultiplayerClient>,
    local: Res<LocalPlayer>,
    view: Res<crate::killcam::PlayerView>,
) -> Result<()> {
    if view.dead {
        current.0 = ShooterInputs::default();
        return Ok(());
    }
    if bot.0 {
        let state = client.0.read_state()?;
        if let LobbyState::Ongoing(ongoing) = &state.lobby.state {
            current.0 = shooter_bot(&ongoing.tick_info.game_state, &local.0, &current.0);
        }
        return Ok(());
    }

    let grabbed = cursor.grab_mode != CursorGrabMode::None;

    let mut move_x: i8 = 0;
    let mut move_z: i8 = 0;
    if grabbed {
        if keys.pressed(KeyCode::KeyW) {
            move_z += 100;
        }
        if keys.pressed(KeyCode::KeyS) {
            move_z -= 100;
        }
        if keys.pressed(KeyCode::KeyD) {
            move_x += 100;
        }
        if keys.pressed(KeyCode::KeyA) {
            move_x -= 100;
        }
    }

    current.0.move_x = move_x;
    current.0.move_z = move_z;
    // `just_pressed` as well as `pressed`: a click that starts and ends inside one
    // frame leaves `pressed` false, and dropping it would eat the shot outright.
    current.0.fire =
        grabbed && (mouse.pressed(MouseButton::Left) || mouse.just_pressed(MouseButton::Left));
    current.0.jump = grabbed && (keys.pressed(KeyCode::Space) || keys.just_pressed(KeyCode::Space));
    current.0.set_look(orientation.yaw, orientation.pitch);
    Ok(())
}

/// The frame's closing sample. Unconditional: a tick that receives nothing has its
/// inputs predicted by repeating the previous tick's, so staying quiet — even while
/// idle — is a guess, and this is the one push guaranteed to carry the keyboard
/// state that [`mouse_look`] never sees on a frame with no mouse movement.
pub fn send_inputs(client: ResMut<MultiplayerClient>, current: Res<CurrentInputs>) -> Result<()> {
    client.0.set_inputs(current.0.clone())?;
    Ok(())
}

/// Mirror the (already smoothed) game state into the scene: diff-spawn capsules
/// move them, present shot events, and pin the camera to the local player's eyes.
#[allow(clippy::too_many_arguments)]
pub fn update_state(
    mut commands: Commands,
    client: Res<MultiplayerClient>,
    local: Res<LocalPlayer>,
    scene: Res<SceneAssets>,
    mut orientation: ResMut<CameraOrientation>,
    mut view: ResMut<crate::killcam::PlayerView>,
    mut visibility: Query<&mut Visibility>,
    outlines: Query<(Entity, &crate::killcam::KillerOutline)>,
    mut player_entities: ResMut<PlayerEntities>,
    mut effects: ResMut<crate::effects::ShotEffects>,
    weapon_model: Res<crate::effects::WeaponModel>,
    remote_weapons: Query<(Entity, &crate::effects::RemoteWeapon)>,
    mut transforms: Query<&mut Transform>,
    mut net_stats: ResMut<NetStats>,
) -> Result<()> {
    // Snapshot under one short lock; copy latency out before any early return.
    let lobby = {
        let state = client.0.read_state()?;
        net_stats.ping_ms = state.stats.ping_ms;
        state.lobby.clone()
    };

    let game_state = match &lobby.state {
        LobbyState::NotStarted(_) => return Ok(()),
        LobbyState::Finished(finished) => &finished.0.tick_info.game_state,
        LobbyState::Ongoing(ongoing) => &ongoing.tick_info.game_state,
    };

    let me = game_state.players.get(&local.0);
    view.dead = me.is_some_and(|p| p.death.is_some());
    if view.round != Some(game_state.round) {
        view.round = Some(game_state.round);
        if let Some(me) = me {
            orientation.yaw = (-me.look_xz.x).atan2(-me.look_xz.y);
            orientation.pitch = me.pitch;
        }
    }
    let killer = me.and_then(|p| p.death.as_ref()).map(|death| death.killer);

    // Match the sorted lobby-key order used by new_game_from_lobby/spawn_for_slot.
    // HashMap iteration order is different on each client and cannot assign teams.
    let mut players: Vec<_> = game_state.players.iter().collect();
    players.sort_unstable_by_key(|(pk, _)| **pk);
    for (slot, (pk, ps)) in players.into_iter().enumerate() {
        let yaw = (-ps.look_xz.x).atan2(-ps.look_xz.y);
        let position = if game_state.phase == RoundPhase::SpawnFreeze {
            spawn_for_slot(slot).0 // teleport immediately, without interpolation from the corpse
        } else {
            ps.pos
        };
        let rotation = if ps.death.is_some() {
            Quat::from_array(ps.body_rotation)
        } else {
            Quat::from_rotation_y(yaw)
        };
        let transform = Transform::from_translation(position).with_rotation(rotation);
        let body_visibility = if *pk == local.0 && ps.death.is_none() {
            Visibility::Hidden
        } else {
            Visibility::Visible
        };
        match player_entities.0.get(pk) {
            Some(entity) => {
                if let Ok(mut t) = transforms.get_mut(*entity) {
                    *t = transform;
                }
                if let Ok(mut visible) = visibility.get_mut(*entity) {
                    *visible = body_visibility;
                }
            }
            None => {
                let entity = commands
                    .spawn((
                        PlayerCapsule(*pk),
                        Mesh3d(scene.player_mesh.clone()),
                        MeshMaterial3d(scene.player_material.clone()),
                        transform,
                        body_visibility,
                    ))
                    .id();
                commands.entity(entity).with_children(|parent| {
                    parent.spawn((
                        crate::killcam::KillerOutline(*pk),
                        Mesh3d(scene.player_mesh.clone()),
                        MeshMaterial3d(scene.outline_material.clone()),
                        Transform::from_scale(Vec3::splat(1.015)),
                        Visibility::Hidden,
                        bevy::light::NotShadowCaster,
                    ));
                    parent.spawn((
                        TeamHeadband,
                        Name::new(if slot % 2 == 0 {
                            "Team A headband"
                        } else {
                            "Team B headband"
                        }),
                        Mesh3d(scene.headband_mesh.clone()),
                        MeshMaterial3d(scene.team_materials[slot % 2].clone()),
                        Transform::from_xyz(0.0, 0.68, 0.0),
                        Visibility::Inherited,
                    ));
                });
                weapon_model.spawn_remote(&mut commands, entity, *pk, ps.pitch);
                player_entities.0.insert(*pk, entity);
            }
        }
    }
    player_entities.0.retain(|pk, entity| {
        if game_state.players.contains_key(pk) {
            true
        } else {
            commands.entity(*entity).despawn();
            false
        }
    });

    for (entity, weapon) in &remote_weapons {
        if let Some(player) = game_state.players.get(&weapon.0)
            && let Ok(mut transform) = transforms.get_mut(entity)
        {
            transform.rotation = Quat::from_rotation_x(player.pitch);
        }
    }

    for (entity, outline) in &outlines {
        if let Ok(mut visible) = visibility.get_mut(entity) {
            *visible = if killer == Some(outline.0) {
                Visibility::Inherited
            } else {
                Visibility::Hidden
            };
        }
    }
    effects.present(&mut commands, game_state, local.0);

    // Camera position follows the local player's (smoothed) body; camera rotation
    // stays whatever the mouse last said — deliberately not read from the state.
    if let Some(me) = game_state.players.get(&local.0)
        && let Ok(mut camera_transform) = transforms.get_mut(scene.camera)
    {
        if let Some(death) = &me.death {
            if let Some(killer) = game_state.players.get(&death.killer) {
                *camera_transform = crate::killcam::death_camera(me, killer);
            } else {
                camera_transform.translation = death.camera_position;
            }
        } else {
            let position = if game_state.phase == RoundPhase::SpawnFreeze {
                let slot = game_state
                    .players
                    .keys()
                    .filter(|pk| **pk < local.0)
                    .count();
                spawn_for_slot(slot).0
            } else {
                me.pos
            };
            camera_transform.translation = position + Vec3::Y * PLAYER_EYE_HEIGHT;
            camera_transform.rotation =
                Quat::from_euler(EulerRot::YXZ, orientation.yaw, orientation.pitch, 0.0);
        }
    }

    Ok(())
}

pub fn on_app_exit(
    mut exits: MessageReader<AppExit>,
    cancellation_token: Option<Res<BackendCancellationToken>>,
) {
    for _exit in exits.read() {
        if let Some(token) = &cancellation_token {
            token.0.cancel();
        }
    }
}

#[derive(Resource)]
pub struct LaunchOptions {
    pub offline: bool,
    pub smoke_test: bool,
}

fn auto_start(
    mut commands: Commands,
    options: Res<LaunchOptions>,
    mut next: ResMut<NextState<AppState>>,
) -> Result<()> {
    if options.smoke_test && std::env::var_os("AIRSOFT_ROUND_SMOKE").is_some() {
        crate::round_smoke::start(&mut commands);
        next.set(AppState::InGame);
    } else if options.offline {
        start_offline(
            &mut commands,
            Pubkey::new_from_array([1; 32]),
            16_667,
            !options.smoke_test && std::env::var_os("AIRSOFT_PERF_PROBE").is_none(),
        )?;
        next.set(AppState::InGame);
    }
    Ok(())
}

fn screenshot_key(mut commands: Commands, keys: Res<ButtonInput<KeyCode>>) {
    if keys.just_pressed(KeyCode::F12) {
        commands
            .spawn(bevy::render::view::screenshot::Screenshot::primary_window())
            .observe(bevy::render::view::screenshot::save_to_disk(
                "airsoft-screenshot.png",
            ));
    }
}

fn initialize_aim(commands: &mut Commands, lobby: &Lobby<ShooterGame>, player: Pubkey) {
    let yaw = match &lobby.state {
        LobbyState::NotStarted(lobby) => {
            spawn_for_slot(
                lobby
                    .player_status
                    .keys()
                    .position(|p| *p == player)
                    .unwrap_or(0),
            )
            .1
        }
        LobbyState::Ongoing(lobby) => lobby
            .tick_info
            .game_state
            .players
            .get(&player)
            .map(|p| (-p.look_xz.x).atan2(-p.look_xz.y))
            .unwrap_or(0.0),
        LobbyState::Finished(_) => 0.0,
    };
    commands.insert_resource(CameraOrientation { yaw, pitch: 0.0 });
    let mut inputs = ShooterInputs::default();
    inputs.set_look(yaw, 0.0);
    commands.insert_resource(CurrentInputs(inputs));
}
