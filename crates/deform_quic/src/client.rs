use better_tokio_select::tokio_select;
use glam::FloatExt;
use quinn::crypto::rustls::QuicClientConfig;
use std::{
    collections::HashMap,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};
use tokio::{
    sync::{Notify, mpsc, oneshot},
    time::{Sleep, interval, sleep_until},
};

use deform_core::{
    DeformClient, DeformError, DeformInputs, DeformResult, DeformSharedBackendState,
    DeformUserLogic, Pubkey, Smooth, TickInfo,
    accounts::lobby::{Lobby, LobbyFinished, LobbyState, started::LobbyOngoing},
    error::{UserFacingError, UserFacingResult},
};

use crate::{
    ALPN_PROTOCOL, DeformQuicLogic, ReliableMessage, UnreliableServerInstruction,
    UnreliableServerResponse, UserIdentification,
};

pub(crate) struct QuicBackend<Q: DeformQuicLogic + Send + 'static> {
    pub local_tick: u64,
    // pub remote_tick: u64, // stored in the remote lobby
    // we do not store the entire Lobby as we want to avoid cloning the UserLogic
    // TODO: but would that be desireable so we can roll back to a previous UserLogic state?
    pub info_per_tick: HashMap<u64, TickInfo<Q::UserLogic>>,
    pub remote_lobby: Lobby<Q::UserLogic>,
    // these are the inputs from our own player, appended only by set_inputs().
    // FIX: this might not be necessary. We may be able to just store the latest inputs, then reuse the old ones from `info_per_tick`. I only did it this way because it was easier in my head
    pub inputs: HashMap<u64, <Q::UserLogic as DeformUserLogic>::Inputs>,

    // pub rpc_client: Arc<RpcClient>,
    pub connection: quinn::Connection,
    // pub control_send: quinn::SendStream,
    pub control_recv: quinn::RecvStream,

    pub player: Pubkey,
    // pub lobby: Pubkey,
    // pub lobby_id: u64,
    pub terminate: Arc<Notify>,
    pub set_inputs_receiver: mpsc::UnboundedReceiver<<Q::UserLogic as DeformUserLogic>::Inputs>,
    pub backend_state: Arc<std::sync::Mutex<DeformSharedBackendState<Q::UserLogic>>>,
    // gets cloned into the backend_state, so we could tecnically use that
    // but I use a local copy instead so I don't have to lock mutexes a lot
    pub user_logic: Q::UserLogic,

    pub smoother: <Q::UserLogic as DeformUserLogic>::Smoother,
    pub visual_tick_micros: u64,
    pub last_sim_instant: Instant,
    /// Absolute deadline for the next simulation tick. Anchored to the previous deadline
    /// (not to `Instant::now()`), so time spent doing per-tick work and scheduling jitter
    /// do not accumulate as drift relative to the server's fixed-rate clock.
    pub next_tick_deadline: tokio::time::Instant,

    pub avg_rtt: Duration,
    /// If ticks are below this, simulation is fast forwarded. Also used to compute time dilation.
    pub min_ticks_ahead: u64,
    /// If ticks are above this, simulation is stopped; hard limit. It is always at least 5.
    pub max_ticks_ahead: u64,
    // /// Cumulative count of datagrams inferred to have been dropped (gaps in received ticks).
    // pub dropped_datagrams: u64,
    // /// Cumulative count of stale/out-of-order datagrams (tick <= remote_tick).
    // pub stale_datagrams: u64,
}

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

impl<Q: DeformQuicLogic + Send + 'static> QuicBackend<Q> {
    pub fn init(
        server_addr: String,
        server_name: String,
        lobby: Lobby<Q::UserLogic>,
        player: Pubkey,
        skip_cert_verify: bool,
        visual_tick_micros: u64,
        auth: Q::Auth,
    ) -> UserFacingResult<Q::UserLogic, DeformClient<Q::UserLogic>> {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let (setup_tx, setup_rx) = oneshot::channel::<DeformResult>();

        let (lobby, user_logic, starting_tick_info) = match &lobby.state {
            LobbyState::Finished(_) => {
                Err(DeformError::InvalidState("Game already ended!".into()))?
            }
            LobbyState::NotStarted(not_started) => {
                let mut inputs = HashMap::new();
                for player in not_started.player_status.keys() {
                    inputs.insert(
                        *player,
                        <Q::UserLogic as DeformUserLogic>::Inputs::default(),
                    );
                }

                let user_logic =
                    <Q::UserLogic as DeformUserLogic>::new_from_lobby(&lobby.metadata, not_started)
                        .map_err(|e| UserFacingError::User(e))?;
                let game_state = <Q::UserLogic as DeformUserLogic>::new_game_from_lobby(
                    &lobby.metadata,
                    not_started,
                )
                .map_err(|e| UserFacingError::User(e))?;
                let tick_info = TickInfo {
                    game_state: game_state.clone(),
                    inputs,
                };

                (
                    Lobby {
                        metadata: lobby.metadata.clone(),
                        state: LobbyState::Ongoing(LobbyOngoing {
                            tick: 0,
                            tick_info: tick_info.clone(),
                            user_logic: user_logic.clone(),
                        }),
                    },
                    user_logic,
                    tick_info,
                )
            }
            LobbyState::Ongoing(state) => (
                lobby.clone(),
                state.user_logic.clone(),
                state.tick_info.clone(),
            ),
        };

        let terminate = Arc::new(Notify::new());
        let (set_inputs_sender, set_inputs_receiver) =
            mpsc::unbounded_channel::<<Q::UserLogic as DeformUserLogic>::Inputs>();

        let backend_state = Arc::new(std::sync::Mutex::new(DeformSharedBackendState::<
            Q::UserLogic,
        >::new_from_lobby(
            lobby.clone()
        )?));
        let backend_dead = Arc::new(AtomicBool::new(false));

        // cursed
        let backend_state_clone = backend_state.clone();
        let backend_state_clone_2 = backend_state.clone();

        let terminate_clone = terminate.clone();
        let backend_dead_clone = backend_dead.clone();

        // tracing::info!(
        //     "QUIC init with server: {} (SNI: {})",
        //     server_addr,
        //     server_name
        // );

        let _rss_thread = thread::spawn(move || {
            match tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => {
                    rt.block_on(async move {
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

                        // make a handshake message and send it to the server
                        // if server returns an error, then communicate it to setup_tx and just exit. this is already done below

                        // Send handshake
                        let handshake_message =
                            ReliableMessage::<Q>::Identification(UserIdentification {
                                user: player.clone(),
                                lobby_id: lobby.metadata.id,
                                auth,
                            });

                        if let Err(e) = handshake_message.write(&mut control_send).await {
                            let _ = setup_tx.send(Err(e));
                            return;
                        }

                        // Wait for AuthOk
                        let control_msg = match ReliableMessage::<Q>::read(&mut control_recv).await
                        {
                            Ok(msg) => msg,
                            Err(e) => {
                                let _ = setup_tx.send(Err(e));
                                return;
                            }
                        };

                        match control_msg {
                            ReliableMessage::Authorized => {}
                            ReliableMessage::Error(e) => {
                                // TODO: I think this is bad
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
                            // let mut states = HashMap::new();
                            // let state = lobby_info.game_state;

                            // state.reset_ball();
                            // state.position_p0();
                            // state.position_p1();

                            // states.insert(lobby_info.tick, state.clone());

                            let mut smoother =
                                <Q::UserLogic as DeformUserLogic>::Smoother::default();
                            let decay_ratio = visual_tick_micros as f32
                                / <Q::UserLogic as DeformUserLogic>::TICK_RATE_MICROS as f32;
                            smoother.scale_decay(decay_ratio);

                            let tick_info = QuicBackend::<Q> {
                                info_per_tick: HashMap::new(),
                                local_tick: 0,
                                remote_lobby: lobby,
                                inputs: HashMap::new(),

                                connection,
                                control_recv,

                                player,
                                terminate: terminate_clone,
                                set_inputs_receiver,
                                backend_state: backend_state_clone.clone(),
                                user_logic,
                                min_ticks_ahead: 4,
                                max_ticks_ahead: 3 * 4,

                                smoother,
                                visual_tick_micros,
                                last_sim_instant: Instant::now(),
                                // real value is set at the top of `tick_loop`
                                next_tick_deadline: tokio::time::Instant::now(),

                                avg_rtt: Duration::from_millis(50),
                            };

                            if let Err(e) = tick_info.tick_loop(starting_tick_info).await
                                && let Ok(mut shared) = backend_state_clone.lock()
                            {
                                shared.internal_error = Err(e.into());
                            }
                        });

                        if let Err(e) = tick_thread.await {
                            // if error aquiring lock, there is really no way to report it
                            if let Ok(mut shared) = backend_state_clone_2.lock() {
                                shared.internal_error =
                                    Err(DeformError::BackendPanicked(format!("{e:?}")).into());
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
            backend_state,
            backend_dead,
        })
    }

    pub async fn tick_loop(
        mut self,
        starting_tick_info: TickInfo<Q::UserLogic>,
    ) -> UserFacingResult<Q::UserLogic> {
        self.info_per_tick.insert(0, starting_tick_info);

        self.next_tick_deadline = tokio::time::Instant::now()
            + Duration::from_micros(<Q::UserLogic as DeformUserLogic>::TICK_RATE_MICROS);
        let mut tick_sleep = Box::pin(sleep_until(self.next_tick_deadline));
        let mut visual_ticker = interval(Duration::from_micros(self.visual_tick_micros));
        let mut inputs_ticker = interval(Duration::from_micros(
            <Q::UserLogic as DeformUserLogic>::TICK_RATE_MICROS,
        ));
        let mut rtt_ticker = interval(Duration::from_millis(RTT_SAMPLE_INTERVAL_MS));

        let mut terminated = false;

        loop {
            tokio_select!(match .. {
                // Tick every ~16ms (or more, depending on time dilation)
                .. if let _ = &mut tick_sleep => {
                    let remote_tick = match &self.remote_lobby.state {
                        LobbyState::Finished(_) => break,
                        // Grace period: the match has not started on the server, so there is no
                        // authoritative stream to reconcile against. Predicting here would just get
                        // rolled back the moment the match goes live, so we hold at the initial state.
                        // The first `Started` datagram bootstraps us via the FastForward path.
                        LobbyState::NotStarted(_) => break,
                        LobbyState::Ongoing(ongoing) => {
                            let remote_tick = ongoing.tick;
                            let min_target_tick = remote_tick + self.min_ticks_ahead;
                            let current_tick = self.local_tick;

                            if current_tick < min_target_tick {
                                let delta_ticks = min_target_tick - current_tick;
                                // #[cfg(feature = "log")]
                                // tracing::warn!(
                                //     "Ticking to catch up to remote slot - from {current_tick} to {min_target_tick}"
                                // );
                                for _ in 0..delta_ticks {
                                    self.advance_local_simulation()?
                                    // finish is handled when server tells us, not here
                                }
                            } else {
                                let max_target_tick = ongoing.tick + self.max_ticks_ahead;
                                if current_tick < max_target_tick {
                                    self.advance_local_simulation()?
                                    // finish is handled when server tells us, not here
                                }
                            }
                            self.last_sim_instant = Instant::now();

                            remote_tick
                        }
                    };

                    // Advance the anchored deadline by the (variable) dilated interval rather than
                    // sleeping from `now`, so the work done in this arm does not accumulate as drift.
                    // Dilation is preserved: `compute_dilated_tick_interval` still decides the step.
                    let dilated = self.compute_dilated_tick_interval(remote_tick);
                    self.next_tick_deadline += dilated;
                    // If a stall pushed us a full tick past the deadline, resync to `now` so we
                    // don't fire a burst of back-to-back catch-up ticks (manual MissedTickBehavior::Delay).
                    let now = tokio::time::Instant::now();
                    if self.next_tick_deadline < now {
                        self.next_tick_deadline = now;
                    }
                    tick_sleep.as_mut().reset(self.next_tick_deadline);
                }

                // Visual: interpolate between previous and current sim state
                .. if let _ = visual_ticker.tick() => {
                    let prev_tick = self.local_tick.saturating_sub(1);

                    if let (Some(prev), Some(current)) = (
                        self.info_per_tick.get(&prev_tick),
                        self.info_per_tick.get(&self.local_tick),
                    ) {
                        let elapsed = self.last_sim_instant.elapsed().as_micros() as f32;
                        let t = (elapsed
                            / <Q::UserLogic as DeformUserLogic>::TICK_RATE_MICROS as f32)
                            .clamp(0.0, 1.0);

                        let mut visual_state = current.clone();
                        self.smoother
                            .apply(&prev.game_state, &mut visual_state.game_state, t);

                        {
                            let mut shared = self
                                .backend_state
                                .lock()
                                .map_err(|_| DeformError::LockPoisoned)?;

                            // TODO: cleaner way of doing this? I can only set the new state if the game is ongoing (or finished, but whatever)
                            if let LobbyState::Ongoing(ongoing) = &mut shared.lobby.state {
                                ongoing.tick_info = visual_state;
                            }
                        }
                    }
                }

                // Commit inputs periodically
                .. if let _ = inputs_ticker.tick() => {
                    // only while the match is live; no authoritative tick exists otherwise
                    if matches!(self.remote_lobby.state, LobbyState::Ongoing(_)) {
                        #[cfg(feature = "tracy")]
                        {
                            if let Some(max_input) = self.inputs.keys().max() {
                                if let Some(client) = tracy_client::Client::running() {
                                    client.plot(
                                        tracy_client::plot_name!("commit_inputs"),
                                        *max_input as f64,
                                    );
                                }
                            }
                        }

                        self.commit_inputs().await?;
                    }
                }

                // Sample RTT from quinn (already an EWMA internally)
                .. if let _ = rtt_ticker.tick() => {
                    if !matches!(self.remote_lobby.state, LobbyState::Finished(_)) {
                        self.avg_rtt = self.connection.rtt();

                        #[cfg(feature = "tracy")]
                        {
                            if let Some(client) = tracy_client::Client::running() {
                                client.plot(
                                    tracy_client::plot_name!("RTT"),
                                    self.avg_rtt.as_secs_f64() * 1000.0,
                                );
                            }
                        }

                        self.update_ticks_ahead()?;
                    }
                }

                // Shutdown signal
                .. if let _ = self.terminate.notified() => {
                    // #[cfg(feature = "log")]
                    // tracing::warn!("Shutdown signal received; exiting");
                    terminated = true;
                    break;
                }

                // New inputs from the game engine
                .. if let new_inputs = self.set_inputs_receiver.recv() => {
                    if new_inputs.is_none() {
                        break;
                    }
                    if !matches!(self.remote_lobby.state, LobbyState::Finished(_))
                        && let Some(new_inputs) = new_inputs
                    {
                        self.inputs.entry(self.local_tick).or_insert(new_inputs);
                    }
                }

                // Receive game state updates via unreliable datagrams
                .. if let datagram = self.connection.read_datagram() => {
                    if !matches!(self.remote_lobby.state, LobbyState::Finished(_)) {
                        let bytes = datagram.map_err(|e| DeformError::Connection(e.to_string()))?;
                        self.process_server_update(&bytes, &mut tick_sleep).await?;
                    }
                }

                // Receive control messages on the reliable stream
                .. if let control_msg = ReliableMessage::<Q>::read(&mut self.control_recv) => {
                    match control_msg? {
                        ReliableMessage::Finish(lobby) => {
                            self.remote_lobby = lobby;
                            break;
                        }
                        ReliableMessage::Error(e) => {
                            return Err(DeformError::Protocol(format!("server error: {e}")).into());
                        }
                        other => {
                            return Err(DeformError::Protocol(format!(
                                "unexpected control message: {other:?}"
                            ))
                            .into());
                        }
                    }
                }
            });
        }

        if !terminated && let LobbyState::Finished(mut finished) = self.remote_lobby.state {
            if let Some(tick_info) = self.info_per_tick.get(&self.local_tick) {
                let visual_state = tick_info.clone();
                {
                    let mut shared = self
                        .backend_state
                        .lock()
                        .map_err(|_| DeformError::LockPoisoned)?;

                    finished.0.tick_info = visual_state;
                    shared.lobby.state = LobbyState::Finished(finished);
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
        #[cfg(feature = "tracy")]
        let _span = tracy_client::span!("update_ticks_ahead");

        let rtt_secs = self.avg_rtt.as_secs_f64();
        let mut rtt_micros = rtt_secs * 1_000_000.0;
        // to be conservative, add 3ns to make it so that values are slightly pushed over the edge
        rtt_micros += 3000.0;
        // adding 10% worked well in the past
        // rtt_micros += 0.1 * rtt_micros;

        // Full RTT is required, not RTT/2: remote_tick is already RTT/2 old when received,
        // so inputs travel another RTT/2 before reaching the server, totalling one full RTT
        // of server advancement since the observed state was sent. +1 absorbs commit-timer jitter.
        self.min_ticks_ahead =
            (rtt_micros / <Q::UserLogic as DeformUserLogic>::TICK_RATE_MICROS as f64).ceil() as u64
                + 1;
        self.max_ticks_ahead = (3 * self.min_ticks_ahead).max(5);

        #[cfg(feature = "tracy")]
        {
            if let Some(client) = tracy_client::Client::running() {
                client.plot(
                    tracy_client::plot_name!("min ticks ahead"),
                    self.min_ticks_ahead as f64,
                );
                client.plot(
                    tracy_client::plot_name!("max ticks ahead"),
                    self.max_ticks_ahead as f64,
                );
            }
        }

        {
            let mut shared = self
                .backend_state
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
    fn compute_dilated_tick_interval(&mut self, remote_tick: u64) -> Duration {
        #[cfg(feature = "tracy")]
        let _span = tracy_client::span!("compute_dilated_tick_interval");
        let base_sleep_ms: f32 =
            <Q::UserLogic as DeformUserLogic>::TICK_RATE_MICROS as f32 / 1000.0;
        let mid_sleep_ms: f32 = base_sleep_ms * 1.5;
        let max_sleep_ms: f32 = base_sleep_ms * 4.0;

        let ticks_ahead = self.local_tick.saturating_sub(remote_tick);

        let ahead_over_min = ticks_ahead.saturating_sub(self.min_ticks_ahead) as f32;
        let window = (self.max_ticks_ahead.saturating_sub(self.min_ticks_ahead)).max(1) as f32;
        let ahead_percent = (ahead_over_min / window).max(0.0);

        let sleep_ms = if ahead_percent <= 0.30 {
            base_sleep_ms
        } else if ahead_percent <= 0.60 {
            let t = ((ahead_percent - 0.30) / 0.30).clamp(0.0, 1.0);
            base_sleep_ms.lerp(mid_sleep_ms, t)
        } else {
            let t = ((ahead_percent - 0.60) / 0.40).clamp(0.0, 1.0);
            mid_sleep_ms.lerp(max_sleep_ms, t)
        };

        let micros = (sleep_ms * 1000.0) as u64;

        #[cfg(feature = "tracy")]
        if let Some(client) = tracy_client::Client::running() {
            client.plot(tracy_client::plot_name!("sleep_time"), sleep_ms as f64);
        }

        Duration::from_micros(micros)
    }

    pub fn advance_local_simulation(&mut self) -> UserFacingResult<Q::UserLogic> {
        #[cfg(feature = "tracy")]
        let _span = tracy_client::span!("advance_local_simulation");

        #[cfg(feature = "tracy")]
        if let Some(client) = tracy_client::Client::running() {
            client.plot(
                tracy_client::plot_name!("current_vs_remote_adv"),
                self.local_tick as f64 - self.remote_tick as f64,
            );
        }

        let current_tick = self.local_tick;
        let new_tick = self.local_tick + 1;

        let current_info = self
            .info_per_tick
            .get(&current_tick)
            .ok_or(DeformError::InvalidState("slot not found".into()))?;

        // clone the old array so that we have the correct pubkeys
        // the inputs will be overwritten
        let mut new_players_inputs: HashMap<Pubkey, <Q::UserLogic as DeformUserLogic>::Inputs> =
            current_info.inputs.clone();

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

        #[cfg(feature = "tracy")]
        if let Some(client) = tracy_client::Client::running() {
            client.plot(tracy_client::plot_name!("advance_sim"), new_tick as f64);
        }

        let new_state = self
            .user_logic
            .advance_frame(&current_info.game_state, &new_players_inputs)
            .map_err(|e| UserFacingError::User(e))?;

        let next_info = TickInfo {
            game_state: new_state,
            inputs: new_players_inputs,
        };

        self.local_tick = new_tick;
        self.info_per_tick.insert(new_tick, next_info);

        Ok(())
    }

    pub async fn commit_inputs(&mut self) -> DeformResult {
        #[cfg(feature = "tracy")]
        let _span = tracy_client::span!("commit_inputs");

        if self.inputs.is_empty() {
            return Ok(());
        }

        let ix = UnreliableServerInstruction::<<Q::UserLogic as DeformUserLogic>::Inputs>::BatchSetInputs(self.inputs.clone());
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
    ) -> UserFacingResult<Q::UserLogic> {
        #[cfg(feature = "tracy")]
        let _span = tracy_client::span!("process_server_update");

        #[cfg(feature = "tracy")]
        if let Some(client) = tracy_client::Client::running() {
            client.plot(
                tracy_client::plot_name!("current_vs_remote_reception"),
                self.local_tick as f64 - self.remote_tick as f64,
            );
        }

        let UnreliableServerResponse {
            lobby_state: new_remote_state,
        }: UnreliableServerResponse<Q::UserLogic> =
            wincode::deserialize(bytes).map_err(|e| DeformError::Deserialize(e.to_string()))?;

        match new_remote_state {
            // no matter if the new state is old or not, if the new state is Finished, we end the match and no other checks or pruning are performed
            LobbyState::Finished(LobbyFinished(ref finished_state)) => {
                let new_remote_tick = finished_state.tick;
                let new_tick_info = finished_state.tick_info.clone();

                self.inputs.clear();
                self.info_per_tick.clear();
                self.info_per_tick.insert(new_remote_tick, new_tick_info);
                self.local_tick = new_remote_tick;
                self.remote_lobby.state = new_remote_state;
                self.inputs.clear();

                // self.events_queue.push(GameEvent::StateTransition {
                //     old: GameStateEnum::Playing,
                //     new: GameStateEnum::Finished,
                // });

                Ok(())
            }
            LobbyState::Ongoing(ongoing) => self.handle_new_ongoing(ongoing, tick_sleep),
            LobbyState::NotStarted(_) => Ok(()),
        }
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
        new_tick_info: TickInfo<Q::UserLogic>,
        conflicting_tick: u64,
        tick_sleep: &mut Pin<Box<Sleep>>,
    ) -> UserFacingResult<Q::UserLogic> {
        #[cfg(feature = "tracy")]
        if let Some(client) = tracy_client::Client::running() {
            client.message("rollback", 0);
        }

        let previous_local_tick = self.local_tick;
        // at this point, there was a predicted state, meaning the local tick is either == or > than the remote tick
        // by using remove here we avoid a clone
        // in the (impossible?) case where previous_local_tick == conflicting_tick,
        // right below a state gets inserted into conflicting_tick so everything should be fine
        let pre_rollback_info = self
            .info_per_tick
            .remove(&previous_local_tick)
            .ok_or(DeformError::InvalidState("State not found!".into()))?;

        // insert the new state as-is, and update our tick to match it
        self.info_per_tick.insert(conflicting_tick, new_tick_info);

        self.local_tick = conflicting_tick;

        // #[cfg(feature = "log")]
        // tracing::debug!(
        //     "Rollback triggered: rolling back to {} and recomputing to {}",
        //     conflicting_tick,
        //     previous_local_tick
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
            .ok_or(DeformError::InvalidState("State not found!".into()))?;

        self.smoother.on_rollback(
            &pre_rollback_info.game_state,
            &post_rollback_info.game_state,
        );

        self.user_logic
            .on_rollback(pre_rollback_info, post_rollback_info)
            .map_err(|e| UserFacingError::User(e))?;

        let dilated = self.compute_dilated_tick_interval(conflicting_tick);
        let new_deadline = tokio::time::Instant::now() + dilated;
        // keep the anchored deadline, but never push the next tick further out than it already was
        self.next_tick_deadline = new_deadline.min(self.next_tick_deadline);
        tick_sleep.as_mut().reset(self.next_tick_deadline);
        // #[cfg(feature = "log")]
        // tracing::debug!("after rollback, local tick is {}", self.local_tick);

        Ok(())
    }

    pub fn handle_new_ongoing(
        &mut self,
        remote_ongoing: LobbyOngoing<Q::UserLogic>,
        tick_sleep: &mut Pin<Box<Sleep>>,
    ) -> UserFacingResult<Q::UserLogic> {
        let new_remote_tick = remote_ongoing.tick;
        let old_remote_tick = match &self.remote_lobby.state {
            LobbyState::Ongoing(old_ongoing) => old_ongoing.tick,
            _ => Err(DeformError::InvalidState(
                "Previous lobby was not Ongoing".into(),
            ))?,
        };
        let new_tick_info = remote_ongoing.tick_info.clone();

        #[cfg(feature = "tracy")]
        if let Some(client) = tracy_client::Client::running() {
            client.plot(
                tracy_client::plot_name!("last_tick_slot"),
                new_remote_tick as f64,
            );
        }

        /// Trying to handle all cases was a mess to keep up with all invariants so this makes it cleaner. I assume the compiler will take care of this.
        /// IMPORTANT: each case is evaluated one after another; this means that the [`ReceivedScenario::Default`] branch will only trigger if all others do not.
        ///
        /// NOTE: in the case where there is no gap (`new_remote == old_remote + 1`) but the new remote is exactly equal to the local (`new_remote == local_sim`), then if falls through to the [`ReceivedScenario::Rollback`] and [`ReceivedScenario::Default`] branches.
        #[derive(Clone, Copy)]
        enum ReceivedScenario {
            /// The received state is too old or a repeat message.
            /// `new_remote <= old_remote`
            OldOrRepeated,
            /// The received state is strictly ahead of our own.
            /// `new_remote > local_sim`
            FastForward,
            /// The received state is ahead of the old remote state by more than one tick.
            /// `new_remote > old_remote + 1`
            Gap,
            /// The predicted inputs do not match the received ones, so we must roll back the simulation.
            Rollback,
            /// The default, expected scenario, were the remote state advanced by +1
            /// and we are still ahead of it, and the inputs match the prediction.
            Default,
        }

        let scenario = if old_remote_tick >= new_remote_tick {
            // if the old tick is too old or repeated, just leave and do nothing
            ReceivedScenario::OldOrRepeated
        } else if new_remote_tick > self.local_tick {
            // if the new tick is ahead of our local tick, we have fallen behind the server, and must fast-forward.
            // the latest state is always under the ID `tick_info.local_tick`, so it will never exist
            ReceivedScenario::FastForward
        } else if new_remote_tick > old_remote_tick + 1 {
            // the expected scenario is `new_remote_tick == old_remote_tick + 1`.
            // if this does not happen, a gap was detected, and will need to be taken care of.
            ReceivedScenario::Gap
        } else {
            // everything was ok, so now we can finally check that the inputs match
            // according to the previous checks, the remote tick must exist in a previous predicted state, so error if it doesn't
            let predicted_inputs = &self
                .info_per_tick
                .get(&new_remote_tick)
                .ok_or(DeformError::InvalidState(
                    "remote tick has not been predicted".into(),
                ))?
                .inputs;

            let remote_inputs = &new_tick_info.inputs;

            // compare inputs from all players, and check if they match the ones the server sent
            let mut mismatch = false;
            for (player, predicted_input) in predicted_inputs.iter() {
                let remote_input = remote_inputs.get(player).ok_or(DeformError::InvalidState(
                    "player not found in remote inputs".into(),
                ))?;

                if remote_input != predicted_input {
                    mismatch = true;
                    break;
                }
            }

            // if !mismatch {
            //     // even though absolutely nothing went wrong, PowerupSpawnScheduled is a special case where the server is responsible for spawning powerups.
            //     // there is no need to go and check all the other events, just this one.
            //     // since handling a rollback also implies manually_emit_events, this is a lazy solution but it will work fine

            //     if predicted_state.scheduled_powerup.is_none()
            //         && new_lobby_state.game_state.scheduled_powerup.is_some()
            //     {
            //         mismatch = true;
            //     }
            // }

            if mismatch {
                ReceivedScenario::Rollback
            } else {
                ReceivedScenario::Default
            }
        };

        // handle early-return scenarios
        match scenario {
            ReceivedScenario::OldOrRepeated => {
                // TODO: log something
                return Ok(());
            }
            ReceivedScenario::FastForward => {
                let last_computed_state =
                    self.info_per_tick
                        .get(&self.local_tick)
                        .ok_or(DeformError::InvalidState(
                            "Local state not found, wtf".into(),
                        ))?;
                // manually_emit_events(
                //     last_computed_state,
                //     &new_lobby_state.game_state,
                //     &mut self.events_queue,
                // );

                self.user_logic
                    .on_fast_forward(last_computed_state, &new_tick_info)
                    .map_err(|e| UserFacingError::User(e))?;

                self.smoother.reset();
                self.local_tick = new_remote_tick;
                self.info_per_tick.clear();
                self.info_per_tick.insert(new_remote_tick, new_tick_info);
                self.remote_lobby.state = LobbyState::Ongoing(remote_ongoing);
                self.inputs.clear();

                // trigger immediate catch-up on the next select iteration, re-anchoring the deadline
                self.next_tick_deadline = tokio::time::Instant::now();
                tick_sleep.as_mut().reset(self.next_tick_deadline);

                return Ok(());
            }
            _ => {}
        }

        // --- shared setup for Gap / Rollback / Default ---

        self.remote_lobby.state = LobbyState::Ongoing(remote_ongoing);

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
        // tracing::trace!("QUIC received tick {}", new_remote_tick);

        #[cfg(feature = "tracy")]
        if let Some(client) = tracy_client::Client::running() {
            client.plot(
                tracy_client::plot_name!("remote_tick (clean)"),
                new_remote_tick as f64,
            );
        }

        match scenario {
            // if a gap was detected, no need to compare inputs. we have to rollback either way.
            // while these inputs could be correct, the previous missed frames could be wrong, causing divergence.
            // this is unlikely and should resolve itself quickly, but is still an issue we need to handle to ensure events aren't missed
            ReceivedScenario::Gap => {
                // this could be remove() due to all the invariants but whatever, perf should be similar
                let old_remote_state =
                    self.info_per_tick
                        .get(&old_remote_tick)
                        .ok_or(DeformError::InvalidState(
                            "Remote state not found, wtf".into(),
                        ))?;

                // manually_emit_events(
                //     old_remote_state,
                //     &new_lobby_state.game_state,
                //     &mut self.events_queue,
                // );

                self.user_logic
                    .on_gap(old_remote_state, &new_tick_info)
                    .map_err(|e| UserFacingError::User(e))?;

                // a rollback is always triggered, as it is assumed that the simulation is now out of sync
                // so it is resimulated from the new tick up to the current tick
                // this also inserts the new state etc
                self.handle_rollback(new_tick_info, new_remote_tick, tick_sleep)?;
            }
            ReceivedScenario::Rollback => {
                // manually_emit_events(
                //     predicted_state,
                //     &new_lobby_state.game_state,
                //     &mut self.events_queue,
                // );
                self.handle_rollback(new_tick_info, new_remote_tick, tick_sleep)?;
            }
            ReceivedScenario::Default => {}
            _ => unreachable!(),
        }

        // prune
        for slot in old_remote_tick..new_remote_tick {
            self.info_per_tick.remove(&slot);
        }

        Ok(())
    }
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
