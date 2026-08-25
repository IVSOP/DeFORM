use std::{
    collections::{BTreeMap, HashMap},
    pin::Pin,
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

use better_tokio_select::tokio_select;
use deform_core::{
    ChannelInputs, DeformClient, DeformError, DeformInputs, DeformResult, DeformSharedBackendState,
    DeformUserLogic, Pubkey, Smooth, TickInfo,
    accounts::lobby::{Lobby, LobbyFinished, LobbyState, ongoing::LobbyOngoing},
    error::{UserFacingError, UserFacingResult},
};
use quinn::crypto::rustls::QuicClientConfig;
use tokio::{
    sync::{mpsc, oneshot},
    time::{Sleep, interval, sleep_until},
};
use tokio_util::sync::CancellationToken;

use crate::{
    ALPN_PROTOCOL, Compressed, DeformQuicLogic, ReliableMessage, ServerInstruction,
    StateUpdatePacket, UserIdentification,
    datagram::{DatagramDefragmentor, DatagramFragmentor},
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
    pub inputs: HashMap<u64, ChannelInputs<Q::UserLogic>>,

    // pub rpc_client: Arc<RpcClient>,
    pub connection: quinn::Connection,
    // pub control_send: quinn::SendStream,
    pub control_recv: quinn::RecvStream,

    pub player: Pubkey,
    // pub lobby: Pubkey,
    // pub lobby_id: u64,
    pub cancellation_token: CancellationToken,
    pub set_inputs_receiver: mpsc::UnboundedReceiver<ChannelInputs<Q::UserLogic>>,
    pub backend_state: Arc<std::sync::Mutex<DeformSharedBackendState<Q::UserLogic>>>,
    // gets cloned into the backend_state, so we could tecnically use that
    // but I use a local copy instead so I don't have to lock mutexes a lot
    pub user_logic: Q::UserLogic,

    pub smoother: <Q::UserLogic as DeformUserLogic>::Smoother,
    pub visual_tick_micros: u64,
    pub last_sim_instant: Instant,
    /// Measured duration of the last sim interval. Denominator for visual `t`.
    pub last_tick_interval: Duration,
    /// Absolute deadline for the next simulation tick. Anchored to the previous deadline
    /// (not to `Instant::now()`), so time spent doing per-tick work and scheduling jitter
    /// do not accumulate as drift relative to the server's fixed-rate clock.
    pub next_tick_deadline: tokio::time::Instant,
    /// Pessimistic estimate of how many of our inputs the server holds for future ticks.
    /// (we assume it has less than reality by being agressive when reducing and slow to increase)
    pub buffer_estimate: f32,
    /// Target bonus applied after a self-rollback, decaying on every server update.
    pub rollback_panic: f32,
    // /// Cumulative count of datagrams inferred to have been dropped (gaps in received ticks).
    // pub dropped_datagrams: u64,
    // /// Cumulative count of stale/out-of-order datagrams (tick <= remote_tick).
    // pub stale_datagrams: u64,
}

/// How often to publish the RTT into the stats, just so we don't do it constantly.
pub const RTT_SAMPLE_INTERVAL_MS: u64 = 500;

/// How many inputs the server should have queued up AFTER consuming the current tick's
const TARGET_BUFFER: f32 = 0.0;
/// Only after this deadzone do we start slowing the simulation down
const SLOWDOWN_DEADZONE: f32 = 1.0;

// coefficients for the math
const SPEEDUP_SOFTNESS: f32 = 1.0;
const SLOWDOWN_SOFTNESS: f32 = 3.0;
const SLOWDOWN_RATIO: f32 = 0.5;
// make the buffer estimate drop fast
const BUFFER_FALL: f32 = 0.60;
// make the buffer estimate climb slow
const BUFFER_RISE: f32 = 0.05;
/// When our own input is late this means we are either too slow or there was input loss, so instantly kick the tick target up
const ROLLBACK_KICK: f32 = 1.0;
/// Max value the tick target can be kicked by
const PANIC_MAX: f32 = 3.0;
const PANIC_DECAY: f32 = 0.955;
/// Freeze if we get this many ticks ahead. Means reports stopped and either server died or we lost connection.
const MAX_PREDICTION_TICKS: u64 = 30;

impl<Q: DeformQuicLogic + Send + 'static> QuicBackend<Q> {
    pub fn init(
        server_addr: String,
        server_name: String,
        lobby: Lobby<Q::UserLogic>,
        player: Pubkey,
        skip_cert_verify: bool,
        visual_tick_micros: u64,
        auth: Q::Auth,
        cancellation_token: CancellationToken,
    ) -> UserFacingResult<Q::UserLogic, DeformClient<Q::UserLogic>> {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let (setup_tx, setup_rx) = oneshot::channel::<DeformResult>();

        #[cfg(feature = "metrics")]
        deform_metrics::init(deform_metrics::RunInfo {
            backend: "quic",
            player: player.to_string(),
            lobby_id: lobby.metadata.id,
            tick_rate_micros: <Q::UserLogic as DeformUserLogic>::TICK_RATE_MICROS,
            extra: vec![("server".into(), server_addr.clone())],
        });

        let (lobby, user_logic, starting_tick_info) = match &lobby.state {
            LobbyState::Finished(_) => {
                Err(DeformError::InvalidState("Game already ended!".into()))?
            }
            LobbyState::NotStarted(not_started) => {
                let mut inputs = BTreeMap::new();
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
                        state: LobbyState::NotStarted(not_started.clone()),
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

        let (set_inputs_sender, set_inputs_receiver) =
            mpsc::unbounded_channel::<ChannelInputs<Q::UserLogic>>();

        let backend_state = Arc::new(std::sync::Mutex::new(DeformSharedBackendState::<
            Q::UserLogic,
        >::new_from_lobby(
            lobby.clone()
        )?));

        // cursed
        let backend_state_clone = backend_state.clone();
        let backend_state_clone_2 = backend_state.clone();
        let cancellation_token_clone = cancellation_token.clone();

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
                                cancellation_token: cancellation_token_clone,
                                set_inputs_receiver,
                                backend_state: backend_state_clone.clone(),
                                user_logic,
                                buffer_estimate: TARGET_BUFFER + Q::JITTER_SLACK,
                                rollback_panic: 0.0,

                                smoother,
                                visual_tick_micros,
                                last_sim_instant: Instant::now(),
                                last_tick_interval: Duration::from_micros(
                                    <Q::UserLogic as DeformUserLogic>::TICK_RATE_MICROS,
                                ),
                                // real value is set at the top of `tick_loop`
                                next_tick_deadline: tokio::time::Instant::now(),
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
        });

        setup_rx.blocking_recv().map_err(|_| {
            DeformError::Connection("setup thread terminated unexpectedly".into())
        })??;

        Ok(DeformClient::new(
            set_inputs_sender,
            backend_state,
            cancellation_token,
        ))
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
        let mut rtt_ticker = interval(Duration::from_millis(RTT_SAMPLE_INTERVAL_MS));

        let mut fragmentor = DatagramFragmentor::new(self.connection.clone());
        let mut defragmentor = DatagramDefragmentor::<Q>::new(self.connection.clone());

        loop {
            tokio_select!(match .. {
                // Tick every ~16ms (or more, depending on time dilation)
                .. if let _ = &mut tick_sleep => {
                    match &self.remote_lobby.state {
                        LobbyState::Finished(_) => break,
                        LobbyState::NotStarted(_) => {}
                        LobbyState::Ongoing(ongoing) => {
                            let remote_tick = ongoing.tick;
                            if self.local_tick == remote_tick {
                                // at match start the data has not been populated, so just use the RTT to estimate
                                // RTT + 3000ms + 1 tick
                                // it should quickly converge as the match starts
                                let fake_lead = (self.connection.rtt().as_micros() as u64 + 3000)
                                    .div_ceil(<Q::UserLogic as DeformUserLogic>::TICK_RATE_MICROS)
                                    + 1;

                                for _ in 0..fake_lead {
                                    self.advance_local_simulation()?;
                                    // finish is handled when server tells us, not here
                                }
                                // A burst has no interval to measure; hold the base rate.
                                self.last_tick_interval = Duration::from_micros(
                                    <Q::UserLogic as DeformUserLogic>::TICK_RATE_MICROS,
                                );
                                self.last_sim_instant = Instant::now();
                                // The estimate describes the lead we just discarded, so
                                // restart it neutral rather than below target.
                                self.buffer_estimate = TARGET_BUFFER + Q::JITTER_SLACK;
                            } else {
                                // Every smaller correction, catching up or shedding
                                // lead, is handled by time dilation (see
                                // `compute_dilated_tick_interval`).
                                if self.local_tick < remote_tick + MAX_PREDICTION_TICKS {
                                    self.advance_local_simulation()?;
                                    // finish is handled when server tells us, not here
                                    self.close_sim_interval();
                                }
                            }
                        }
                    }

                    // Commit right after the simulation step, so an input is transmitted as soon
                    // as its tick closes. Sending on an independent timer instead meant a sample
                    // could wait a whole extra period for a commit it had just missed.
                    #[cfg(feature = "metrics")]
                    if let Some(max_input) = self.inputs.keys().max() {
                        deform_metrics::plot!("commit_inputs", *max_input as f64);
                    }

                    self.commit_inputs(&mut fragmentor).await?;

                    // Advance the anchored deadline by the (variable) dilated interval rather than
                    // sleeping from `now`, so the work done in this arm does not accumulate as drift.
                    // Dilation is preserved: `compute_dilated_tick_interval` still decides the step.
                    let dilated = self.compute_dilated_tick_interval();
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
                        let t =
                            (elapsed / self.last_tick_interval.as_micros() as f32).clamp(0.0, 1.0);
                        #[cfg(feature = "metrics")]
                        deform_metrics::plot!("visual_t", t as f64);

                        let mut visual_state = current.clone();
                        self.smoother
                            .apply(&prev.game_state, &mut visual_state.game_state, t);

                        {
                            let mut shared = self
                                .backend_state
                                .lock()
                                .map_err(|_| DeformError::LockPoisoned)?;

                            if let LobbyState::Ongoing(ongoing) = &mut shared.lobby.state {
                                ongoing.tick_info = visual_state;
                            }
                        }
                    }
                }

                // tick to plot the RTT once in a while and write it into the stats
                // just so we don't do this constantly
                .. if let _ = rtt_ticker.tick() => {
                    if !matches!(self.remote_lobby.state, LobbyState::Finished(_)) {
                        let rtt = self.connection.rtt();

                        #[cfg(feature = "metrics")]
                        deform_metrics::plot!("RTT", rtt.as_secs_f64() * 1000.0);

                        let mut shared = self
                            .backend_state
                            .lock()
                            .map_err(|_| DeformError::LockPoisoned)?;
                        shared.stats.ping_ms = rtt.as_secs_f64() * 1_000.0;
                    }
                }

                // New inputs from the game engine
                .. if let new_inputs = self.set_inputs_receiver.recv() => {
                    if new_inputs.is_none() {
                        break;
                    }
                    if !matches!(self.remote_lobby.state, LobbyState::Finished(_))
                        && let Some(new_inputs) = new_inputs
                    {
                        // The engine can provide several samples within one tick. Merge them
                        // instead of keeping only the first, which silently dropped the rest.
                        // Safe to mutate in place: this entry is not sent until the tick closes.
                        match self.inputs.get_mut(&self.local_tick) {
                            // Merging keeps the existing entry's creation_time, so a tick
                            // reports its first sample: the one that waited longest.
                            #[cfg(feature = "metrics")]
                            Some(existing) => existing.inputs.merge(&new_inputs.inputs),
                            #[cfg(not(feature = "metrics"))]
                            Some(existing) => existing.merge(&new_inputs),
                            None => {
                                self.inputs.insert(self.local_tick, new_inputs);
                            }
                        }
                    }
                }

                // instead of triggering every time a datagram is received, this waits for an entire message to be collected first
                .. if let message = defragmentor.recv() => {
                    // One unusable datagram is not worth ending the match over. A
                    // connection error is critical and must exit.
                    let message = match message {
                        Ok(message) => message,
                        Err(e @ DeformError::Connection(_)) => Err(e)?,
                        Err(e) => {
                            tracing::warn!("discarding datagram: {e}");
                            continue;
                        }
                    };

                    if !matches!(self.remote_lobby.state, LobbyState::Finished(_)) {
                        let packet: StateUpdatePacket =
                            wincode::deserialize(&message).map_err(|e| {
                                DeformError::Deserialize(
                                    "error deserializing packet: ".to_string() + &e.to_string(),
                                )
                            })?;

                        let decompressed_lobby_state: LobbyState<Q::UserLogic> =
                            wincode::deserialize(&packet.lobby_state.decompress()?)
                                .map_err(|e| DeformError::Deserialize(e.to_string()))?;

                        self.rollback_panic *= PANIC_DECAY;

                        #[cfg(feature = "metrics")]
                        deform_metrics::plot!("rollback_panic", self.rollback_panic as f64);

                        // Must run first since a rollback below recomputes the tick deadline from
                        // `buffer_estimate`, so the estimate has to include this packet
                        self.process_buffer_len_update(packet.player_input_buffer_len);

                        self.process_server_update(decompressed_lobby_state, &mut tick_sleep)
                            .await?;
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

                .. if let _ = self.cancellation_token.cancelled() => {
                    break;
                }
            });
        }

        // when leaving, if the game has finished, set the final visual state to the state of the lobby
        if let LobbyState::Finished(mut finished) = self.remote_lobby.state {
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
        }

        self.connection.close(quinn::VarInt::from_u32(0), b"done");

        #[cfg(feature = "metrics")]
        deform_metrics::flush();

        Ok(())
    }

    /// Measure the interval that just closed and re-anchor `last_sim_instant`.
    /// Clamped to dilation's own range so a frozen tick can't stall interpolation.
    fn close_sim_interval(&mut self) {
        let now = Instant::now();
        let base = Duration::from_micros(<Q::UserLogic as DeformUserLogic>::TICK_RATE_MICROS);
        self.last_tick_interval = now
            .duration_since(self.last_sim_instant)
            .clamp(base / 2, base * 4);
        self.last_sim_instant = now;
    }

    /// Updates values according to a newly received buffered_inputs_len value
    ///
    /// - buffer_estimate: pessimistic EWMA (fast down, slow up)
    fn process_buffer_len_update(&mut self, buffered: u8) {
        let buffered = buffered as f32;
        let alpha = if buffered < self.buffer_estimate {
            BUFFER_FALL
        } else {
            BUFFER_RISE
        };
        self.buffer_estimate += alpha * (buffered - self.buffer_estimate);

        #[cfg(feature = "metrics")]
        {
            deform_metrics::plot!("input_buffer", buffered as f64);
            deform_metrics::plot!("input_buffer_est", self.buffer_estimate as f64);
        }
    }

    /// Time dilation steered by how many of our inputs the server has queued up.
    ///
    /// Time speeds up (shorter ticks) when we are behind our target, or when self-rollbacks happen.
    ///
    /// Time slows down (longer ticks) when we are ahead of our target.
    fn compute_dilated_tick_interval(&self) -> Duration {
        #[cfg(feature = "metrics")]
        let _span = deform_metrics::span!("compute_dilated_tick_interval");
        let base_micros = <Q::UserLogic as DeformUserLogic>::TICK_RATE_MICROS as f32;

        let target = TARGET_BUFFER + Q::JITTER_SLACK + self.rollback_panic;

        let behind = (target - self.buffer_estimate).max(0.0);
        let ahead = (self.buffer_estimate - target - SLOWDOWN_DEADZONE).max(0.0);

        let rate = 1.0 + Q::TIME_DILATION * (behind / SPEEDUP_SOFTNESS).tanh()
            - Q::TIME_DILATION * SLOWDOWN_RATIO * (ahead / SLOWDOWN_SOFTNESS).tanh();

        let micros = base_micros / rate;

        #[cfg(feature = "metrics")]
        {
            deform_metrics::plot!("input_buffer_target", target as f64);
            deform_metrics::plot!("sleep_time", micros as f64 / 1000.0);
        }

        Duration::from_micros(micros as u64)
    }

    pub fn advance_local_simulation(&mut self) -> UserFacingResult<Q::UserLogic> {
        #[cfg(feature = "metrics")]
        let _span = deform_metrics::span!("advance_local_simulation");

        // inneficient but only used in dev
        #[cfg(feature = "metrics")]
        {
            let remote_tick = match &self.remote_lobby.state {
                LobbyState::Ongoing(ongoing) => ongoing.tick,
                LobbyState::Finished(LobbyFinished(finished)) => finished.tick,
                LobbyState::NotStarted(_) => 0,
            };

            deform_metrics::plot!(
                "current_vs_remote_adv",
                self.local_tick as f64 - remote_tick as f64
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
        let mut new_players_inputs: BTreeMap<Pubkey, <Q::UserLogic as DeformUserLogic>::Inputs> =
            current_info.inputs.clone();

        for (player, inputs) in new_players_inputs.iter_mut() {
            *inputs = if *player == self.player {
                // for our own player: try to get from the map. else, predict from the
                // previous value and remember it below
                if let Some(provided_inputs) = self.inputs.get(&current_tick) {
                    #[cfg(feature = "metrics")]
                    deform_metrics::plot!(
                        "local_input_delay",
                        provided_inputs.creation_time.elapsed().as_micros() as f64
                    );
                    #[cfg(feature = "metrics")]
                    let provided_inputs = &provided_inputs.inputs;
                    provided_inputs.clone()
                } else {
                    let predicted = inputs.predict();

                    // when predicting inputs, add them to the inputs buffer so we don't starve the server
                    #[cfg(feature = "metrics")]
                    self.inputs.insert(
                        current_tick,
                        deform_core::StampedInputs {
                            inputs: predicted.clone(),
                            creation_time: Instant::now(),
                        },
                    );
                    #[cfg(not(feature = "metrics"))]
                    self.inputs.insert(current_tick, predicted.clone());

                    predicted
                }
            } else {
                inputs.predict()
            }
        }

        #[cfg(feature = "metrics")]
        deform_metrics::plot!("advance_sim", new_tick as f64);

        let new_state = {
            #[cfg(feature = "metrics")]
            let _span = deform_metrics::span!("sim_compute");
            self.user_logic
                .advance_frame(&current_info.game_state, &new_players_inputs)
        }
        .map_err(|e| UserFacingError::User(e))?;

        let next_info = TickInfo {
            game_state: new_state,
            inputs: new_players_inputs,
        };

        self.local_tick = new_tick;
        self.info_per_tick.insert(new_tick, next_info);

        // Attribute every subsequent record to the tick it happened on. Rollbacks and
        // fast-forwards re-enter here, so this alone keeps the attribution honest.
        #[cfg(feature = "metrics")]
        deform_metrics::set_tick(new_tick);

        Ok(())
    }

    pub async fn commit_inputs(&mut self, fragmentor: &mut DatagramFragmentor<Q>) -> DeformResult {
        #[cfg(feature = "metrics")]
        let _span = deform_metrics::span!("commit_inputs");

        // Never send the tick still in progress: samples are still being merged into it.
        // The server commits first-write-wins, so a non-final value would be locked in
        // there while we keep changing ours, guaranteeing a mismatch and a rollback.
        // Normally this filters nothing, since the caller has just advanced past that
        // tick, but the simulation does not advance while frozen at `MAX_PREDICTION_TICKS`.
        // NOTE: this may add 1 tick of delay to committing inputs!!!
        // TODO: this is important in FOC mode as users could change their inputs last second to gain an advantage.
        // However, in QUIC mode, we could allow the server to overwrite/merge inputs instead of having first-write-wins,
        // which would take care of this issue.
        let pending: HashMap<u64, <Q::UserLogic as DeformUserLogic>::Inputs> = self
            .inputs
            .iter()
            .filter(|(tick, _)| **tick < self.local_tick)
            .map(|(tick, inputs)| {
                #[cfg(feature = "metrics")]
                let inputs = &inputs.inputs;
                (*tick, inputs.clone())
            })
            .collect();

        // WARN: sending message when nothing is there will do absolutely nothing
        // our goal is to never enter this branch
        if pending.is_empty() {
            return Ok(());
        }

        #[cfg(feature = "metrics")]
        deform_metrics::plot!("commit_batch_ticks", pending.len() as f64);

        #[cfg(feature = "metrics")]
        let newest = pending.keys().max().copied();

        let ix =
            ServerInstruction::<<Q::UserLogic as DeformUserLogic>::Inputs>::BatchSetInputs(pending);
        let message = Compressed::compress(&wincode::serialize(&ix)?, Q::COMPRESSION)?;
        fragmentor.send(&message.0)?;

        #[cfg(feature = "metrics")]
        if let Some(newest) = newest
            && let Some(entry) = self.inputs.get(&newest)
        {
            deform_metrics::plot!(
                "input_to_commit",
                entry.creation_time.elapsed().as_micros() as f64
            );
        }

        Ok(())
    }

    pub async fn process_server_update(
        &mut self,
        new_remote_state: LobbyState<Q::UserLogic>,
        tick_sleep: &mut Pin<Box<Sleep>>,
    ) -> UserFacingResult<Q::UserLogic> {
        #[cfg(feature = "metrics")]
        let _span = deform_metrics::span!("process_server_update");

        // inneficient since the variant is checked below, but this is only used in dev
        #[cfg(feature = "metrics")]
        {
            let new_remote = match &new_remote_state {
                LobbyState::Finished(LobbyFinished(finished_state)) => finished_state.tick,
                LobbyState::NotStarted(_) => 0,
                LobbyState::Ongoing(ongoing) => ongoing.tick,
            };

            deform_metrics::plot!(
                "current_vs_remote_reception",
                self.local_tick as f64 - new_remote as f64
            );
        }

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
            LobbyState::Ongoing(ongoing) => {
                if matches!(self.remote_lobby.state, LobbyState::NotStarted(_)) {
                    // first authoritative state of the match, populate local copy
                    let mut shared = self
                        .backend_state
                        .lock()
                        .map_err(|_| DeformError::LockPoisoned)?;
                    shared.lobby.state = LobbyState::Ongoing(ongoing.clone());
                }

                self.handle_new_ongoing(ongoing, tick_sleep)
            }
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

        #[cfg(feature = "metrics")]
        let discarded_before = self.smoother.corrections_discarded();

        self.smoother.on_rollback(
            &pre_rollback_info.game_state,
            &post_rollback_info.game_state,
        );

        #[cfg(feature = "metrics")]
        deform_metrics::event!(
            "rollback",
            to_tick = conflicting_tick,
            depth = previous_local_tick.saturating_sub(conflicting_tick),
            magnitude = self.smoother.correction_magnitude_sq().sqrt(),
            corrections_discarded = self.smoother.corrections_discarded() - discarded_before,
        );

        self.user_logic
            .on_rollback(pre_rollback_info, post_rollback_info)
            .map_err(|e| UserFacingError::User(e))?;

        let dilated = self.compute_dilated_tick_interval();
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
            LobbyState::NotStarted(_) => 0,
            _ => Err(DeformError::InvalidState(
                "Previous lobby was not Ongoing".into(),
            ))?,
        };
        let new_tick_info = remote_ongoing.tick_info.clone();

        #[cfg(feature = "metrics")]
        deform_metrics::plot!("last_tick_slot", new_remote_tick as f64);

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
            Rollback { self_mismatch: bool },
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
            // a mismatch on our own player is singled out: it means our inputs reached the
            // server after it needed them, which is what the pacing has to react to.
            let mut mismatch = false;
            let mut self_mismatch = false;
            for (player, predicted_input) in predicted_inputs.iter() {
                let remote_input = remote_inputs.get(player).ok_or(DeformError::InvalidState(
                    "player not found in remote inputs".into(),
                ))?;

                if remote_input != predicted_input {
                    mismatch = true;
                    if *player == self.player {
                        self_mismatch = true;
                        break;
                    }
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
                ReceivedScenario::Rollback { self_mismatch }
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

                #[cfg(feature = "metrics")]
                deform_metrics::event!(
                    "fast_forward",
                    from_tick = self.local_tick,
                    to_tick = new_remote_tick,
                    jump = new_remote_tick.saturating_sub(self.local_tick),
                );

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
        //     #[cfg(feature = "metrics")]
        //     deform_metrics::plot!("stale datagrams", self.stale_datagrams as f64);
        //     return Ok(());
        // }

        // // Count gaps in received tick sequence as dropped datagrams
        // let gap = new_lobby_state
        //     .tick
        //     .saturating_sub(self.remote_tick + 1);
        // if gap > 0 {
        //     self.dropped_datagrams += gap;
        //     #[cfg(feature = "metrics")]
        //     deform_metrics::plot!("dropped datagrams", self.dropped_datagrams as f64);
        // }

        // prune all local inputs that are older than the new remote tick
        self.inputs.retain(|tick, _| *tick >= new_remote_tick);

        // #[cfg(feature = "log")]
        // tracing::trace!("QUIC received tick {}", new_remote_tick);

        #[cfg(feature = "metrics")]
        deform_metrics::plot!("remote_tick (clean)", new_remote_tick as f64);

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

                #[cfg(feature = "metrics")]
                deform_metrics::event!(
                    "gap",
                    from_tick = old_remote_tick,
                    to_tick = new_remote_tick,
                    missed = new_remote_tick.saturating_sub(old_remote_tick + 1),
                );

                self.user_logic
                    .on_gap(old_remote_state, &new_tick_info)
                    .map_err(|e| UserFacingError::User(e))?;

                // a rollback is always triggered, as it is assumed that the simulation is now out of sync
                // so it is resimulated from the new tick up to the current tick
                // this also inserts the new state etc
                self.handle_rollback(new_tick_info, new_remote_tick, tick_sleep)?;
            }
            ReceivedScenario::Rollback { self_mismatch } => {
                if self_mismatch {
                    self.rollback_panic = (self.rollback_panic + ROLLBACK_KICK).min(PANIC_MAX);

                    #[cfg(feature = "metrics")]
                    deform_metrics::event!("self_rollback", panic = self.rollback_panic);
                }

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
