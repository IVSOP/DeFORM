use std::sync::Arc;

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
use soccer::{
    soccer_logic::*,
    solana::anchor_client::{GAME_PROGRAM, SoccerAnchorClient},
};
use solana_address::Address;
use solana_client::rpc_client::RpcClient;
use solana_sdk::{
    signature::{Keypair, read_keypair_file},
    signer::Signer,
};

#[cfg(feature = "foc")]
use crate::client::start_online_foc;
use crate::{
    client::{
        AppState, BotEnabled, MultiplayerClient, NETWORK_PRESETS, NetStats, Player, PlayerEntities,
        PlayerSlots, scan_json_files, start_offline, start_online, web2_server_addr,
    },
    send_and_confirm_tx,
};

#[derive(Resource)]
pub struct MenuState {
    pub keypair_files: Vec<String>,
    pub selected_keypair_idx: usize,
    pub keypair: Option<Arc<Keypair>>,

    pub selected_preset_idx: usize,

    pub rpc_client: Option<Arc<RpcClient>>,
    pub program_client: SoccerAnchorClient,

    pub network: Network,

    pub lobby_id: u64,
    pub lobby_id_text: String,

    pub lobby_data: Option<Lobby<SoccerGame>>,

    pub skip_cert_verify: bool,
}

pub fn egui_in_menu(
    mut contexts: EguiContexts,
    mut bot: ResMut<BotEnabled>,
    mut next_state: ResMut<NextState<AppState>>,
    mut menu: ResMut<MenuState>,
    mut toasts: ResMut<EguiToasts>,
    mut commands: Commands,
    mut player_entities: ResMut<PlayerEntities>,
    player_slots: Res<PlayerSlots>,
    mut players_q: Query<(&mut Player, &mut Visibility)>,
    monitor_q: Query<&Monitor>,
) -> Result {
    let ctx = contexts.ctx_mut()?.clone();
    egui::Window::new("Soccer").show(&ctx, |ui| {
        egui::ScrollArea::vertical().show(ui, |ui| {
            if cfg!(feature = "metrics") {
                ui.colored_label(egui::Color32::LIGHT_GREEN, "Metrics enabled");
            } else {
                ui.colored_label(egui::Color32::GRAY, "Metrics disabled");
            }
            ui.checkbox(&mut bot.0, "bot");
            ui.add_space(6.0);
            ui.heading("Eggy League");
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
                            .unwrap_or(SoccerGame::TICK_RATE_MICROS);
                        match start_offline(
                            &mut commands,
                            main_player,
                            &mut player_entities,
                            &player_slots,
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
                                menu.keypair = Some(Arc::new(kp));
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
                    menu.rpc_client = Some(Arc::new(rpc));
                    menu.program_client = SoccerAnchorClient;
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
                let rpc_handle = menu.rpc_client.clone().unwrap();
                let rpc = &*rpc_handle;
                let program_client = menu.program_client.clone();
                let keypair_handle = menu.keypair.clone().unwrap();
                let keypair = &*keypair_handle;
                let lobby_id = menu.lobby_id;
                let (lobby_pda, _) =
                    Lobby::<SoccerGame>::find_program_address(lobby_id, &GAME_PROGRAM);
                let user = Pubkey::from(keypair.pubkey().to_bytes());

                ui.label(format!("Lobby PDA: {lobby_pda}"));

                ui.horizontal(|ui| {
                    if ui.button("Create Lobby").clicked() {
                        match program_client.create_lobby_ix(
                            user,
                            lobby_pda,
                            lobby_id,
                            menu.network.clone(),
                        ) {
                            Ok(ix) => match send_and_confirm_tx(
                                rpc,
                                ix,
                                keypair,
                                menu.selected_preset_idx == 0,
                            ) {
                                Ok(_) => {
                                    toasts.0.info("Lobby created!");
                                }
                                Err(e) => {
                                    toasts.0.error(format!("Create failed: {e}"));
                                }
                            },
                            Err(e) => {
                                toasts.0.error(format!("Create failed: {e}"));
                            }
                        }
                    }

                    if ui.button("Join Lobby").clicked() {
                        match program_client.join_lobby_ix(user, lobby_pda, lobby_id) {
                            Ok(ix) => match send_and_confirm_tx(
                                rpc,
                                ix,
                                keypair,
                                menu.selected_preset_idx == 0,
                            ) {
                                Ok(_) => {
                                    toasts.0.info("Joined lobby!");
                                }
                                Err(e) => {
                                    toasts.0.error(format!("Join failed: {e}"));
                                }
                            },
                            Err(e) => {
                                toasts.0.error(format!("Join failed: {e}"));
                            }
                        }
                    }

                    if ui.button("Ready").clicked() {
                        if let Some(lobby) = read_lobby(&lobby_pda, rpc, ui, &mut toasts) {
                            menu.network = lobby.metadata.network.clone();
                            menu.lobby_data = Some(lobby);
                        }

                        let args = match menu.network.clone() {
                            Network::Web2(_) => ReadyArgs::Web2 {
                                user,
                                lobby: lobby_pda,
                                id: lobby_id,
                            },
                            Network::FullyOnChain(_) => {
                                let (inputs_pda, _) =
                                    InputsAccount::<SoccerGame>::find_program_address(
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
                        match program_client.ready_ix(args) {
                            Ok(ix) => match send_and_confirm_tx(
                                rpc,
                                ix,
                                keypair,
                                menu.selected_preset_idx == 0,
                            ) {
                                Ok(_) => {
                                    toasts.0.info("Ready!");
                                }
                                Err(e) => {
                                    toasts.0.error(format!("Ready failed: {e}"));
                                }
                            },
                            Err(e) => {
                                toasts.0.error(format!("Ready failed: {e}"));
                            }
                        }
                    }

                    if matches!(menu.network, Network::FullyOnChain(_))
                        && ui.button("Start").clicked()
                    {
                        if let Some(lobby) = read_lobby(&lobby_pda, rpc, ui, &mut toasts) {
                            menu.network = lobby.metadata.network.clone();
                            menu.lobby_data = Some(lobby);
                        }

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
                                            Ok(_) => {
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

                    if matches!(menu.network, Network::FullyOnChain(_))
                        && ui.button("Init Crank").clicked()
                    {
                        match menu.lobby_data.as_ref() {
                            Some(lobby) => {
                                let players: Vec<Pubkey> = match &lobby.state {
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
                                let inputs_accounts: Vec<Pubkey> = players
                                    .iter()
                                    .map(|player| {
                                        InputsAccount::<SoccerGame>::find_program_address(
                                            lobby_id,
                                            player,
                                            &GAME_PROGRAM,
                                        )
                                        .0
                                    })
                                    .collect();

                                let execution_interval_millis =
                                    (SoccerGame::TICK_RATE_MICROS / 1_000).max(1) as i64;

                                match program_client.init_crank_ix(
                                    user,
                                    lobby_pda,
                                    lobby_id,
                                    &inputs_accounts,
                                    execution_interval_millis,
                                    i64::MAX,
                                ) {
                                    Ok(ix) => {
                                        let er_rpc = RpcClient::new(
                                            NETWORK_PRESETS[menu.selected_preset_idx]
                                                .er_rpc_url
                                                .to_string(),
                                        );
                                        match send_and_confirm_tx(
                                            &er_rpc,
                                            ix,
                                            keypair,
                                            menu.selected_preset_idx == 0,
                                        ) {
                                            Ok(_) => {
                                                toasts.0.info("Crank scheduled!");
                                            }
                                            Err(e) => {
                                                toasts.0.error(format!("Init crank failed: {e}"));
                                            }
                                        }
                                    }
                                    Err(e) => {
                                        toasts.0.error(format!("Init crank failed: {e}"));
                                    }
                                }
                            }
                            None => {
                                toasts.0.error("Read a lobby first to init the crank.");
                            }
                        }
                    }
                });

                if ui.button("Read Lobby").clicked() {
                    if let Some(lobby) = read_lobby(&lobby_pda, rpc, ui, &mut toasts) {
                        menu.network = lobby.metadata.network.clone();
                        menu.lobby_data = Some(lobby);
                    }
                }

                ui.separator();

                // --- Server / Play Online ---
                ui.heading("Server");
                // *Which* server is part of the lobby's `Network` (the picker above);
                // this only shows where that resolves to on this machine.
                if let Network::Web2(server) = &menu.network {
                    let addr = web2_server_addr(server);
                    if addr.is_empty() {
                        ui.label("Address: unknown; run deploy.sh");
                    } else {
                        ui.label(format!("Address: {addr}"));
                    }
                }
                ui.checkbox(&mut menu.skip_cert_verify, "Skip TLS verification (dev)");

                if menu.lobby_data.is_some() {
                    let play_label = match menu.network {
                        Network::Web2(_) => "Play Online (web2)",
                        Network::FullyOnChain(_) => "Play Online (FoC)",
                    };
                    if ui.button(play_label).clicked() {
                        let lobby = menu.lobby_data.clone().unwrap();
                        let visual_tick_micros = monitor_q
                            .iter()
                            .filter_map(|m| m.refresh_rate_millihertz)
                            .max()
                            .map(|mhz| 1_000_000_000 / mhz as u64)
                            .unwrap_or(SoccerGame::TICK_RATE_MICROS);
                        // Resolved here so the arm below stays a plain call: the lobby says
                        // *which* server, this machine says where that one lives.
                        let web2_addr = match &menu.network {
                            Network::Web2(server) => web2_server_addr(server),
                            Network::FullyOnChain(_) => String::new(),
                        };
                        let result = match menu.network.clone() {
                            Network::Web2(_) if web2_addr.is_empty() => {
                                Err(anyhow!("remote server address unknown; run deploy.sh").into())
                            }
                            Network::Web2(_) => start_online(
                                &mut commands,
                                lobby,
                                user,
                                &web2_addr,
                                menu.skip_cert_verify,
                                &mut player_entities,
                                &player_slots,
                                &mut players_q,
                                visual_tick_micros,
                            ),
                            #[cfg(feature = "foc")]
                            Network::FullyOnChain(_) => start_online_foc(
                                &mut commands,
                                lobby,
                                menu.keypair.as_ref().unwrap(),
                                &mut player_entities,
                                &player_slots,
                                &mut players_q,
                                visual_tick_micros,
                            ),
                            #[cfg(not(feature = "foc"))]
                            Network::FullyOnChain(_) => Err(anyhow!(
                                "FoC backend not compiled in; rebuild with a `foc-*` feature"
                            )
                            .into()),
                        };
                        match result {
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

pub fn egui_in_game(
    mut contexts: EguiContexts,
    mut bot: ResMut<BotEnabled>,
    client: Res<MultiplayerClient>,
    net_stats: Res<NetStats>,
) -> Result {
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

    egui::Window::new("Score").show(contexts.ctx_mut()?, |ui| {
        ui.checkbox(&mut bot.0, "bot");
        ui.separator();
        ui.label(format!("{} - {}", creator_score, right_player_score));
        ui.separator();
        ui.label(format!("Ping: {:.0} ms", net_stats.ping_ms));
    });
    Ok(())
}

pub fn read_lobby(
    lobby_pda: &Pubkey,
    rpc: &RpcClient,
    ui: &mut egui::Ui,
    toasts: &mut EguiToasts,
) -> Option<Lobby<SoccerGame>> {
    match rpc.get_account_data(&Address::new_from_array(lobby_pda.to_bytes())) {
        Ok(data) => match DeformAccount::<SoccerGame>::from_bytes(&data) {
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

                return Some(lobby);
            }
            Ok(_) => {
                toasts
                    .0
                    .error("Deserialize failed: wrong account type".to_string());
            }
            Err(e) => {
                toasts.0.error(format!("Deserialize failed: {e}"));
            }
        },
        Err(e) => {
            toasts.0.error(format!("RPC error: {e}"));
        }
    }

    None
}
