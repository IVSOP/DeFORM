use anyhow::anyhow;
use bevy::{prelude::*, window::Monitor};
use bevy_egui::{EguiContexts, egui};
use bevy_egui_notify::EguiToasts;
use deform_core::{
    DeformUserLogic, Pubkey,
    accounts::{
        DeformAccount,
        inputs::InputsAccount,
        lobby::{Lobby, LobbyFinished, LobbyState, Network},
    },
    game_program_client::{GameProgramClient, ReadyArgs},
};
use egui_probe::Probe;
use pong::{
    pong_logic::*,
    solana::anchor_client::{GAME_PROGRAM, PongAnchorClient},
};
use solana_address::Address;
use solana_client::rpc_client::RpcClient;
use solana_sdk::{
    signature::{Keypair, read_keypair_file},
    signer::Signer,
};

use crate::{
    client::{
        AppState, MultiplayerClient, NETWORK_PRESETS, PaddleSlots, Player, PlayerEntities,
        scan_json_files, start_offline, start_online,
    },
    send_and_confirm_tx,
};

#[derive(Resource)]
pub struct MenuState {
    pub keypair_files: Vec<String>,
    pub selected_keypair_idx: usize,
    pub keypair: Option<Keypair>,

    pub selected_preset_idx: usize,

    pub rpc_client: Option<RpcClient>,
    pub program_client: PongAnchorClient,

    // is either set manually or when reading the lobby
    pub network: Network,

    pub lobby_id: u64,
    pub lobby_id_text: String,

    pub lobby_data: Option<Lobby<PongGame>>,

    pub server_addr: String,
    pub skip_cert_verify: bool,
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

            // --- RPC cluster preset ---
            ui.horizontal(|ui| {
                ui.label("Cluster:");
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

            // --- Network (composed for the next Create Lobby) ---
            // egui-probe renders `deform_core::Network` recursively: each variant's
            // payload unfolds into further combo boxes. The header row carries the
            // variant selector, so `.with_header` is required (without it, only the
            // inner fields show — nothing for a payload-less variant like Web2).
            // Distinct from the RPC cluster above.
            Probe::new(&mut menu.network)
                .with_header("Network")
                .show(ui);

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

                ui.horizontal(|ui| {
                    if ui.button("Create Lobby").clicked() {
                        let ix = program_client.create_lobby_ix(
                            user,
                            lobby_pda,
                            lobby_id,
                            menu.network.clone(),
                        );
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

                    // The network (Web2 vs FullyOnChain) picks the ready variant.
                    if ui.button("Ready").clicked() {
                        let args = match menu.network.clone() {
                            Network::Web2 => ReadyArgs::Web2 {
                                user,
                                lobby: lobby_pda,
                                id: lobby_id,
                            },
                            Network::FullyOnChain(_) => {
                                let (inputs_pda, _) =
                                    InputsAccount::<PongGame>::find_program_address(
                                        lobby_id,
                                        &user,
                                        &GAME_PROGRAM,
                                    );
                                ReadyArgs::FullyOnchain {
                                    user,
                                    lobby: lobby_pda,
                                    id: lobby_id,
                                    inputs: inputs_pda,
                                }
                            }
                        };
                        let ix = program_client.ready_ix(args);
                        match send_and_confirm_tx(rpc, ix, keypair, menu.selected_preset_idx == 0) {
                            Ok(()) => {
                                toasts.0.info("Ready!");
                            }
                            Err(e) => {
                                toasts.0.error(format!("Ready failed: {e}"));
                            }
                        }
                    }

                    // Start delegates the lobby + inputs to the ephemeral rollup, so it
                    // only exists on FullyOnChain. `matches!` sidesteps naming the payload.
                    if matches!(menu.network, Network::FullyOnChain(_))
                        && ui.button("Start").clicked()
                    {
                        match menu.lobby_data.as_ref() {
                            Some(lobby) => match &lobby.state {
                                LobbyState::NotStarted(not_started) => {
                                    match program_client.start_ix(
                                        user,
                                        lobby_pda,
                                        &lobby.metadata,
                                        not_started,
                                        GAME_PROGRAM,
                                    ) {
                                        Ok(ix) => match send_and_confirm_tx(
                                            rpc,
                                            ix,
                                            keypair,
                                            menu.selected_preset_idx == 0,
                                        ) {
                                            Ok(()) => {
                                                toasts.0.info("Game started!");
                                            }
                                            Err(e) => {
                                                toasts.0.error(format!("Start failed: {e}"));
                                            }
                                        },
                                        Err(e) => {
                                            toasts.0.error(format!("Start failed: {e}"));
                                        }
                                    }
                                }
                                _ => {
                                    toasts.0.error("Lobby already started.");
                                }
                            },
                            None => {
                                toasts.0.error("Read a lobby first to start.");
                            }
                        }
                    }
                });

                if ui.button("Read Lobby").clicked() {
                    match rpc.get_account_data(&Address::new_from_array(lobby_pda.to_bytes())) {
                        // FIX: how tf is the server deserializing the lobby???
                        Ok(data) => match DeformAccount::<PongGame>::from_bytes(&data) {
                            Ok(DeformAccount::Lobby(lobby)) => {
                                let player_keys: Vec<Pubkey> = match &lobby.state {
                                    LobbyState::NotStarted(not_started) => {
                                        not_started.player_status.keys().copied().collect()
                                    }
                                    LobbyState::Ongoing(ongoing) => {
                                        ongoing.tick_info.inputs.keys().copied().collect()
                                    }
                                    LobbyState::Finished(LobbyFinished(finished)) => {
                                        finished.tick_info.inputs.keys().copied().collect()
                                    }
                                };

                                toasts
                                    .0
                                    .info(format!("Lobby loaded, players: {}", player_keys.len()));

                                ui.label(format!(
                                    "Creator: {}",
                                    &lobby.metadata.creator.to_string()[..8]
                                ));
                                ui.label(format!("Players: {}", player_keys.len()));
                                for pk in player_keys.iter() {
                                    let role = if *pk == lobby.metadata.creator {
                                        "L"
                                    } else {
                                        "R"
                                    };
                                    ui.label(format!("  [{}] {}", role, &pk.to_string()[..8]));
                                }

                                menu.network = lobby.metadata.network.clone();
                                menu.lobby_data = Some(lobby);
                            }
                            Ok(_) => {
                                toasts
                                    .0
                                    .error(format!("Deserialize failed: wrong account type"));
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
                        // The lobby's network decides which backend serves the game.
                        match menu.network.clone() {
                            Network::Web2 => {
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
                            // TODO: web3 backend — route the game through the selected
                            // validator network instead of the QUIC server.
                            Network::FullyOnChain(_) => {
                                toasts.0.error("Fully on-chain backend not implemented yet");
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
    let lobby = client.0.read_state()?.lobby.clone();

    let creator = lobby.metadata.creator;

    let (creator_score, right_player_score) = match &lobby.state {
        LobbyState::Finished(_) => Err(anyhow!("Lobby has already finished!"))?,
        LobbyState::NotStarted(_) => (0, 0),
        LobbyState::Ongoing(ongoing) => {
            let creator_score = ongoing
                .tick_info
                .game_state
                .players
                .get(&creator)
                .map_or(0, |info| info.score);

            let right_player = ongoing
                .tick_info
                .inputs
                .keys()
                .find(|pk| **pk != creator)
                .copied();

            let right_player_score = if let Some(right_player) = &right_player {
                ongoing
                    .tick_info
                    .game_state
                    .players
                    .get(right_player)
                    .map_or(0, |info| info.score)
            } else {
                0
            };

            (creator_score, right_player_score)
        }
    };

    egui::Window::new("Scoreboard").show(contexts.ctx_mut()?, |ui| {
        ui.label(format!("Player 1 (Left): {}", creator_score));
        ui.label(format!("Player 2 (Right): {}", right_player_score));
    });
    Ok(())
}
