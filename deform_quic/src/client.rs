use glam::FloatExt;
use pinocchio::pubkey::Pubkey;
use quinn::crypto::rustls::QuicClientConfig;
use solana_client::{rpc_client::RpcClient, rpc_config::CommitmentConfig};
use solana_sdk::signature::Signature;
use std::{
    collections::HashMap,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};
use tokio::{
    sync::{Notify, mpsc, oneshot},
    time::{Sleep, interval, sleep},
};

use deform_core::{
    DeformClient, DeformError, DeformGameState, DeformInputs, DeformReadState, DeformResult,
    DeformUserLogic, TickInfo,
    lobby::{Lobby, LobbyStatus},
};

use crate::{ALPN_PROTOCOL, ControlMessage, ServerResponse, ServerUnreliableInstruction};

pub(crate) struct QuicBackend<T: DeformUserLogic> {
    pub local_tick: u64,
    pub remote_tick: u64,
    pub info_per_tick: HashMap<u64, TickInfo<T>>,
    pub last_remote_status: LobbyStatus,
    // these are the inputs from our own player, appended only by set_inputs().
    // TODO: this might not be necessary. We may be able to just store the latest inputs, then reuse the old ones from `info_per_tick`. I only did it this way because it was easier in my head
    pub inputs: HashMap<u64, T::Inputs>,

    // pub rpc_client: Arc<RpcClient>,
    pub connection: quinn::Connection,
    // pub control_send: quinn::SendStream,
    pub control_recv: quinn::RecvStream,

    pub player: Pubkey,
    // pub lobby: Pubkey,
    // pub lobby_id: u64,
    pub terminate: Arc<Notify>,
    pub set_inputs_receiver: mpsc::UnboundedReceiver<T::Inputs>,
    pub sdk_game_state: Arc<std::sync::Mutex<DeformReadState<T>>>,
    pub user_logic: T,

    pub avg_rtt: Duration,
    /// If ticks are below this, simulation is fast forwarded. Also used to compute time dilation.
    pub min_ticks_ahead: u64,
    /// If ticks are above this, simulation is stopped; hard limit. It is always at least 5.
    pub max_ticks_ahead: u64,
    // /// Cumulative count of datagrams inferred to have been dropped (gaps in received ticks).
    // pub dropped_datagrams: u64,
    // /// Cumulative count of stale/out-of-order datagrams (tick <= remote_tick).
    // pub stale_datagrams: u64,
    // TODO:
    // pub smoother: RollbackSmoother,
}

pub const COMMIT_INPUTS_MICROS: u64 = 16667;

/// How long to wait before using the RTT value to update how far ahead the simulation is.
///
/// High value:
///     - simulation is stable, more likely to be a constant number of ticks ahead. even if it changes, it will change less frequently
///     - slow to react to changing network conditions, both positive and negative
///     - low compute overhead
///
/// Low value is the exact opposite.
///
/// From my experimentation:
/// 1s: works fine but will be slow to react to changes in the network
/// 200ms: introduces a bit of jitter
pub const RTT_SAMPLE_INTERVAL_MS: u64 = 500;

// Helpers for length-prefixed messages on reliable streams
async fn stream_write_msg(send: &mut quinn::SendStream, data: &[u8]) -> DeformResult {
    send.write_all(&(data.len() as u32).to_le_bytes())
        .await
        .map_err(|e| DeformError::Connection(e.to_string()))?;
    send.write_all(data)
        .await
        .map_err(|e| DeformError::Connection(e.to_string()))?;
    Ok(())
}

async fn stream_read_msg(recv: &mut quinn::RecvStream) -> DeformResult<Vec<u8>> {
    let mut len_buf = [0u8; 4];
    recv.read_exact(&mut len_buf)
        .await
        .map_err(|e| DeformError::Connection(e.to_string()))?;
    let len = u32::from_le_bytes(len_buf) as usize;
    if len > 1024 * 1024 {
        return Err(DeformError::Protocol(format!(
            "message too large: {} bytes",
            len
        )));
    }
    let mut buf = vec![0u8; len];
    recv.read_exact(&mut buf)
        .await
        .map_err(|e| DeformError::Connection(e.to_string()))?;
    Ok(buf)
}

impl<T: DeformUserLogic> QuicBackend<T> {
    pub fn init(
        rpc_url: String,
        server_addr: String,
        server_name: String,
        lobby_id: u64,
        player: Pubkey,
        game_program: Pubkey,
        // TODO: abstract signature and auth in general!!!!!
        sig: Signature,
        skip_cert_verify: bool,
        // these are now passed in by using default!!
        // user_logic: T,
        // initial_game_state: T::GameState,
        // initial_inputs: T::Inputs,
    ) -> DeformResult<DeformClient<T>> {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let (setup_tx, setup_rx) = oneshot::channel::<DeformResult>();

        let terminate = Arc::new(Notify::new());
        let (set_inputs_sender, set_inputs_receiver) = mpsc::unbounded_channel::<T::Inputs>();
        let sdk_game_state = Arc::new(std::sync::Mutex::new(DeformReadState::<T>::default()));
        let backend_dead = Arc::new(AtomicBool::new(false));

        // cursed
        let sdk_game_state_clone = sdk_game_state.clone();
        let sdk_game_state_clone_2 = sdk_game_state.clone();

        let terminate_clone = terminate.clone();
        let backend_dead_clone = backend_dead.clone();

        // #[cfg(feature = "log")]
        // info!("QUIC init with rpc: {}", rpc_url);
        // #[cfg(feature = "log")]
        // info!(
        //     "QUIC init with server: {} (SNI: {})",
        //     server_addr, server_name
        // );

        let _rss_thread = thread::spawn(move || {
            match tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => {
                    let rpc_client = Arc::new(RpcClient::new_with_commitment(
                        rpc_url,
                        CommitmentConfig::confirmed(),
                    ));

                    rt.block_on(async move {
                        let (lobby, _) = Lobby::<T::Inputs, T::GameState>::find_program_address(
                            lobby_id,
                            &game_program,
                        );

                        // FIX: make this exit after N retries
                        let lobby_info = loop {
                            match fetch_lobby::<T::Inputs, T::GameState>(&lobby, &rpc_client).await
                            {
                                Ok(infos) => break infos,
                                Err(_e) => {
                                    tokio::time::sleep(Duration::from_millis(200)).await;
                                }
                            }
                        };

                        // --- Build QUIC client endpoint ---
                        let tls_config = if skip_cert_verify {
                            rustls::ClientConfig::builder()
                                .dangerous()
                                .with_custom_certificate_verifier(Arc::new(SkipCertVerification))
                                .with_no_client_auth()
                        } else {
                            let mut roots = rustls::RootCertStore::empty();
                            roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
                            rustls::ClientConfig::builder()
                                .with_root_certificates(roots)
                                .with_no_client_auth()
                        };

                        let mut tls_config = tls_config;
                        tls_config.alpn_protocols = vec![ALPN_PROTOCOL.to_vec()];

                        let quic_tls = match QuicClientConfig::try_from(tls_config) {
                            Ok(c) => c,
                            Err(e) => {
                                let _ = setup_tx.send(Err(DeformError::Connection(format!(
                                    "failed to build QUIC TLS config: {e}"
                                ))));
                                return;
                            }
                        };
                        let mut client_config = quinn::ClientConfig::new(Arc::new(quic_tls));
                        {
                            let mut transport = quinn::TransportConfig::default();
                            transport.keep_alive_interval(Some(Duration::from_secs(20)));
                            client_config.transport_config(Arc::new(transport));
                        }

                        let bind_addr: std::net::SocketAddr = match "0.0.0.0:0".parse() {
                            Ok(a) => a,
                            Err(e) => {
                                let _ = setup_tx.send(Err(DeformError::Connection(format!(
                                    "failed to parse bind address: {e}"
                                ))));
                                return;
                            }
                        };
                        let mut endpoint = match quinn::Endpoint::client(bind_addr) {
                            Ok(ep) => ep,
                            Err(e) => {
                                let _ = setup_tx.send(Err(DeformError::Connection(format!(
                                    "failed to create QUIC endpoint: {e}"
                                ))));
                                return;
                            }
                        };
                        endpoint.set_default_client_config(client_config);

                        // Resolve server address
                        let addr = match tokio::net::lookup_host(&server_addr).await {
                            Ok(mut iter) => match iter.next() {
                                Some(a) => a,
                                None => {
                                    let _ = setup_tx.send(Err(DeformError::Connection(format!(
                                        "DNS resolved no addresses for '{server_addr}'"
                                    ))));
                                    return;
                                }
                            },
                            Err(e) => {
                                let _ = setup_tx.send(Err(DeformError::Connection(format!(
                                    "failed to resolve '{server_addr}': {e}"
                                ))));
                                return;
                            }
                        };

                        // Connect
                        let connection = match endpoint.connect(addr, &server_name) {
                            Ok(connecting) => match connecting.await {
                                Ok(conn) => conn,
                                Err(e) => {
                                    let _ = setup_tx.send(Err(DeformError::Connection(format!(
                                        "QUIC connection failed: {e}"
                                    ))));
                                    return;
                                }
                            },
                            Err(e) => {
                                let _ = setup_tx.send(Err(DeformError::Connection(format!(
                                    "QUIC connect error: {e}"
                                ))));
                                return;
                            }
                        };

                        // Open bi-directional stream for auth + control
                        let (mut control_send, mut control_recv) = match connection.open_bi().await
                        {
                            Ok(streams) => streams,
                            Err(e) => {
                                let _ = setup_tx.send(Err(DeformError::Connection(format!(
                                    "failed to open control stream: {e}"
                                ))));
                                return;
                            }
                        };

                        // Send handshake
                        let handshake_message = ControlMessage::Handshake {
                            lobby_id,
                            player_pubkey: player,
                            sig,
                        };

                        let bytes = match wincode::serialize(&handshake_message) {
                            Ok(bytes) => bytes,
                            Err(e) => {
                                let _ = setup_tx
                                    .send(Err(DeformError::Serialize(format!("handshake: {e:?}"))));
                                return;
                            }
                        };

                        if let Err(e) = stream_write_msg(&mut control_send, &bytes).await {
                            let _ = setup_tx.send(Err(e));
                            return;
                        }

                        // Wait for AuthOk
                        let response_bytes = match stream_read_msg(&mut control_recv).await {
                            Ok(b) => b,
                            Err(e) => {
                                let _ = setup_tx.send(Err(e));
                                return;
                            }
                        };

                        let control_msg: ControlMessage =
                            match wincode::deserialize(&response_bytes) {
                                Ok(msg) => msg,
                                Err(e) => {
                                    let _ = setup_tx.send(Err(DeformError::Deserialize(format!(
                                        "auth response: {e:?}"
                                    ))));
                                    return;
                                }
                            };

                        match control_msg {
                            ControlMessage::AuthOk => {}
                            ControlMessage::Error(e) => {
                                let _ = setup_tx.send(Err(DeformError::Protocol(format!(
                                    "server auth error: {e}"
                                ))));
                                return;
                            }
                            other => {
                                let _ = setup_tx.send(Err(DeformError::Protocol(format!(
                                    "unexpected control message during auth: {other:?}"
                                ))));
                                return;
                            }
                        }

                        // Setup succeeded
                        let _ = setup_tx.send(Ok(()));

                        // --- Runtime phase ---
                        let tick_thread = tokio::spawn(async move {
                            let mut states = HashMap::new();
                            let state = lobby_info.game_state;

                            // state.reset_ball();
                            // state.position_p0();
                            // state.position_p1();

                            states.insert(lobby_info.tick, state.clone());

                            let tick_info = QuicBackend {
                                info_per_tick: HashMap::new(),
                                local_tick: lobby_info.tick,
                                remote_tick: lobby_info.tick,
                                // events_queue: Vec::new(),
                                last_remote_status: LobbyStatus::NotStarted,
                                inputs: HashMap::new(),

                                // rpc_client,
                                connection,
                                // control_send,
                                control_recv,

                                player,
                                // lobby,
                                // lobby_id,
                                terminate: terminate_clone,
                                set_inputs_receiver,
                                sdk_game_state: sdk_game_state_clone.clone(),
                                min_ticks_ahead: 4,
                                max_ticks_ahead: 3 * 4,

                                avg_rtt: Duration::from_millis(50),
                                user_logic: T::default(),
                                // dropped_datagrams: 0,
                                // stale_datagrams: 0,
                                // smoother: RollbackSmoother::new(state.players.len()),
                            };

                            if let Err(e) = tick_info.tick_loop().await {
                                if let Ok(mut shared) = sdk_game_state_clone.lock() {
                                    shared.internal_error = Err(e);
                                }
                            }
                        });

                        if let Err(e) = tick_thread.await {
                            // if error aquiring lock, there is really no way to report it
                            if let Ok(mut shared) = sdk_game_state_clone_2.lock() {
                                shared.internal_error =
                                    Err(DeformError::BackendPanicked(format!("{e:?}")));
                            }
                        }
                    });
                }
                Err(e) => {
                    let _ = setup_tx.send(Err(DeformError::Connection(format!(
                        "failed to build tokio runtime: {e}"
                    ))));
                }
            }
            backend_dead_clone.store(true, Ordering::Relaxed);
        });

        setup_rx.blocking_recv().map_err(|_| {
            DeformError::Connection("setup thread terminated unexpectedly".into())
        })??;

        Ok(DeformClient {
            terminate,
            set_inputs_sender,
            sdk_game_state,
            backend_dead,
        })
    }

    pub async fn tick_loop(mut self) -> DeformResult {
        // FIX: initial state is already in the game state message, read it from there
        let current_tick_info: TickInfo<T> = {
            let shared = self
                .sdk_game_state
                .lock()
                .map_err(|_| DeformError::LockPoisoned)?;

            shared.tick_info.clone()
        };
        self.info_per_tick.insert(0, current_tick_info);

        let mut tick_sleep = Box::pin(sleep(Duration::from_micros(16667)));
        let mut visual_ticker = interval(Duration::from_micros(16667));
        let mut inputs_ticker = interval(Duration::from_micros(COMMIT_INPUTS_MICROS));
        let mut rtt_ticker = interval(Duration::from_millis(RTT_SAMPLE_INTERVAL_MS));

        let mut terminated = false;

        loop {
            tokio::select! {
                // Tick every ~16ms (or more, depending on time dilation)
                _ = &mut tick_sleep => {
                    if self.last_remote_status != LobbyStatus::Finished {
                        let min_target_tick = self.remote_tick + self.min_ticks_ahead;
                        let current_tick = self.local_tick;

                        if current_tick < min_target_tick {
                            let delta_ticks = min_target_tick - current_tick;
                            // #[cfg(feature = "log")]
                            // warn!("Ticking to catch up to remote slot - from {current_tick} to {min_target_tick}");
                            for _ in 0..delta_ticks {
                                self.advance_local_simulation()?
                                // finish is handled when server tells us, not here
                            }
                        } else {
                            let max_target_tick = self.remote_tick + self.max_ticks_ahead;
                            if current_tick < max_target_tick {
                                self.advance_local_simulation()?
                                // finish is handled when server tells us, not here
                            }
                        }
                    } else {
                        break;
                    }

                    tick_sleep = Box::pin(sleep(self.compute_dilated_tick_interval()));
                }

                // Visual: fixed 60fps update independent of simulation rate
                _ = visual_ticker.tick() => {
                    // tick_info.smoother.decay();

                    // FIX: RETURN ERROR
                    if let Some(state) = self.info_per_tick.get(&self.local_tick) {
                        let visual_state = state.clone();
                        // tick_info.smoother.apply(&mut visual_state);
                        {
                            let mut shared = self.sdk_game_state.lock().map_err(|_| DeformError::LockPoisoned)?;
                            shared.tick_info = visual_state;
                            shared.remote_status = self.last_remote_status;
                            // shared.events.append(&mut tick_info.events_queue);
                        }
                    }
                }

                // Commit inputs periodically
                _ = inputs_ticker.tick() => {
                    if self.last_remote_status != LobbyStatus::Finished {
                        // #[cfg(feature = "tracy")]
                        // {
                        //     if let Some(max_input) = self.inputs.keys().max() {
                        //         if let Some(client) = tracy_client::Client::running() {
                        //             client.plot(tracy_client::plot_name!("commit_inputs"), *max_input as f64);
                        //         }
                        //     }
                        // }

                        self.commit_inputs().await?;
                    }
                }

                // Sample RTT from quinn (already an EWMA internally)
                _ = rtt_ticker.tick() => {
                    if self.last_remote_status != LobbyStatus::Finished {
                        self.avg_rtt = self.connection.rtt();

                        // #[cfg(feature = "tracy")]
                        // {
                        //     if let Some(client) = tracy_client::Client::running() {
                        //         client.plot(
                        //             tracy_client::plot_name!("RTT"),
                        //             self.avg_rtt.as_secs_f64() * 1000.0,
                        //         );
                        //     }
                        // }

                        self.update_ticks_ahead()?;
                    }
                }

                // Shutdown signal
                _ = self.terminate.notified() => {
                    // #[cfg(feature = "log")]
                    // warn!("Shutdown signal received; exiting");
                    terminated = true;
                    break;
                }

                // New inputs from the game engine
                new_inputs = self.set_inputs_receiver.recv() => {
                    if new_inputs.is_none() {
                        break;
                    }
                    if self.last_remote_status != LobbyStatus::Finished {
                        if let Some(new_inputs) = new_inputs {
                            self.inputs.entry(self.local_tick).or_insert(new_inputs);
                        }
                    }
                }

                // Receive game state updates via unreliable datagrams
                datagram = self.connection.read_datagram() => {
                    if self.last_remote_status != LobbyStatus::Finished {
                        let bytes = datagram.map_err(|e| DeformError::Connection(e.to_string()))?;
                        self.process_server_update(&bytes, &mut tick_sleep).await?;
                    }
                }

                // Receive control messages on the reliable stream
                control_msg = stream_read_msg(&mut self.control_recv) => {
                    let bytes = control_msg?;
                    match wincode::deserialize::<ControlMessage>(&bytes) {
                        Ok(ControlMessage::Finish) => {
                            self.last_remote_status = LobbyStatus::Finished;
                            break;
                        }
                        Ok(ControlMessage::Error(e)) => {
                            return Err(DeformError::Protocol(format!("server error: {e}")));
                        }
                        Ok(other) => {
                            return Err(DeformError::Protocol(format!("unexpected control message: {other:?}")));
                        }
                        Err(e) => {
                            return Err(DeformError::Deserialize(format!("control message: {e:?}")));
                        }
                    }
                }
            }
        }

        if !terminated && self.last_remote_status == LobbyStatus::Finished {
            // // Update shared state one last time (through smoother for consistency)
            // tick_info.smoother.decay();
            if let Some(tick_info) = self.info_per_tick.get(&self.local_tick) {
                let visual_state = tick_info.clone();
                {
                    let mut shared = self
                        .sdk_game_state
                        .lock()
                        .map_err(|_| DeformError::LockPoisoned)?;
                    // tick_info.smoother.apply(&mut visual_state);
                    shared.tick_info = visual_state;
                    shared.remote_status = self.last_remote_status;
                    shared.user_logic = self.user_logic.clone();
                }
            }
            // Wait for termination signal
            self.terminate.notified().await;
        }

        self.connection.close(quinn::VarInt::from_u32(0), b"done");

        Ok(())
    }

    /// Change our ticks ahead target based on the current RTT
    pub fn update_ticks_ahead(&mut self) -> DeformResult {
        // #[cfg(feature = "tracy")]
        // let _span = tracy_client::span!("update_ticks_ahead");

        let rtt_secs = self.avg_rtt.as_secs_f64();
        let mut rtt_micros = rtt_secs * 1_000_000.0;
        // to be conservative, add 3ns to make it so that values are slightly pushed over the edge
        rtt_micros += 3000.0;
        // adding 10% worked well in the past
        // rtt_micros += 0.1 * rtt_micros;

        // Full RTT is required, not RTT/2: remote_tick is already RTT/2 old when received,
        // so inputs travel another RTT/2 before reaching the server, totalling one full RTT
        // of server advancement since the observed state was sent. +1 absorbs commit-timer jitter.
        self.min_ticks_ahead = (rtt_micros / 16666.667).ceil() as u64 + 1;
        self.max_ticks_ahead = (3 * self.min_ticks_ahead).max(5);

        // #[cfg(feature = "tracy")]
        // {
        //     if let Some(client) = tracy_client::Client::running() {
        //         client.plot(
        //             tracy_client::plot_name!("min ticks ahead"),
        //             self.min_ticks_ahead as f64,
        //         );
        //         client.plot(
        //             tracy_client::plot_name!("max ticks ahead"),
        //             self.max_ticks_ahead as f64,
        //         );
        //     }
        // }

        {
            let mut shared = self
                .sdk_game_state
                .lock()
                .map_err(|_| DeformError::LockPoisoned)?;
            shared.stats.ping_ms = rtt_secs * 1_000.0;
        }

        Ok(())
    }

    /// Change the time between frames according to how much we are ahead of the simulation.
    /// The goal of this is to make it so that if we are way ahead of the server (server lagged etc),
    /// then we start to slow down to let it catch up.
    ///
    /// We have a `min_ticks_ahead` target that is computed based on the latency to the server. A % is taken using this value. For example, if the target is 10, and we are exactly 10 ticks ahead of the server, the % is 0. If we are 20 ticks ahead of the server, the % is 10. So, the percentage varies according to how much we expect to be ahead of the server.
    fn compute_dilated_tick_interval(&mut self) -> Duration {
        // #[cfg(feature = "tracy")]
        // let _span = tracy_client::span!("compute_dilated_tick_interval");
        /// <30% ahead
        const BASE_SLEEP_MS: f32 = 16.666667;
        /// 30% to 60% ahead
        const MID_SLEEP_MS: f32 = 25.0;
        /// >60%
        const MAX_SLEEP_MS: f32 = 66.666667;

        let ticks_ahead = self.local_tick.saturating_sub(self.remote_tick);

        let ahead_over_min = ticks_ahead.saturating_sub(self.min_ticks_ahead) as f32;
        let window = (self.max_ticks_ahead.saturating_sub(self.min_ticks_ahead)).max(1) as f32;
        let ahead_percent = (ahead_over_min / window).max(0.0);

        let sleep_ms = if ahead_percent <= 0.30 {
            BASE_SLEEP_MS
        } else if ahead_percent <= 0.60 {
            let t = ((ahead_percent - 0.30) / 0.30).clamp(0.0, 1.0);
            BASE_SLEEP_MS.lerp(MID_SLEEP_MS, t)
        } else {
            let t = ((ahead_percent - 0.60) / 0.40).clamp(0.0, 1.0);
            MID_SLEEP_MS.lerp(MAX_SLEEP_MS, t)
        };

        let micros = (sleep_ms * 1000.0) as u64;

        // #[cfg(feature = "tracy")]
        // if let Some(client) = tracy_client::Client::running() {
        //     client.plot(tracy_client::plot_name!("sleep_time"), sleep_ms as f64);
        // }

        Duration::from_micros(micros)
    }

    pub fn advance_local_simulation(&mut self) -> DeformResult {
        // #[cfg(feature = "tracy")]
        // let _span = tracy_client::span!("advance_local_simulation");

        // #[cfg(feature = "tracy")]
        // if let Some(client) = tracy_client::Client::running() {
        //     client.plot(
        //         tracy_client::plot_name!("current_vs_remote"),
        //         current_tick as f64 - self.remote_tick as f64,
        //     );
        // }

        let current_tick = self.local_tick;
        let new_tick = self.local_tick + 1;

        let current_info = self
            .info_per_tick
            .get(&current_tick)
            .ok_or(DeformError::InvalidState("slot not found"))?;

        let mut new_players_inputs: HashMap<Pubkey, T::Inputs> =
            HashMap::with_capacity(current_info.inputs.len());

        // ainda por cima acho que ele nunca era pruned sequer...
        // NOTE: we are already iterating the new array, which is cloned from the previous, so we can just edit in-place
        for (player, inputs) in new_players_inputs.iter_mut() {
            *inputs = if *player == self.player {
                // for our own player: try to get from the map. else, copy previous value, pruning it
                if let Some(provided_inputs) = self.inputs.get(&current_tick) {
                    provided_inputs.clone()
                } else {
                    inputs.predict()
                }
            } else {
                inputs.predict()
            }
        }

        // #[cfg(feature = "tracy")]
        // if let Some(client) = tracy_client::Client::running() {
        //     client.plot(tracy_client::plot_name!("advance_sim"), new_slot as f64);
        // }

        let new_state = self
            .user_logic
            .advance_frame(&current_info.game_state, &new_players_inputs)
            .map_err(|e| DeformError::UserLogic(Box::new(e)))?;

        let next_info = TickInfo {
            game_state: new_state,
            inputs: new_players_inputs,
        };

        self.local_tick = new_tick;
        self.info_per_tick.insert(new_tick, next_info);

        Ok(())
    }

    pub async fn commit_inputs(&mut self) -> DeformResult {
        // #[cfg(feature = "tracy")]
        // let _span = tracy_client::span!("commit_inputs");

        if self.inputs.is_empty() {
            return Ok(());
        }

        let ix = ServerUnreliableInstruction::<T::Inputs>::BatchSetInputs(self.inputs.clone());
        let bytes = wincode::serialize(&ix)?;

        self.connection
            .send_datagram(bytes.into())
            .map_err(|e| DeformError::Connection(e.to_string()))?;

        Ok(())
    }

    pub async fn process_server_update(
        &mut self,
        bytes: &[u8],
        tick_sleep: &mut Pin<Box<Sleep>>,
    ) -> DeformResult {
        // #[cfg(feature = "tracy")]
        // let _span = tracy_client::span!("process_server_update");

        let message: ServerResponse<T::Inputs, T::GameState> = wincode::deserialize(bytes)?;
        match message {
            ServerResponse::Error(e) => {
                return Err(DeformError::Protocol(e));
            }
            ServerResponse::NewState(new_lobby_state) => {
                // #[cfg(feature = "tracy")]
                // if let Some(client) = tracy_client::Client::running() {
                //     client.plot(
                //         tracy_client::plot_name!("last_tick_slot"),
                //         new_lobby_state.last_tick_slot as f64,
                //     );
                // }

                let old_remote_tick = self.remote_tick;
                let new_remote_tick = new_lobby_state.tick;
                let new_remote_status = new_lobby_state.status;
                let new_tick_info: TickInfo<T> = new_lobby_state.into();

                // no matter if the new state is old or not, if the new state is Finished, we end the match and no other checks or pruning are performed
                if matches!(new_remote_status, LobbyStatus::Finished) {
                    self.inputs.clear();
                    self.info_per_tick.clear();
                    self.info_per_tick.insert(new_remote_tick, new_tick_info);
                    self.remote_tick = new_remote_tick;
                    self.local_tick = new_remote_tick;
                    self.last_remote_status = new_remote_status;
                    self.inputs.clear();

                    // self.events_queue.push(GameEvent::StateTransition {
                    //     old: GameStateEnum::Playing,
                    //     new: GameStateEnum::Finished,
                    // });

                    return Ok(());
                }

                let mut gap = false;
                let mut rollback = false;

                // if the old tick is too old or repeated, just leave and do nothing
                if old_remote_tick >= new_remote_tick {
                    // TODO: log something
                    return Ok(());
                }

                // if the new tick is ahead of our local tick, we have fallen behind the server, and must fast-forward.
                // the latest state is always under the ID `tick_info.local_tick`, so it will never exist
                if new_remote_tick > self.local_tick {
                    // let last_computed_state = self
                    //     .info_per_tick
                    //     .get(&self.local_tick)
                    //     .ok_or(anyhow!("Local state not found, wtf"))?;
                    // manually_emit_events(
                    //     last_computed_state,
                    //     &new_lobby_state.game_state,
                    //     &mut self.events_queue,
                    // );
                    // self.smoother.reset();
                    self.remote_tick = new_remote_tick;
                    self.local_tick = new_remote_tick;
                    self.info_per_tick.clear();
                    self.info_per_tick.insert(new_remote_tick, new_tick_info);
                    self.last_remote_status = new_remote_status;
                    self.inputs.clear();

                    // trigger immediate catch-up on the next select iteration
                    tick_sleep.as_mut().reset(tokio::time::Instant::now());

                    return Ok(());
                }

                // the expected scenario is `new_remote_tick == old_remote_tick + 1`.
                // if this does not happen, a gap was detected, and will need to be taken care of.
                if new_remote_tick > old_remote_tick + 1 {
                    gap = true;
                }

                self.last_remote_status = new_remote_status;
                self.remote_tick = new_remote_tick;

                // if new_lobby_state.tick <= self.remote_tick {
                //     self.stale_datagrams += 1;
                //     #[cfg(feature = "tracy")]
                //     if let Some(client) = tracy_client::Client::running() {
                //         client.plot(
                //             tracy_client::plot_name!("stale datagrams"),
                //             self.stale_datagrams as f64,
                //         );
                //     }
                //     return Ok(());
                // }

                // // Count gaps in received tick sequence as dropped datagrams
                // let gap = new_lobby_state
                //     .tick
                //     .saturating_sub(self.remote_tick + 1);
                // if gap > 0 {
                //     self.dropped_datagrams += gap;
                //     #[cfg(feature = "tracy")]
                //     if let Some(client) = tracy_client::Client::running() {
                //         client.plot(
                //             tracy_client::plot_name!("dropped datagrams"),
                //             self.dropped_datagrams as f64,
                //         );
                //     }
                // }

                // prune all local inputs that are older than the new remote tick
                self.inputs.retain(|tick, _| *tick >= new_remote_tick);

                // #[cfg(feature = "log")]
                // warn!("QUIC received {}", new_lobby_state.tick);

                // #[cfg(feature = "tracy")]
                // if let Some(client) = tracy_client::Client::running() {
                //     client.plot(
                //         tracy_client::plot_name!("remote_tick (clean)"),
                //         new_lobby_state.tick as f64,
                //     );
                // }

                // if a gap was detected, no need to compare inputs. we have to rollback either way.
                // while these inputs could be correct, the previous missed frames could be wrong, causing divergence.
                // this is unlikely and should resolve itself quickly, but is still an issue we need to handle to ensure events aren't missed
                if gap {
                    // let old_remote_state = self
                    //     .info_per_tick
                    //     .get(&old_remote_tick)
                    //     .ok_or(DeformError::InvalidState("Remote state not found, wtf"))?;

                    // manually_emit_events(
                    //     old_remote_state,
                    //     &new_lobby_state.game_state,
                    //     &mut self.events_queue,
                    // );

                    self.handle_rollback(new_tick_info, new_remote_tick, tick_sleep)?;

                    // prune
                    for slot in old_remote_tick..new_remote_tick {
                        self.info_per_tick.remove(&slot);
                    }

                    return Ok(());
                }

                // everything was ok, so now we can finally check that the inputs match
                // according to the previous checks, the remote tick must exist in a previous predicted state, so error if it doesn't
                let predicted_inputs = &self
                    .info_per_tick
                    .get(&new_remote_tick)
                    .ok_or(DeformError::InvalidState(
                        "remote tick has not been predicted",
                    ))?
                    .inputs;

                let remote_inputs = &new_tick_info.inputs;

                // compare inputs from all players, and check if they match the ones the server sent
                for (player, predicted_input) in predicted_inputs.iter() {
                    let remote_input = remote_inputs.get(player).ok_or(
                        DeformError::InvalidState("player not found in remote inputs"),
                    )?;

                    if remote_input != predicted_input {
                        rollback = true;
                        break;
                    }
                }

                // if !rollback {
                //     // even though absolutely nothing went wrong, PowerupSpawnScheduled is a special case where the server is responsible for spawning powerups.
                //     // there is no need to go and check all the other events, just this one.
                //     // since handling a rollback also implies manually_emit_events, this is a lazy solution but it will work fine

                //     if predicted_state.scheduled_powerup.is_none()
                //         && new_lobby_state.game_state.scheduled_powerup.is_some()
                //     {
                //         rollback = true;
                //     }
                // }

                if rollback {
                    // manually_emit_events(
                    //     predicted_state,
                    //     &new_lobby_state.game_state,
                    //     &mut self.events_queue,
                    // );
                    self.handle_rollback(new_tick_info, new_remote_tick, tick_sleep)?;
                }

                // prune
                for slot in old_remote_tick..new_remote_tick {
                    self.info_per_tick.remove(&slot);
                }
            }
        }

        Ok(())
    }

    /// Takes a new state as the source of truth, rolling back to it,
    /// and applying the known inputs that have happened after this new state.
    ///
    /// NOTE: old inputs are not purged here (for now)
    ///
    /// Additionally, [`GameEvent::ManualPowerupActivation`] is emitted manually if necessary, see the NOTE: below
    pub fn handle_rollback(
        &mut self,
        // the state that will be used as the new source of truth
        new_tick_info: TickInfo<T>,
        conflicting_tick: u64,
        tick_sleep: &mut Pin<Box<Sleep>>,
    ) -> DeformResult {
        // #[cfg(feature = "tracy")]
        // if let Some(client) = tracy_client::Client::running() {
        //     client.message("rollback", 0);
        // }

        let previous_local_tick = self.local_tick;
        // at this point, there was a predicted state, meaning the local tick is either == or > than the remote tick
        // by using remove here we avoid a clone
        // in the (impossible?) case where previous_local_tick == conflicting_tick,
        // right below a state gets inserted into conflicting_tick so everything should be fine
        let pre_rollback_info = self
            .info_per_tick
            .remove(&previous_local_tick)
            .ok_or(DeformError::InvalidState("State not found!"))?;

        // insert the new state as-is, and update our tick to match it
        self.info_per_tick.insert(conflicting_tick, new_tick_info);

        self.local_tick = conflicting_tick;

        // #[cfg(feature = "log")]
        // warn!(
        //     "Rollback was triggered. Rolling back to {} and recomputing to {}",
        //     new_tick, previous_local_tick
        // );
        // run the simulation until we catch up to the current local tick
        // this will automatically reuse any registered inputs, and re-predict as needed
        for _tick in conflicting_tick..previous_local_tick {
            self.advance_local_simulation()?;
            // finish is handled when server tells us, not here
        }

        let post_rollback_info = self
            .info_per_tick
            .get(&self.local_tick)
            .ok_or(DeformError::InvalidState("State not found!"))?;

        self.user_logic
            .on_rollback(pre_rollback_info, post_rollback_info)
            .map_err(|e| DeformError::UserLogic(Box::new(e)))?;

        // // compute the new offset from previous frame to current frame
        // // uses the state @ current tick before and after rollback (tick is the same!)
        // if let Some(pre) = pre_rollback_state.as_ref() {
        //     if let Some(post) = self.states.get(&self.local_tick) {
        //         self.smoother.on_rollback(pre, post);
        //     }
        // }

        tick_sleep
            .as_mut()
            .reset(tokio::time::Instant::now() + self.compute_dilated_tick_interval());
        // #[cfg(feature = "log")]
        // warn!("after rollback, ticks is {}", self.local_tick);

        Ok(())
    }
}

async fn fetch_lobby<I: DeformInputs, G: DeformGameState>(
    lobby: &Pubkey,
    rpc_client: &RpcClient,
) -> DeformResult<Lobby<I, G>> {
    let account = rpc_client
        .get_account(&solana_sdk::pubkey::Pubkey::new_from_array(*lobby))
        .map_err(|e| DeformError::Rpc(e.to_string()))?;
    Lobby::from_bytes(&account.data).map_err(|e| DeformError::Deserialize(format!("lobby: {e:?}")))
}

// ---------------------------------------------------------------------------
// Certificate verification bypass for dev mode
// ---------------------------------------------------------------------------

#[derive(Debug)]
struct SkipCertVerification;

impl rustls::client::danger::ServerCertVerifier for SkipCertVerification {
    fn verify_server_cert(
        &self,
        _end_entity: &rustls::pki_types::CertificateDer<'_>,
        _intermediates: &[rustls::pki_types::CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> std::result::Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &rustls::pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &rustls::pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        rustls::crypto::ring::default_provider()
            .signature_verification_algorithms
            .supported_schemes()
    }
}
