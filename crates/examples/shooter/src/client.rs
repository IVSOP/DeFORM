use std::{collections::HashMap, path::PathBuf};

use bevy::{
    ecs::message::MessageReader,
    input::mouse::AccumulatedMouseMotion,
    prelude::*,
    window::{CursorGrabMode, CursorOptions, PrimaryWindow},
};
use bevy_egui::{EguiPlugin, EguiPrimaryContextPass};
use bevy_egui_notify::EguiToastsPlugin;
use deform_core::{
    DeformClient, Pubkey,
    accounts::lobby::{
        Lobby, LobbyMetadata, LobbyState, Network, PlayerStatus, not_started::LobbyNotStarted,
    },
};
use deform_offline::new_offline_client;
use shooter::{shooter_logic::*, solana::anchor_client::ShooterAnchorClient};
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

/// The inputs being composed this frame, sent to the backend by [`send_inputs`].
#[derive(Resource, Default)]
pub struct CurrentInputs(pub ShooterInputs);

/// Which pubkey is "me" — used to place the first-person camera and hide my own
/// capsule.
#[derive(Resource)]
pub struct LocalPlayer(pub Pubkey);

#[derive(Resource)]
#[repr(transparent)]
pub struct MultiplayerClient(pub DeformClient<ShooterGame>);

#[derive(Resource)]
#[repr(transparent)]
pub struct BackendCancellationToken(pub CancellationToken);

/// Mesh/material handles for entities spawned mid-game, plus the camera entity.
#[derive(Resource)]
pub struct SceneAssets {
    pub camera: Entity,
    pub player_mesh: Handle<Mesh>,
    pub player_material: Handle<StandardMaterial>,
    pub projectile_mesh: Handle<Mesh>,
    pub projectile_material: Handle<StandardMaterial>,
}

/// Capsules and spheres are spawned/despawned dynamically by diffing the game
/// state each frame, keyed the same way the state is.
#[derive(Resource, Default)]
pub struct PlayerEntities(pub HashMap<Pubkey, Entity>);

#[derive(Resource, Default)]
pub struct ProjectileEntities(pub HashMap<u32, Entity>);

#[derive(Component)]
pub struct PlayerCapsule(pub Pubkey);

#[derive(Component)]
pub struct ProjectileBall(pub u32);

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default, States)]
pub enum AppState {
    #[default]
    MainMenu,
    InGame,
}

const MOUSE_SENSITIVITY: f32 = 0.002;

pub fn run_game(wallet: Option<PathBuf>) {
    let mut app = App::new();
    app.insert_resource(WalletArg(wallet))
        .add_plugins((DefaultPlugins,))
        .add_plugins(EguiPlugin::default())
        .add_plugins(EguiToastsPlugin::default())
        .init_state::<AppState>()
        .init_resource::<NetStats>()
        .init_resource::<CameraOrientation>()
        .init_resource::<CurrentInputs>()
        .init_resource::<PlayerEntities>()
        .init_resource::<ProjectileEntities>()
        .add_systems(Startup, setup)
        .add_systems(
            Update,
            (cursor_grab, mouse_look, update_inputs, send_inputs)
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
        .add_systems(PostUpdate, update_state.run_if(in_state(AppState::InGame)))
        .add_systems(Update, on_app_exit)
        .run();
}

#[derive(Clone)]
pub struct NetworkPreset {
    pub name: &'static str,
    pub rpc_url: &'static str,
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
    wallet: Res<WalletArg>,
) -> Result<()> {
    // Camera looks at the arena from above until a match starts; then
    // [`update_state`] snaps it to the local player's eyes each frame.
    let camera = commands
        .spawn((
            Camera3d::default(),
            Transform::from_xyz(0.0, 25.0, 30.0).looking_at(Vec3::ZERO, Vec3::Y),
            AmbientLight {
                color: Color::WHITE,
                brightness: 300.0,
                ..default()
            },
        ))
        .id();

    commands.spawn((
        DirectionalLight {
            illuminance: 8_000.0,
            shadow_maps_enabled: true,
            ..default()
        },
        Transform::from_xyz(10.0, 30.0, 10.0).looking_at(Vec3::ZERO, Vec3::Y),
    ));

    // The arena, matching the sim's static colliders: a floor and 4 walls.
    let floor_material = materials.add(StandardMaterial {
        base_color: Color::srgb(0.25, 0.3, 0.28),
        perceptual_roughness: 0.95,
        ..default()
    });
    let wall_material = materials.add(StandardMaterial {
        base_color: Color::srgb(0.45, 0.42, 0.38),
        perceptual_roughness: 0.9,
        ..default()
    });

    let wall_len_x = 2.0 * ARENA_HALF_X + 2.0 * WALL_THICKNESS;
    let wall_len_z = 2.0 * ARENA_HALF_Z + 2.0 * WALL_THICKNESS;
    let floor = meshes.add(Cuboid::new(wall_len_x, FLOOR_THICKNESS, wall_len_z));
    commands.spawn((
        Mesh3d(floor),
        MeshMaterial3d(floor_material),
        Transform::from_xyz(0.0, -FLOOR_THICKNESS / 2.0, 0.0),
    ));

    let wall_x = meshes.add(Cuboid::new(WALL_THICKNESS, WALL_HEIGHT, wall_len_z));
    let wall_z = meshes.add(Cuboid::new(wall_len_x, WALL_HEIGHT, WALL_THICKNESS));
    for (mesh, pos) in [
        (
            wall_x.clone(),
            Vec3::new(ARENA_HALF_X + WALL_THICKNESS / 2.0, WALL_HEIGHT / 2.0, 0.0),
        ),
        (
            wall_x,
            Vec3::new(-ARENA_HALF_X - WALL_THICKNESS / 2.0, WALL_HEIGHT / 2.0, 0.0),
        ),
        (
            wall_z.clone(),
            Vec3::new(0.0, WALL_HEIGHT / 2.0, ARENA_HALF_Z + WALL_THICKNESS / 2.0),
        ),
        (
            wall_z,
            Vec3::new(0.0, WALL_HEIGHT / 2.0, -ARENA_HALF_Z - WALL_THICKNESS / 2.0),
        ),
    ] {
        commands.spawn((
            Mesh3d(mesh),
            MeshMaterial3d(wall_material.clone()),
            Transform::from_translation(pos),
        ));
    }

    commands.insert_resource(SceneAssets {
        camera,
        player_mesh: meshes.add(Capsule3d::new(PLAYER_RADIUS, PLAYER_CAPSULE_LENGTH)),
        player_material: materials.add(StandardMaterial {
            base_color: Color::srgb(0.2, 0.7, 0.3),
            ..default()
        }),
        projectile_mesh: meshes.add(Sphere::new(PROJECTILE_RADIUS)),
        projectile_material: materials.add(StandardMaterial {
            base_color: Color::srgb(0.9, 0.6, 0.1),
            emissive: LinearRgba::rgb(2.0, 1.2, 0.2),
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
        keypair_files,
        selected_keypair_idx,
        keypair,
        selected_preset_idx: 0,
        rpc_client: None,
        program_client: ShooterAnchorClient,
        network: Network::Web2,
        lobby_id: 0,
        lobby_id_text: "0".into(),
        lobby_data: None,
        server_addr: "127.0.0.1:4433".into(),
        skip_cert_verify: true,
    });

    Ok(())
}

fn make_backend_lobby(main_player: Pubkey, bot_player: Pubkey) -> Lobby<ShooterGame> {
    let mut player_status = std::collections::BTreeMap::new();
    for pk in [main_player, bot_player] {
        player_status.insert(pk, PlayerStatus::Ready);
    }

    Lobby {
        metadata: LobbyMetadata {
            id: 0,
            creator: main_player,
            network: Network::Web2,
            bump: 0,
        },
        state: LobbyState::NotStarted(LobbyNotStarted { player_status }),
    }
}

pub fn start_offline(
    commands: &mut Commands,
    main_player: Pubkey,
    visual_tick_micros: u64,
) -> Result<()> {
    let bot_player = Pubkey::new_from_array([255; 32]);
    let lobby = make_backend_lobby(main_player, bot_player);

    let cancellation_token = CancellationToken::new();
    commands.insert_resource(BackendCancellationToken(cancellation_token.clone()));

    let client = new_offline_client::<ShooterGame>(
        main_player,
        lobby,
        shooter_bot,
        visual_tick_micros,
        cancellation_token,
    )?;
    commands.insert_resource(MultiplayerClient(client));
    commands.insert_resource(LocalPlayer(main_player));
    commands.insert_resource(CurrentInputs::default());

    Ok(())
}

pub fn start_online(
    commands: &mut Commands,
    lobby: Lobby<ShooterGame>,
    main_player: Pubkey,
    server_addr: &str,
    skip_cert_verify: bool,
    visual_tick_micros: u64,
) -> Result<()> {
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
    )?;
    commands.insert_resource(MultiplayerClient(client));
    commands.insert_resource(LocalPlayer(main_player));
    commands.insert_resource(CurrentInputs::default());

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

/// Runs every render frame, straight from the mouse to the camera — no netcode in
/// between, see [`CameraOrientation`].
pub fn mouse_look(
    motion: Res<AccumulatedMouseMotion>,
    mut orientation: ResMut<CameraOrientation>,
    cursor: Single<&CursorOptions, With<PrimaryWindow>>,
    scene: Res<SceneAssets>,
    mut transforms: Query<&mut Transform>,
) {
    if cursor.grab_mode == CursorGrabMode::None {
        return;
    }

    orientation.yaw -= motion.delta.x * MOUSE_SENSITIVITY;
    orientation.pitch = (orientation.pitch - motion.delta.y * MOUSE_SENSITIVITY).clamp(-1.54, 1.54); // just short of straight up/down

    if let Ok(mut camera_transform) = transforms.get_mut(scene.camera) {
        camera_transform.rotation =
            Quat::from_euler(EulerRot::YXZ, orientation.yaw, orientation.pitch, 0.0);
    }
}

pub fn update_inputs(
    mut current: ResMut<CurrentInputs>,
    orientation: Res<CameraOrientation>,
    keys: Res<ButtonInput<KeyCode>>,
    mouse: Res<ButtonInput<MouseButton>>,
    cursor: Single<&CursorOptions, With<PrimaryWindow>>,
) {
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
    current.0.fire = grabbed && mouse.pressed(MouseButton::Left);
    current.0.jump = grabbed && keys.pressed(KeyCode::Space);
    current.0.set_look(orientation.yaw, orientation.pitch);
}

pub fn send_inputs(client: ResMut<MultiplayerClient>, current: Res<CurrentInputs>) -> Result<()> {
    client.0.set_inputs(current.0.clone())?;
    Ok(())
}

/// Mirror the (already smoothed) game state into the scene: diff-spawn capsules
/// and spheres, move them, and pin the camera to the local player's eyes.
#[allow(clippy::too_many_arguments)]
pub fn update_state(
    mut commands: Commands,
    client: Res<MultiplayerClient>,
    local: Res<LocalPlayer>,
    scene: Res<SceneAssets>,
    orientation: Res<CameraOrientation>,
    mut player_entities: ResMut<PlayerEntities>,
    mut projectile_entities: ResMut<ProjectileEntities>,
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
        // Freeze the last frame; the HUD announces the result.
        LobbyState::Finished(_) => return Ok(()),
        LobbyState::Ongoing(ongoing) => &ongoing.tick_info.game_state,
    };

    // players
    for (pk, ps) in game_state.players.iter() {
        let yaw = (-ps.look_xz.x).atan2(-ps.look_xz.y);
        let transform =
            Transform::from_translation(ps.pos).with_rotation(Quat::from_rotation_y(yaw));
        match player_entities.0.get(pk) {
            Some(entity) => {
                if let Ok(mut t) = transforms.get_mut(*entity) {
                    *t = transform;
                }
            }
            None => {
                // First-person: the local player's own capsule is never rendered.
                let visibility = if *pk == local.0 {
                    Visibility::Hidden
                } else {
                    Visibility::Visible
                };
                let entity = commands
                    .spawn((
                        PlayerCapsule(*pk),
                        Mesh3d(scene.player_mesh.clone()),
                        MeshMaterial3d(scene.player_material.clone()),
                        transform,
                        visibility,
                    ))
                    .id();
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

    // projectiles
    for (id, projectile) in game_state.projectiles.iter() {
        match projectile_entities.0.get(id) {
            Some(entity) => {
                if let Ok(mut t) = transforms.get_mut(*entity) {
                    t.translation = projectile.pos;
                }
            }
            None => {
                let entity = commands
                    .spawn((
                        ProjectileBall(*id),
                        Mesh3d(scene.projectile_mesh.clone()),
                        MeshMaterial3d(scene.projectile_material.clone()),
                        Transform::from_translation(projectile.pos),
                    ))
                    .id();
                projectile_entities.0.insert(*id, entity);
            }
        }
    }
    projectile_entities.0.retain(|id, entity| {
        if game_state.projectiles.contains_key(id) {
            true
        } else {
            commands.entity(*entity).despawn();
            false
        }
    });

    // Camera position follows the local player's (smoothed) body; camera rotation
    // stays whatever the mouse last said — deliberately not read from the state.
    if let Some(me) = game_state.players.get(&local.0) {
        if let Ok(mut camera_transform) = transforms.get_mut(scene.camera) {
            camera_transform.translation = me.pos + Vec3::Y * PLAYER_EYE_HEIGHT;
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
