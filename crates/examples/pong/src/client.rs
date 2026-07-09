use std::collections::HashMap;

use anyhow::anyhow;
use bevy::{prelude::*, window::Monitor};
use bevy_egui::{EguiContexts, EguiPlugin, EguiPrimaryContextPass, egui};
use bevy_egui_notify::{EguiToasts, EguiToastsPlugin};
use deform_core::{
    DeformClient, DeformUserLogic, Pubkey,
    accounts::{
        inputs::InputsAccount,
        lobby::{Lobby, Network},
    },
    game_program_client::{GameProgramClient, ReadyArgs},
};
use deform_offline::new_offline_client;
use solana_address::Address;
use solana_client::rpc_client::RpcClient;
use solana_sdk::{signature::Keypair, signer::Signer, signer::keypair::read_keypair_file};

use pong_logic::*;

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

use pong::{
    solana::anchor_client::{GAME_PROGRAM, PongAnchorClient},
    *,
};

use crate::send_and_confirm_tx;

#[derive(Component)]
pub struct Ball;

#[derive(Component)]
#[repr(transparent)]
pub struct Player(Pubkey);

#[derive(Resource)]
#[repr(transparent)]
pub struct MultiplayerClient(DeformClient<PongGame>);

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
struct NetworkPreset {
    name: &'static str,
    rpc_url: &'static str,
}

const NETWORK_PRESETS: &[NetworkPreset] = &[
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

#[derive(Resource)]
pub struct MenuState {
    keypair_files: Vec<String>,
    selected_keypair_idx: usize,
    keypair: Option<Keypair>,

    selected_preset_idx: usize,

    rpc_client: Option<RpcClient>,
    program_client: PongAnchorClient,

    lobby_id: u64,
    lobby_id_text: String,

    lobby_data: Option<Lobby<PongGame>>,

    server_addr: String,
    skip_cert_verify: bool,
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
    use deform_core::accounts::lobby::{LobbyStatus, PLayerStatus, PlayerInfo};

    let bot_player = Pubkey::new_from_array([255; 32]);

    let mut player_infos = HashMap::new();
    for pk in [main_player, bot_player] {
        player_infos.insert(
            pk,
            PlayerInfo {
                status: PLayerStatus::Ready,
                inputs: PongInputs::default(),
            },
        );
    }

    let lobby = Lobby::<PongGame>::new(
        0,
        0,
        main_player,
        LobbyStatus::NotStarted,
        Network::Web2,
        None,
        player_infos,
        0,
    );

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
    let creator = lobby.creator;
    let right_player = lobby
        .player_infos
        .keys()
        .find(|pk| **pk != creator)
        .copied();

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
    let state = {
        let shared = client.0.read_state()?;
        shared.tick_info.game_state.clone()
    };

    for (player, new_player_state) in state.players.iter() {
        let entity = player_entities
            .0
            .get(player)
            .ok_or(anyhow!("Player not found!"))?;
        let mut player_transform = players.get_mut(*entity)?;
        player_transform.translation.y = new_player_state.paddle_y;
    }

    ball.into_inner().translation = state.ball_pos.extend(0.0);
    Ok(())
}

pub fn egui_in_menu(
    mut contexts: EguiContexts,
    mut next_state: ResMut<NextState<AppState>>,
    mut menu: ResMut<MenuState>,
    mut toasts: ResMut<EguiToasts>,
    mut commands: Commands,
    mut player_entities: ResMut<PlayerEntities>,
    paddle_slots: Res<PaddleSlots>,
    mut players_q: Query<(&mut Player, &mut Visibility)>,
    monitor_q: Query<&Monitor>,
) -> Result {
    let ctx = contexts.ctx_mut()?.clone();
    egui::Window::new("Pong").show(&ctx, |ui| {
        egui::ScrollArea::vertical().show(ui, |ui| {
            ui.heading("Pong");
            ui.add_space(10.0);

            // --- Play Offline ---
            if ui.button("Play Offline").clicked() {
                let main_player = menu
                    .keypair
                    .as_ref()
                    .map(|kp| Pubkey::from(kp.pubkey().to_bytes()));
                match main_player {
                    Some(main_player) => {
                        let visual_tick_micros = monitor_q
                            .iter()
                            .filter_map(|m| m.refresh_rate_millihertz)
                            .max()
                            .map(|mhz| 1_000_000_000 / mhz as u64)
                            .unwrap_or(PongGame::TICK_RATE_MICROS);
                        match start_offline(
                            &mut commands,
                            main_player,
                            &mut player_entities,
                            &paddle_slots,
                            &mut players_q,
                            visual_tick_micros,
                        ) {
                            Ok(()) => next_state.set(AppState::InGame),
                            Err(e) => {
                                toasts.0.error(format!("Offline error: {e}"));
                            }
                        }
                    }
                    None => {
                        toasts
                            .0
                            .error("Load a keypair first — it identifies your player.");
                    }
                }
            }

            ui.separator();
            ui.heading("Solana");

            // --- Keypair selection ---
            ui.horizontal(|ui| {
                ui.label("Keypair:");
                let selected_name = menu
                    .keypair_files
                    .get(menu.selected_keypair_idx)
                    .cloned()
                    .unwrap_or_else(|| "(none)".into());
                let files_snapshot: Vec<String> = menu.keypair_files.clone();
                egui::ComboBox::from_id_salt("keypair_select")
                    .selected_text(&selected_name)
                    .show_ui(ui, |cb| {
                        for (i, name) in files_snapshot.iter().enumerate() {
                            cb.selectable_value(&mut menu.selected_keypair_idx, i, name);
                        }
                    });
                if ui.button("Refresh").clicked() {
                    menu.keypair_files = scan_json_files();
                }
                if ui.button("Load").clicked() {
                    if let Some(path) = menu.keypair_files.get(menu.selected_keypair_idx) {
                        match read_keypair_file(path) {
                            Ok(kp) => {
                                toasts.0.info(format!("Loaded: {}", kp.pubkey()));
                                menu.keypair = Some(kp);
                            }
                            Err(e) => {
                                toasts.0.error(format!("Failed to load keypair: {e}"));
                            }
                        }
                    }
                }
            });

            if let Some(kp) = &menu.keypair {
                ui.label(format!("Pubkey: {}", kp.pubkey()));
                if let Some(rpc) = &menu.rpc_client {
                    if ui.button("Airdrop 10 SOL").clicked() {
                        let lamports = 10 * solana_sdk::native_token::LAMPORTS_PER_SOL;
                        match rpc.request_airdrop(&kp.pubkey(), lamports) {
                            Ok(sig) => {
                                toasts.0.info(format!("Airdrop requested: {sig}"));
                            }
                            Err(e) => {
                                toasts.0.error(format!("Airdrop failed: {e}"));
                            }
                        }
                    }
                }
            }

            // --- Network preset ---
            ui.horizontal(|ui| {
                ui.label("Network:");
                let preset_name = NETWORK_PRESETS[menu.selected_preset_idx].name;
                egui::ComboBox::from_id_salt("network_select")
                    .selected_text(preset_name)
                    .show_ui(ui, |cb| {
                        for (i, preset) in NETWORK_PRESETS.iter().enumerate() {
                            cb.selectable_value(&mut menu.selected_preset_idx, i, preset.name);
                        }
                    });
                if ui.button("Connect").clicked() {
                    let preset = &NETWORK_PRESETS[menu.selected_preset_idx];
                    let rpc = RpcClient::new(preset.rpc_url.to_string());
                    menu.rpc_client = Some(rpc);
                    menu.program_client = PongAnchorClient;
                    toasts.0.info(format!("Connected to {}", preset.name));
                }
            });

            if menu.rpc_client.is_some() {
                ui.label(format!(
                    "RPC: {}",
                    NETWORK_PRESETS[menu.selected_preset_idx].rpc_url
                ));
            }

            ui.separator();

            // --- Lobby ---
            ui.heading("Lobby");
            ui.horizontal(|ui| {
                ui.label("Lobby ID:");
                let response = ui
                    .add(egui::TextEdit::singleline(&mut menu.lobby_id_text).desired_width(120.0));
                if response.changed() {
                    if let Ok(parsed) = menu.lobby_id_text.trim().parse::<u64>() {
                        menu.lobby_id = parsed;
                    }
                }
            });

            let has_all = menu.rpc_client.is_some() && menu.keypair.is_some();

            if has_all {
                let rpc = menu.rpc_client.as_ref().unwrap();
                let program_client = &menu.program_client;
                let keypair = menu.keypair.as_ref().unwrap();
                let lobby_id = menu.lobby_id;
                let (lobby_pda, _) =
                    Lobby::<PongGame>::find_program_address(lobby_id, &GAME_PROGRAM);
                let user = Pubkey::from(keypair.pubkey().to_bytes());

                ui.label(format!("Lobby PDA: {lobby_pda}"));

                let mut new_lobby_data = None;

                ui.horizontal(|ui| {
                    if ui.button("Create Lobby").clicked() {
                        let ix = program_client.create_lobby_ix(user, lobby_pda, lobby_id);
                        match send_and_confirm_tx(rpc, ix, keypair, menu.selected_preset_idx == 0) {
                            Ok(()) => {
                                toasts.0.info("Lobby created!");
                            }
                            Err(e) => {
                                toasts.0.error(format!("Create failed: {e}"));
                            }
                        }
                    }

                    if ui.button("Join Lobby").clicked() {
                        let ix = program_client.join_lobby_ix(user, lobby_pda, lobby_id);
                        match send_and_confirm_tx(rpc, ix, keypair, menu.selected_preset_idx == 0) {
                            Ok(()) => {
                                toasts.0.info("Joined lobby!");
                            }
                            Err(e) => {
                                toasts.0.error(format!("Join failed: {e}"));
                            }
                        }
                    }

                    if ui.button("Ready").clicked() {
                        let ix = program_client.ready_ix(ReadyArgs::Web2 {
                            user,
                            lobby: lobby_pda,
                            id: lobby_id,
                        });
                        match send_and_confirm_tx(rpc, ix, keypair, menu.selected_preset_idx == 0) {
                            Ok(()) => {
                                toasts.0.info("Ready!");
                            }
                            Err(e) => {
                                toasts.0.error(format!("Ready failed: {e}"));
                            }
                        }
                    }
                    if ui.button("Ready (FOC)").clicked() {
                        let (inputs_pda, _) = InputsAccount::<PongGame>::find_program_address(
                            lobby_id,
                            &user,
                            &GAME_PROGRAM,
                        );
                        let ix = program_client.ready_ix(ReadyArgs::FullyOnchain {
                            user,
                            lobby: lobby_pda,
                            id: lobby_id,
                            inputs: inputs_pda,
                        });
                        match send_and_confirm_tx(rpc, ix, keypair, menu.selected_preset_idx == 0) {
                            Ok(()) => {
                                toasts.0.info("Ready!");
                            }
                            Err(e) => {
                                toasts.0.error(format!("Ready failed: {e}"));
                            }
                        }
                    }
                });

                if ui.button("Read Lobby").clicked() {
                    match rpc.get_account_data(&Address::new_from_array(lobby_pda.to_bytes())) {
                        // FIX: how tf is the server deserializing the lobby???
                        Ok(data) => match Lobby::<PongGame>::from_bytes(&data) {
                            Ok(lobby) => {
                                toasts.0.info(format!(
                                    "Lobby loaded, players: {}",
                                    lobby.player_infos.len()
                                ));
                                new_lobby_data = Some(lobby);
                            }
                            Err(e) => {
                                toasts.0.error(format!("Deserialize failed: {e}"));
                            }
                        },
                        Err(e) => {
                            toasts.0.error(format!("RPC error: {e}"));
                        }
                    }
                }

                if let Some(lobby) = new_lobby_data {
                    menu.lobby_data = Some(lobby);
                }

                if let Some(lobby) = &menu.lobby_data {
                    ui.label(format!("Creator: {}", &lobby.creator.to_string()[..8]));
                    ui.label(format!("Players: {}", lobby.player_infos.len()));
                    for pk in lobby.player_infos.keys() {
                        let role = if *pk == lobby.creator { "L" } else { "R" };
                        ui.label(format!("  [{}] {}", role, &pk.to_string()[..8]));
                    }
                }

                ui.separator();

                // --- Server / Play Online ---
                ui.heading("Server");
                ui.horizontal(|ui| {
                    ui.label("Address:");
                    ui.add(egui::TextEdit::singleline(&mut menu.server_addr).desired_width(200.0));
                });
                ui.checkbox(&mut menu.skip_cert_verify, "Skip TLS verification (dev)");

                if menu.lobby_data.is_some() {
                    if ui.button("Play Online").clicked() {
                        let lobby = menu.lobby_data.clone().unwrap();
                        let visual_tick_micros = monitor_q
                            .iter()
                            .filter_map(|m| m.refresh_rate_millihertz)
                            .max()
                            .map(|mhz| 1_000_000_000 / mhz as u64)
                            .unwrap_or(PongGame::TICK_RATE_MICROS);
                        match start_online(
                            &mut commands,
                            lobby,
                            user,
                            &menu.server_addr,
                            menu.skip_cert_verify,
                            &mut player_entities,
                            &paddle_slots,
                            &mut players_q,
                            visual_tick_micros,
                        ) {
                            Ok(()) => next_state.set(AppState::InGame),
                            Err(e) => {
                                toasts.0.error(format!("Online error: {e}"));
                            }
                        }
                    }
                } else {
                    ui.colored_label(egui::Color32::YELLOW, "Read a lobby first to play online.");
                }
            } else {
                ui.colored_label(
                    egui::Color32::YELLOW,
                    "Load a keypair and connect to a network first.",
                );
            }
        });
    });
    Ok(())
}

pub fn egui_in_game(mut contexts: EguiContexts, client: Res<MultiplayerClient>) -> Result {
    let state = {
        let shared = client.0.read_state()?;
        shared.tick_info.game_state.clone()
    };

    let creator = state.creator;
    let right_pk = state.players.keys().find(|pk| **pk != creator).copied();

    egui::Window::new("Scoreboard").show(contexts.ctx_mut()?, |ui| {
        if let Some(ps) = state.players.get(&creator) {
            ui.label(format!("Player 1 (Left): {}", ps.score));
        }
        if let Some(right) = right_pk {
            if let Some(ps) = state.players.get(&right) {
                ui.label(format!("Player 2 (Right): {}", ps.score));
            }
        }
    });
    Ok(())
}
