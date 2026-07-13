use std::collections::HashMap;

use anyhow::anyhow;
use bevy::prelude::*;
use bevy_egui::{EguiPlugin, EguiPrimaryContextPass};
use bevy_egui_notify::EguiToastsPlugin;
use deform_core::{
    DeformClient, Pubkey,
    accounts::lobby::{
        Lobby, LobbyMetadata, LobbyState, Network, PlayerStatus, not_started::LobbyNotStarted,
    },
};
use deform_offline::new_offline_client;

use pong::pong_logic::*;

pub fn run_game() {
    let mut app = App::new();
    app.add_plugins((DefaultPlugins,))
        .add_plugins(EguiPlugin::default())
        .add_plugins(EguiToastsPlugin::default())
        .init_state::<AppState>()
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
        .add_systems(PostUpdate, update_state.run_if(in_state(AppState::InGame)))
        .run();
}

use pong::solana::anchor_client::PongAnchorClient;

use crate::menu::{MenuState, egui_in_game, egui_in_menu};

#[derive(Component)]
pub struct Ball;

#[derive(Component)]
#[repr(transparent)]
pub struct Player(Pubkey);

#[derive(Resource)]
#[repr(transparent)]
pub struct MultiplayerClient(pub DeformClient<PongGame>);

#[derive(Resource)]
#[repr(transparent)]
pub struct PlayerEntities(HashMap<Pubkey, Entity>);

/// The two pre-spawned paddle entities, by field side.
#[derive(Resource)]
pub struct PaddleSlots {
    pub left: Entity,
    pub right: Entity,
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
    mut materials: ResMut<Assets<ColorMaterial>>,
) -> Result<()> {
    commands.spawn((
        Camera2d,
        Projection::Orthographic(OrthographicProjection {
            scaling_mode: bevy::camera::ScalingMode::AutoMin {
                min_width: FIELD_W,
                min_height: FIELD_H,
            },
            ..OrthographicProjection::default_2d()
        }),
    ));

    let border_thickness = 4.0;
    let border_material = materials.add(Color::LinearRgba(LinearRgba::WHITE));
    let h_border = meshes.add(Rectangle::new(FIELD_W + border_thickness, border_thickness));
    let v_border = meshes.add(Rectangle::new(border_thickness, FIELD_H + border_thickness));
    for (mesh, x, y) in [
        (h_border.clone(), 0.0, FIELD_H / 2.0),
        (h_border, 0.0, -FIELD_H / 2.0),
        (v_border.clone(), -FIELD_W / 2.0, 0.0),
        (v_border, FIELD_W / 2.0, 0.0),
    ] {
        commands.spawn((
            Mesh2d(mesh),
            MeshMaterial2d(border_material.clone()),
            Transform::from_translation(Vec3::new(x, y, 0.0)),
        ));
    }

    let ball_shape = meshes.add(Circle::new(BALL_HALF));
    let player_shape = meshes.add(Rectangle::new(PADDLE_W, PADDLE_H));
    let material = materials.add(Color::LinearRgba(LinearRgba::GREEN));

    commands.spawn((
        Ball,
        Mesh2d(ball_shape),
        MeshMaterial2d(material.clone()),
        Transform::default(),
    ));

    commands.insert_resource(MenuState {
        keypair_files: scan_json_files(),
        selected_keypair_idx: 0,
        keypair: None,
        selected_preset_idx: 0,
        rpc_client: None,
        program_client: PongAnchorClient,
        network: Network::Web2,
        lobby_id: 0,
        lobby_id_text: "0".into(),
        lobby_data: None,
        server_addr: "127.0.0.1:4433".into(),
        skip_cert_verify: true,
    });

    // Pre-spawn paddle slots (hidden until a game starts and assigns players)
    let left = commands
        .spawn((
            Player(Pubkey::default()),
            PongInputs { direction: 0 },
            Mesh2d(player_shape.clone()),
            MeshMaterial2d(material.clone()),
            Transform::from_translation(Vec3::new(-PADDLE_X, 0.0, 0.0)),
            Visibility::Hidden,
        ))
        .id();

    let right = commands
        .spawn((
            Player(Pubkey::default()),
            Mesh2d(player_shape),
            MeshMaterial2d(material),
            Transform::from_translation(Vec3::new(PADDLE_X, 0.0, 0.0)),
            Visibility::Hidden,
        ))
        .id();

    commands.insert_resource(PaddleSlots { left, right });
    commands.insert_resource(PlayerEntities(HashMap::new()));

    Ok(())
}

pub fn start_offline(
    commands: &mut Commands,
    main_player: Pubkey,
    player_entities: &mut ResMut<PlayerEntities>,
    slots: &PaddleSlots,
    players_q: &mut Query<(&mut Player, &mut Visibility)>,
    visual_tick_micros: u64,
) -> Result<()> {
    let bot_player = Pubkey::new_from_array([255; 32]);

    let mut player_status = HashMap::new();
    for pk in [main_player, bot_player] {
        player_status.insert(pk, PlayerStatus::Ready);
    }

    let lobby = Lobby {
        metadata: LobbyMetadata {
            id: 0,
            creator: Pubkey::default(),
            network: Network::Web2,
            bump: 0,
        },
        state: LobbyState::NotStarted(LobbyNotStarted { player_status }),
    };

    let client = new_offline_client::<PongGame>(main_player, lobby, pong_bot, visual_tick_micros)?;
    commands.insert_resource(MultiplayerClient(client));

    // Creator (main_player) is always on the left
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
    lobby: Lobby<PongGame>,
    main_player: Pubkey,
    server_addr: &str,
    skip_cert_verify: bool,
    player_entities: &mut ResMut<PlayerEntities>,
    slots: &PaddleSlots,
    players_q: &mut Query<(&mut Player, &mut Visibility)>,
    visual_tick_micros: u64,
) -> Result<()> {
    let creator = lobby.metadata.creator;

    // lobby might be not started or already started I think
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

    let client = deform_quic::new_quic_client::<PongQuicLogic>(
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
    )?;
    commands.insert_resource(MultiplayerClient(client));

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

pub fn update_inputs(inputs: Single<&mut PongInputs>, kb_input: Res<ButtonInput<KeyCode>>) {
    let mut new_direction: i8 = 0;
    if kb_input.pressed(KeyCode::KeyW) {
        new_direction += 100;
    }
    if kb_input.pressed(KeyCode::KeyS) {
        new_direction -= 100;
    }
    inputs.into_inner().direction = new_direction;
}

pub fn send_inputs(client: ResMut<MultiplayerClient>, inputs: Single<&PongInputs>) -> Result<()> {
    let inputs: PongInputs = inputs.clone();
    client.0.set_inputs(inputs)?;
    Ok(())
}

pub fn update_state(
    client: Res<MultiplayerClient>,
    mut players: Query<&mut Transform, (Without<Ball>, With<Player>)>,
    ball: Single<&mut Transform, (Without<Player>, With<Ball>)>,
    player_entities: Res<PlayerEntities>,
) -> Result<()> {
    let lobby = client.0.read_state()?.lobby.clone();

    // FIX: do something if game has ended!
    let ongoing = match lobby.state {
        LobbyState::NotStarted(_) => Err(anyhow!("Lobby has not started yet"))?, // FIX: just return Ok instead??
        LobbyState::Finished(_) => Err(anyhow!("Lobby has finished!!"))?,
        LobbyState::Ongoing(ongoing) => ongoing,
    };

    for (player, new_player_state) in ongoing.tick_info.game_state.players.iter() {
        let entity = player_entities
            .0
            .get(player)
            .ok_or(anyhow!("Player not found!"))?;
        let mut player_transform = players.get_mut(*entity)?;
        player_transform.translation.y = new_player_state.paddle_y;
    }

    ball.into_inner().translation = ongoing.tick_info.game_state.ball_pos.extend(0.0);
    Ok(())
}
