use std::sync::Arc;

use anyhow::anyhow;
use bevy::{
    diagnostic::{DiagnosticsStore, FrameTimeDiagnosticsPlugin},
    prelude::*,
    window::Monitor,
};
use bevy_egui::{EguiContexts, egui};
use bevy_egui_notify::EguiToasts;
use deform_core::{
    DeformGameState, DeformUserLogic, Pubkey,
    accounts::{
        DeformAccount,
        inputs::InputsAccount,
        lobby::{Lobby, LobbyFinished, LobbyState, Network},
    },
    game_program_client::{GameProgramClient, ReadyArgs},
};
use deform_quic::netem::FakeNetwork;
use egui_probe::Probe;
use shooter_airsoft::{
    shooter_logic::*,
    solana::anchor_client::{GAME_PROGRAM, ShooterAnchorClient},
};
use solana_address::Address;
use solana_client::rpc_client::RpcClient;
use solana_sdk::{
    signature::{Keypair, read_keypair_file},
    signer::Signer,
};

use crate::{
    client::{
        AppState, BotEnabled, LocalPlayer, MultiplayerClient, NETWORK_PRESETS, NetStats,
        scan_json_files, start_offline, start_online, web2_server_addr,
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
    pub program_client: ShooterAnchorClient,

    // is either set manually or when reading the lobby
    pub network: Network,

    pub lobby_id: u64,
    pub lobby_id_text: String,

    pub lobby_data: Option<Lobby<ShooterGame>>,

    pub skip_cert_verify: bool,

    /// Emulated link conditions for the web2 backend. `None` is the real network.
    pub fake_network: Option<FakeNetwork>,
}

#[allow(clippy::too_many_arguments)]
pub fn egui_in_menu(
    mut contexts: EguiContexts,
    mut bot: ResMut<BotEnabled>,
    mut next_state: ResMut<NextState<AppState>>,
    mut menu: ResMut<MenuState>,
    mut toasts: ResMut<EguiToasts>,
    mut commands: Commands,
    monitor_q: Query<&Monitor>,
    diagnostics: Res<DiagnosticsStore>,
) -> Result {
    let ctx = contexts.ctx_mut()?.clone();
    egui::Window::new("Bomb House / Airsoft").show(&ctx, |ui| {
        egui::ScrollArea::vertical().show(ui, |ui| {
            show_fps(ui, &diagnostics);
            if cfg!(feature = "metrics") {
                ui.colored_label(egui::Color32::LIGHT_GREEN, "Metrics enabled");
            } else {
                ui.colored_label(egui::Color32::GRAY, "Metrics disabled");
            }
            ui.checkbox(&mut bot.0, "bot");
            ui.add_space(6.0);
            ui.heading("BOMB HOUSE");
            ui.label(
                "WASD to move, Space to jump, mouse to look, M1 to fire. Esc frees the cursor.",
            );
            ui.add_space(10.0);

            // --- Play Offline ---
            if ui.button("Play Offline (vs bot)").clicked() {
                let main_player = menu
                    .keypair
                    .as_ref()
                    .map(|kp| Pubkey::from(kp.pubkey().to_bytes()))
                    .unwrap_or_else(|| Pubkey::new_from_array([1; 32]));
                match start_offline(
                    &mut commands,
                    main_player,
                    visual_tick_micros(&monitor_q),
                    true,
                ) {
                    Ok(()) => next_state.set(AppState::InGame),
                    Err(e) => {
                        toasts.0.error(format!("Offline error: {e}"));
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
                if ui.button("Load").clicked()
                    && let Some(path) = menu.keypair_files.get(menu.selected_keypair_idx)
                {
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
            });

            if let Some(kp) = &menu.keypair {
                ui.label(format!("Pubkey: {}", kp.pubkey()));
                if let Some(rpc) = &menu.rpc_client
                    && ui.button("Airdrop 10 SOL").clicked()
                {
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
                    menu.program_client = ShooterAnchorClient;
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
            // Unlike pong, this game cannot run fully on-chain: `advance_frame`
            // drives a bevy/avian physics world, which cannot compile for SBF. The
            // probe still lets you create any lobby, but only Web2 ones are playable.
            Probe::new(&mut menu.network)
                .with_header("Network")
                .show(ui);
            if matches!(menu.network, Network::Web2(_)) {
                Probe::new(&mut menu.fake_network)
                    .with_header("Fake network")
                    .show(ui);
            }
            if !matches!(menu.network, Network::Web2(_)) {
                ui.colored_label(
                    egui::Color32::YELLOW,
                    "FullyOnChain lobbies can't be played by this example — \
                     the avian/tnua simulation cannot run inside the program.",
                );
            }

            ui.separator();

            // --- Lobby ---
            ui.heading("Lobby");
            ui.horizontal(|ui| {
                ui.label("Lobby ID:");
                let response = ui
                    .add(egui::TextEdit::singleline(&mut menu.lobby_id_text).desired_width(120.0));
                if response.changed()
                    && let Ok(parsed) = menu.lobby_id_text.trim().parse::<u64>()
                {
                    menu.lobby_id = parsed;
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
                    Lobby::<ShooterGame>::find_program_address(lobby_id, &GAME_PROGRAM);
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

                    // The network (Web2 vs FullyOnChain) picks the ready variant.
                    if ui.button("Ready").clicked() {
                        // also read the lobby
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
                                    InputsAccount::<ShooterGame>::find_program_address(
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
                });

                if ui.button("Read Lobby").clicked()
                    && let Some(lobby) = read_lobby(&lobby_pda, rpc, ui, &mut toasts)
                {
                    menu.network = lobby.metadata.network.clone();
                    menu.lobby_data = Some(lobby);
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
                    if ui.button("Play Online (web2)").clicked() {
                        let lobby = menu.lobby_data.clone().unwrap();
                        let visual_tick_micros = visual_tick_micros(&monitor_q);
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
                                menu.fake_network,
                                visual_tick_micros,
                            ),
                            Network::FullyOnChain(_) => Err(anyhow!(
                                "this example has no FoC backend: avian/tnua cannot run on-chain"
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

fn visual_tick_micros(monitor_q: &Query<&Monitor>) -> u64 {
    monitor_q
        .iter()
        .filter_map(|m| m.refresh_rate_millihertz)
        .max()
        .map(|mhz| 1_000_000_000 / mhz as u64)
        .unwrap_or(ShooterGame::TICK_RATE_MICROS)
}

pub fn egui_in_game(
    mut contexts: EguiContexts,
    mut bot: ResMut<BotEnabled>,
    client: Res<MultiplayerClient>,
    local: Res<LocalPlayer>,
    net_stats: Res<NetStats>,
    diagnostics: Res<DiagnosticsStore>,
) -> Result {
    let lobby = client.0.read_state()?.lobby.clone();

    let (players, finished) = match &lobby.state {
        LobbyState::NotStarted(_) => (Vec::new(), false),
        LobbyState::Ongoing(ongoing) => (
            collect_scores(&ongoing.tick_info.game_state),
            ongoing.tick_info.game_state.has_ended(),
        ),
        LobbyState::Finished(LobbyFinished(finished)) => {
            (collect_scores(&finished.tick_info.game_state), true)
        }
    };

    let round_state = match &lobby.state {
        LobbyState::Ongoing(ongoing) => Some(&ongoing.tick_info.game_state),
        _ => None,
    };
    let ctx = contexts.ctx_mut()?;

    egui::Window::new("Scoreboard").show(ctx, |ui| {
        ui.checkbox(&mut bot.0, "bot");
        ui.separator();
        for (pk, score) in &players {
            let you = if *pk == local.0 { " (you)" } else { "" };
            ui.label(format!("{}…{you}: {score}", &pk.to_string()[..8]));
        }
        ui.separator();
        show_fps(ui, &diagnostics);
        ui.label(format!("Ping: {:.0} ms", net_stats.ping_ms));
        if let Some(state) = round_state {
            use shooter_airsoft::shooter_logic::{
                ROUND_OVER_TICKS, RoundPhase, SPAWN_FREEZE_TICKS,
            };
            match state.phase {
                RoundPhase::SpawnFreeze => {
                    let seconds = SPAWN_FREEZE_TICKS
                        .saturating_sub(state.phase_ticks)
                        .div_ceil(60);
                    ui.heading(format!("Ready in {seconds}s"));
                    ui.label("Look around — movement and shooting unlock at GO.");
                }
                RoundPhase::RoundOver => {
                    let seconds = ROUND_OVER_TICKS
                        .saturating_sub(state.phase_ticks)
                        .div_ceil(60);
                    ui.heading(format!("Respawn in {seconds}s"));
                    if state
                        .players
                        .get(&local.0)
                        .is_some_and(|p| p.death.is_some())
                    {
                        ui.label("You were hit — watching the shooter.");
                    } else {
                        ui.label("Round over — scoring is paused.");
                    }
                }
                RoundPhase::Playing => {}
            }
        }
        if finished {
            let winner = players.first().map(|(pk, _)| pk.to_string());
            ui.separator();
            ui.heading("Match over!");
            if let Some(winner) = winner {
                ui.label(format!("Winner: {}…", &winner[..8]));
            }
        }
    });

    Ok(())
}

fn show_fps(ui: &mut egui::Ui, diagnostics: &DiagnosticsStore) {
    if let Some(fps) = diagnostics
        .get(&FrameTimeDiagnosticsPlugin::FPS)
        .and_then(|diagnostic| diagnostic.smoothed())
    {
        ui.label(format!("FPS: {fps:.0}"));
    } else {
        ui.label("FPS: —");
    }
}

fn collect_scores(state: &ShooterGameState) -> Vec<(Pubkey, u32)> {
    let mut players: Vec<(Pubkey, u32)> = state
        .players
        .iter()
        .map(|(pk, ps)| (*pk, ps.score))
        .collect();
    // highest score first, pubkey as deterministic tie-break
    players.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    players
}

pub fn read_lobby(
    lobby_pda: &Pubkey,
    rpc: &RpcClient,
    ui: &mut egui::Ui,
    toasts: &mut EguiToasts,
) -> Option<Lobby<ShooterGame>> {
    match rpc.get_account_data(&Address::new_from_array(lobby_pda.to_bytes())) {
        Ok(data) => match DeformAccount::<ShooterGame>::from_bytes(&data) {
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
                    ui.label(format!("  {}", &pk.to_string()[..8]));
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
