#[cfg(feature = "foc")]
use std::sync::Arc;
use std::{
    collections::{BTreeMap, HashMap},
    path::PathBuf,
};

use anyhow::anyhow;
use bevy::{ecs::message::MessageReader, prelude::*, sprite::Anchor};
use bevy_egui::{EguiPlugin, EguiPrimaryContextPass};
use bevy_egui_notify::EguiToastsPlugin;
#[cfg(feature = "foc")]
use deform_core::DeformUserLogic;
use deform_core::{
    DeformClient, Pubkey,
    accounts::lobby::{
        Lobby, LobbyMetadata, LobbyState, Network, PlayerStatus, ValidatorNetwork,
        not_started::LobbyNotStarted,
    },
};
use deform_offline::new_offline_client;
use soccer::{soccer_logic::*, solana::anchor_client::SoccerAnchorClient};
#[cfg(feature = "foc")]
use solana_sdk::signature::Keypair;
use solana_sdk::{signature::read_keypair_file, signer::Signer};
use tokio_util::sync::CancellationToken;

use crate::menu::{MenuState, egui_in_game, egui_in_menu};

// ─── Components & Resources ─────────────────────────────────────

#[derive(Component)]
pub struct Ball;

#[derive(Component)]
pub struct BallRotation(pub f32);

#[derive(Component)]
#[repr(transparent)]
pub struct Player(pub Pubkey);

#[derive(Component)]
pub struct PlayerAnimation {
    pub timer: Timer,
    pub frame_count: usize,
}

/// Which pubkey is "me", so the bot can be driven from our own point of view.
#[derive(Resource)]
#[repr(transparent)]
pub struct LocalPlayer(pub Pubkey);

/// When on, our inputs come from the offline bot instead of the keyboard.
#[derive(Resource, Default)]
#[repr(transparent)]
pub struct BotEnabled(pub bool);

#[derive(Resource)]
#[repr(transparent)]
pub struct MultiplayerClient(pub DeformClient<SoccerGame>);

#[derive(Resource)]
#[repr(transparent)]
pub struct PlayerEntities(pub HashMap<Pubkey, Entity>);

#[derive(Resource)]
#[repr(transparent)]
pub struct BackendCancellationToken(pub CancellationToken);

#[derive(Resource)]
pub struct PlayerSlots {
    pub left: Entity,
    pub right: Entity,
}

#[derive(Resource, Default)]
pub struct WalletArg(pub Option<PathBuf>);

#[derive(Resource, Default)]
pub struct NetStats {
    pub ping_ms: f64,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default, States)]
pub enum AppState {
    #[default]
    MainMenu,
    InGame,
}

#[derive(Clone)]
pub struct NetworkPreset {
    pub name: &'static str,
    pub rpc_url: &'static str,
    pub er_rpc_url: &'static str,
}

pub const NETWORK_PRESETS: &[NetworkPreset] = &[
    NetworkPreset {
        name: "Localhost",
        rpc_url: "http://127.0.0.1:8899",
        er_rpc_url: "http://127.0.0.1:7799",
    },
    NetworkPreset {
        name: "Devnet",
        rpc_url: "https://api.devnet.solana.com",
        er_rpc_url: "https://devnet.magicblock.app",
    },
    NetworkPreset {
        name: "Mainnet",
        rpc_url: "https://api.mainnet-beta.solana.com",
        er_rpc_url: "https://mainnet.magicblock.app",
    },
];

// ─── Camera offset: game floor is at y=0, camera centers on the field ───
const CAMERA_CENTER_Y: f32 = FIELD_H / 2.0;

pub fn run_game(wallet: Option<PathBuf>) {
    let mut app = App::new();
    app.insert_resource(WalletArg(wallet))
        .add_plugins(
            DefaultPlugins
                .set(ImagePlugin::default_nearest())
                .set(WindowPlugin {
                    primary_window: Some(Window {
                        resolution: bevy::window::WindowResolution::new(1240, 1080),
                        title: "Eggy League".to_string(),
                        ..default()
                    }),
                    ..default()
                }),
        )
        .add_plugins(EguiPlugin::default())
        .add_plugins(EguiToastsPlugin::default())
        .init_state::<AppState>()
        .init_resource::<NetStats>()
        .init_resource::<BotEnabled>()
        .add_systems(Startup, setup)
        .add_systems(
            Update,
            (update_inputs, send_inputs)
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
        .add_systems(
            PostUpdate,
            (update_state, animate_players)
                .chain()
                .run_if(in_state(AppState::InGame)),
        )
        .add_systems(Update, on_app_exit)
        .run();
}

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
    asset_server: Res<AssetServer>,
    mut texture_atlases: ResMut<Assets<TextureAtlasLayout>>,
    wallet: Res<WalletArg>,
) -> Result<()> {
    // camera
    commands.spawn((
        Camera2d,
        Projection::Orthographic(OrthographicProjection {
            scaling_mode: bevy::camera::ScalingMode::AutoMin {
                min_width: FIELD_W,
                min_height: FIELD_H,
            },
            ..OrthographicProjection::default_2d()
        }),
        Transform::from_translation(Vec3::new(0.0, CAMERA_CENTER_Y, 0.0)),
    ));

    // background — scale to cover the field area
    // Background image is 2560x1080. In the original game the camera was at y=405
    // (game units), with the background as a camera child at the default transform,
    // giving a 1:1 pixel-to-game-unit mapping. The game floor (y=0) was 135px above
    // the bottom of the image. We reproduce that mapping here.
    let bg_scale = FIELD_H / (1080.0 * 0.5);
    let floor_offset = 135.0 * bg_scale;
    commands.spawn((
        Sprite::from_image(asset_server.load("SolanaMap.png")),
        Transform::from_translation(Vec3::new(0.0, 1080.0 * bg_scale * 0.5 - floor_offset, 0.0))
            .with_scale(Vec3::splat(bg_scale)),
    ));

    // goals — anchor from the TOP at the crossbar, positioned at the goalpost.
    // The top of the image aligns with the crossbar top; the image extends
    // downward toward ground and leftward toward the wall.
    let crossbar_top = GOAL_HEIGHT + GOAL_THICKNESS / 2.0;

    // inner strips (z=1, behind ball at z=2)
    commands.spawn((
        Sprite::from_image(asset_server.load("LeftGoal2.png")),
        Anchor::TOP_RIGHT,
        Transform::from_translation(Vec3::new(LEFT_WALL + GOAL_WIDTH, crossbar_top, 1.0)),
    ));
    commands.spawn((
        Sprite::from_image(asset_server.load("RightGoal2.png")),
        Anchor::TOP_LEFT,
        Transform::from_translation(Vec3::new(RIGHT_WALL - GOAL_WIDTH, crossbar_top, 1.0)),
    ));

    // outer goal structure (z=5, in front of ball and players)
    commands.spawn((
        Sprite::from_image(asset_server.load("LeftGoal.png")),
        Anchor::TOP_RIGHT,
        Transform::from_translation(Vec3::new(LEFT_WALL + GOAL_WIDTH, crossbar_top, 5.0)),
    ));
    commands.spawn((
        Sprite::from_image(asset_server.load("RightGoal.png")),
        Anchor::TOP_LEFT,
        Transform::from_translation(Vec3::new(RIGHT_WALL - GOAL_WIDTH, crossbar_top, 5.0)),
    ));

    // ball (z=2)
    commands.spawn((
        Ball,
        BallRotation(0.0),
        Sprite::from_image(asset_server.load("football.png")),
        Transform::from_translation(Vec3::new(0.0, 300.0, 2.0)),
    ));

    // player sprite atlas: 160x32 = 5 frames of 32x32
    let player_texture: Handle<Image> = asset_server.load("walk_eggy.png");
    let layout = TextureAtlasLayout::from_grid(UVec2::new(32, 32), 5, 1, None, None);
    let layout_handle = texture_atlases.add(layout);

    let player_scale = PLAYER_RADIUS * 2.0 / 22.0; // sprite character is ~22px within the 32px frame

    // pre-spawn player slots (hidden until game starts)
    let left = commands
        .spawn((
            Player(Pubkey::default()),
            SoccerInputs::default(),
            PlayerAnimation {
                timer: Timer::from_seconds(0.1, TimerMode::Repeating),
                frame_count: 5,
            },
            Sprite {
                image: player_texture.clone(),
                texture_atlas: Some(TextureAtlas {
                    layout: layout_handle.clone(),
                    index: 0,
                }),
                ..default()
            },
            Transform::from_translation(Vec3::new(-300.0, PLAYER_RADIUS, 3.0))
                .with_scale(Vec3::splat(player_scale)),
            Visibility::Hidden,
        ))
        .id();

    let right = commands
        .spawn((
            Player(Pubkey::default()),
            PlayerAnimation {
                timer: Timer::from_seconds(0.1, TimerMode::Repeating),
                frame_count: 5,
            },
            Sprite {
                image: player_texture,
                texture_atlas: Some(TextureAtlas {
                    layout: layout_handle,
                    index: 0,
                }),
                ..default()
            },
            Transform::from_translation(Vec3::new(300.0, PLAYER_RADIUS, 3.0))
                .with_scale(Vec3::splat(player_scale)),
            Visibility::Hidden,
        ))
        .id();

    commands.insert_resource(PlayerSlots { left, right });
    commands.insert_resource(PlayerEntities(HashMap::new()));

    // keypair loading from --wallet
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
        program_client: SoccerAnchorClient,
        network: Network::FullyOnChain(ValidatorNetwork::default()),
        lobby_id: 0,
        lobby_id_text: "0".into(),
        lobby_data: None,
        server_addr: "127.0.0.1:4433".into(),
        skip_cert_verify: true,
    });

    Ok(())
}

// ─── Start game backends ────────────────────────────────────────

pub fn start_offline(
    commands: &mut Commands,
    main_player: Pubkey,
    player_entities: &mut ResMut<PlayerEntities>,
    slots: &PlayerSlots,
    players_q: &mut Query<(&mut Player, &mut Visibility)>,
    visual_tick_micros: u64,
) -> Result<()> {
    let bot_player = Pubkey::new_from_array([255; 32]);

    let mut player_status = BTreeMap::new();
    for pk in [main_player, bot_player] {
        player_status.insert(pk, PlayerStatus::Ready);
    }

    let lobby = Lobby {
        metadata: LobbyMetadata {
            id: 0,
            creator: main_player,
            network: Network::Web2,
            bump: 0,
        },
        state: LobbyState::NotStarted(LobbyNotStarted { player_status }),
    };

    let cancellation_token = CancellationToken::new();
    commands.insert_resource(BackendCancellationToken(cancellation_token.clone()));

    let client = new_offline_client::<SoccerGame>(
        main_player,
        lobby,
        soccer_bot,
        visual_tick_micros,
        cancellation_token,
    )?;
    commands.insert_resource(MultiplayerClient(client));
    commands.insert_resource(LocalPlayer(main_player));

    player_entities.0.clear();
    for (pk, entity) in [(main_player, slots.left), (bot_player, slots.right)] {
        if let Ok((mut p, mut vis)) = players_q.get_mut(entity) {
            p.0 = pk;
            *vis = Visibility::Visible;
        }
        player_entities.0.insert(pk, entity);
    }

    Ok(())
}

pub fn start_online(
    commands: &mut Commands,
    lobby: Lobby<SoccerGame>,
    main_player: Pubkey,
    server_addr: &str,
    skip_cert_verify: bool,
    player_entities: &mut ResMut<PlayerEntities>,
    slots: &PlayerSlots,
    players_q: &mut Query<(&mut Player, &mut Visibility)>,
    visual_tick_micros: u64,
) -> Result<()> {
    let creator = lobby.metadata.creator;

    let right_player = match &lobby.state {
        LobbyState::Finished(_) => Err(anyhow!("Lobby has already finished!"))?,
        LobbyState::NotStarted(not_started) => not_started
            .player_status
            .keys()
            .find(|pk| **pk != creator)
            .copied(),
        LobbyState::Ongoing(ongoing) => ongoing
            .tick_info
            .inputs
            .keys()
            .find(|pk| **pk != creator)
            .copied(),
    };

    let cancellation_token = CancellationToken::new();
    commands.insert_resource(BackendCancellationToken(cancellation_token.clone()));

    let client = deform_quic::new_quic_client::<SoccerQuicLogic>(
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

    player_entities.0.clear();

    if let Ok((mut p, mut vis)) = players_q.get_mut(slots.left) {
        p.0 = creator;
        *vis = Visibility::Visible;
    }
    player_entities.0.insert(creator, slots.left);

    if let Some(right) = right_player {
        if let Ok((mut p, mut vis)) = players_q.get_mut(slots.right) {
            p.0 = right;
            *vis = Visibility::Visible;
        }
        player_entities.0.insert(right, slots.right);
    }

    Ok(())
}

#[cfg(feature = "foc")]
pub fn start_online_foc(
    commands: &mut Commands,
    lobby: Lobby<SoccerGame>,
    keypair: &Keypair,
    player_entities: &mut ResMut<PlayerEntities>,
    slots: &PlayerSlots,
    players_q: &mut Query<(&mut Player, &mut Visibility)>,
    visual_tick_micros: u64,
) -> Result<()> {
    let creator = lobby.metadata.creator;

    let Network::FullyOnChain(validator_network) = &lobby.metadata.network else {
        return Err(anyhow!("start_online_foc requires a FullyOnChain lobby").into());
    };
    let endpoints = validator_network.er_endpoints();
    let slot_time_micros = <SoccerGame as DeformUserLogic>::get_micros_per_slot(validator_network);

    let right_player = match &lobby.state {
        LobbyState::Finished(_) => Err(anyhow!("Lobby has already finished!"))?,
        LobbyState::NotStarted(not_started) => not_started
            .player_status
            .keys()
            .find(|pk| **pk != creator)
            .copied(),
        LobbyState::Ongoing(ongoing) => ongoing
            .tick_info
            .inputs
            .keys()
            .find(|pk| **pk != creator)
            .copied(),
    };

    let cancellation_token = CancellationToken::new();
    commands.insert_resource(BackendCancellationToken(cancellation_token.clone()));

    let client = deform_foc::new_foc_client::<SoccerFocLogic>(
        endpoints.rpc.to_string(),
        endpoints.ws.to_string(),
        Arc::new(keypair.insecure_clone()),
        SoccerAnchorClient,
        lobby,
        visual_tick_micros,
        slot_time_micros,
        cancellation_token,
    )?;
    commands.insert_resource(MultiplayerClient(client));
    commands.insert_resource(LocalPlayer(Pubkey::new_from_array(
        keypair.pubkey().to_bytes(),
    )));

    player_entities.0.clear();

    if let Ok((mut p, mut vis)) = players_q.get_mut(slots.left) {
        p.0 = creator;
        *vis = Visibility::Visible;
    }
    player_entities.0.insert(creator, slots.left);

    if let Some(right) = right_player {
        if let Ok((mut p, mut vis)) = players_q.get_mut(slots.right) {
            p.0 = right;
            *vis = Visibility::Visible;
        }
        player_entities.0.insert(right, slots.right);
    }

    Ok(())
}

// ─── Input / update systems ─────────────────────────────────────

pub fn update_inputs(
    inputs: Single<&mut SoccerInputs>,
    kb_input: Res<ButtonInput<KeyCode>>,
    bot: Res<BotEnabled>,
    client: Res<MultiplayerClient>,
    local: Res<LocalPlayer>,
) -> Result<()> {
    if bot.0 {
        let mut inputs = inputs.into_inner();
        let state = client.0.read_state()?;
        if let LobbyState::Ongoing(ongoing) = &state.lobby.state {
            let bot_inputs = soccer_bot(&ongoing.tick_info.game_state, &local.0, &inputs);
            *inputs = bot_inputs;
        }
        return Ok(());
    }

    let mut horizontal: i8 = 0;
    if kb_input.pressed(KeyCode::KeyD) || kb_input.pressed(KeyCode::ArrowRight) {
        horizontal += 100;
    }
    if kb_input.pressed(KeyCode::KeyA) || kb_input.pressed(KeyCode::ArrowLeft) {
        horizontal -= 100;
    }

    let jump = kb_input.pressed(KeyCode::Space)
        || kb_input.pressed(KeyCode::KeyW)
        || kb_input.pressed(KeyCode::ArrowUp);

    let mut inputs = inputs.into_inner();
    inputs.horizontal = horizontal;
    inputs.jump = jump;
    Ok(())
}

pub fn send_inputs(client: ResMut<MultiplayerClient>, inputs: Single<&SoccerInputs>) -> Result<()> {
    let inputs: SoccerInputs = inputs.clone();
    client.0.set_inputs(inputs)?;
    Ok(())
}

pub fn update_state(
    client: Res<MultiplayerClient>,
    mut players: Query<(&mut Transform, &mut Sprite, &Player), Without<Ball>>,
    ball: Single<(&mut Transform, &mut BallRotation), (Without<Player>, With<Ball>)>,
    player_entities: Res<PlayerEntities>,
    mut net_stats: ResMut<NetStats>,
) -> Result<()> {
    let lobby = {
        let state = client.0.read_state()?;
        net_stats.ping_ms = state.stats.ping_ms;
        state.lobby.clone()
    };

    let ongoing = match lobby.state {
        LobbyState::NotStarted(_) => Err(anyhow!("Lobby has not started yet"))?,
        LobbyState::Finished(_) => Err(anyhow!("Lobby has finished!!"))?,
        LobbyState::Ongoing(ongoing) => ongoing,
    };

    let game_state = &ongoing.tick_info.game_state;

    // update players
    for (pk, player_state) in game_state.players.iter() {
        let Some(entity) = player_entities.0.get(pk) else {
            continue;
        };
        let Ok((mut transform, _, _)) = players.get_mut(*entity) else {
            continue;
        };
        transform.translation.x = player_state.pos.x;
        transform.translation.y = player_state.pos.y;

        // flip sprite based on direction
        let scale_sign = match player_state.dir {
            PlayerDir::Right => 1.0,
            PlayerDir::Left => -1.0,
        };
        transform.scale.x = transform.scale.x.abs() * scale_sign;
    }

    // update ball
    let (mut ball_transform, mut ball_rot) = ball.into_inner();
    ball_transform.translation.x = game_state.ball_pos.x;
    ball_transform.translation.y = game_state.ball_pos.y;

    // rotate ball based on x velocity
    let dt = 1.0 / 60.0; // visual frame rate for smooth rotation
    ball_rot.0 += game_state.ball_vel.x * dt * -0.005;
    ball_transform.rotation = Quat::from_rotation_z(ball_rot.0);

    Ok(())
}

pub fn animate_players(
    time: Res<Time>,
    client: Option<Res<MultiplayerClient>>,
    mut query: Query<(&mut PlayerAnimation, &mut Sprite, &Player)>,
) {
    let Some(client) = client else { return };
    let Ok(state) = client.0.read_state() else {
        return;
    };
    let ongoing = match &state.lobby.state {
        LobbyState::Ongoing(ongoing) => ongoing,
        _ => return,
    };

    for (mut anim, mut sprite, player) in &mut query {
        let Some(player_state) = ongoing.tick_info.game_state.players.get(&player.0) else {
            continue;
        };

        let is_moving = player_state.vel.x.abs() > 1.0;

        if is_moving {
            anim.timer.tick(time.delta());
            if anim.timer.just_finished() {
                if let Some(atlas) = &mut sprite.texture_atlas {
                    atlas.index = (atlas.index + 1) % anim.frame_count;
                }
            }
        } else {
            // idle: show frame 0
            if let Some(atlas) = &mut sprite.texture_atlas {
                atlas.index = 0;
            }
        }
    }
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
