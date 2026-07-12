use std::{
    collections::{HashMap, HashSet},
    net::{IpAddr, SocketAddr},
    sync::Arc,
    time::Duration,
};

use anyhow::Context;
use better_tokio_select::tokio_select;
use deform_core::{
    DeformError::{self, InvalidState},
    Pubkey,
    accounts::{
        DeformAccount,
        lobby::{Lobby, LobbyMetadata, LobbyState, PlayerStatus, not_started::LobbyNotStarted},
    },
    error::{UserFacingError, UserFacingResult},
    game_program_client::GameProgramClient,
};
use quinn::{Connection, RecvStream, SendStream, ServerConfig, crypto::rustls::QuicServerConfig};
use solana_rpc_client::nonblocking::rpc_client::RpcClient;
use solana_sdk::signature::Keypair;
use tokio::{
    signal::unix::{SignalKind, signal},
    sync::{RwLock, broadcast, mpsc},
    time::sleep,
};
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

use crate::{
    DeformQuicLogic, ReliableMessage,
    server::{
        auth_config::{AuthConfig, build_tls_config},
        matches::{InternalServerResponse, Match, MatchConfig, MatchInfo, MatchMessage},
    },
};

pub mod auth_config;
pub mod matches;
pub mod user;

pub struct DeformQuicServer<Q: DeformQuicLogic> {
    /// NOTE: you can use [`build_tls_config()`] as a helper:
    /// ```ignore
    /// let tls_config = build_tls_config(auth_config)?;
    /// let quic_server_config =
    ///     ServerConfig::with_crypto(Arc::new(QuicServerConfig::try_from(tls_config)?));
    /// ```
    pub quinn_config: ServerConfig,
    pub addr: SocketAddr,
    pub max_conn_per_ip: u64,

    pub matches: Arc<RwLock<HashMap<u64, MatchInfo<Q>>>>,
    pub num_connections_per_ip: Arc<RwLock<HashMap<IpAddr, u64>>>,
    pub match_config: MatchConfig,

    pub rpc_client: Arc<RpcClient>,
    pub admin_keypair: Arc<Keypair>,
    pub user_server_logic: Arc<Q>,
    pub game_program_client: Arc<Q::ProgramClient>,
}

impl<Q: DeformQuicLogic> DeformQuicServer<Q> {
    pub fn new_with_defaults(
        auth_config: &AuthConfig,
        rpc_client: Arc<RpcClient>,
        admin_keypair: Arc<Keypair>,
        user_server_logic: Q,
        game_program_client: Q::ProgramClient,
    ) -> anyhow::Result<Self> {
        let tls_config = build_tls_config(auth_config)?;
        let quic_server_config =
            ServerConfig::with_crypto(Arc::new(QuicServerConfig::try_from(tls_config)?));

        let mut config = Self {
            quinn_config: quic_server_config,
            addr: "0.0.0.0:443".parse()?,
            max_conn_per_ip: 5,
            matches: Arc::new(RwLock::new(HashMap::new())),
            num_connections_per_ip: Arc::new(RwLock::new(HashMap::new())),
            match_config: MatchConfig::WaitForTimeout(Duration::from_secs(10)),
            rpc_client,
            admin_keypair,
            user_server_logic: Arc::new(user_server_logic),
            game_program_client: Arc::new(game_program_client),
        };

        config.apply_custom_quinn_defaults();

        Ok(config)
    }

    pub fn apply_custom_quinn_defaults(&mut self) {
        // Bound the queue of unaccepted connections (backpressure against floods).
        // Retry token validation + per-IP caps are enforced in the accept loop.
        self.quinn_config.max_incoming(1024);

        let mut transport = quinn::TransportConfig::default();
        transport.keep_alive_interval(Some(Duration::from_secs(20)));
        transport.datagram_receive_buffer_size(Some(16 * 1024));
        self.quinn_config.transport_config(Arc::new(transport));
    }

    pub async fn init_server(self) -> anyhow::Result<()> {
        // // TODO: get this out of here, the client has to call it
        // rustls::crypto::ring::default_provider()
        //     .install_default()
        //     .expect("Failed to install rustls crypto provider");
        let endpoint = quinn::Endpoint::server(self.quinn_config.clone(), self.addr.clone())?;

        // I will have many different tokio selects doing different things, so shutdown will be through a cancellation token
        // TODO: pass this from outside so the user can also call it
        let cancellation_token = CancellationToken::new();
        Self::register_signal(cancellation_token.clone()).await;

        let shared_server = Arc::new(self);

        loop {
            tokio_select!(match .. {
                .. if let incoming = endpoint.accept() => {
                    match incoming {
                        Some(incoming) => {
                            Self::handle_incoming(shared_server.clone(), incoming).await;
                        }
                        None => {
                            info!("QUIC endpoint closed");
                            break;
                        }
                    }
                }
                .. if let _ = cancellation_token.cancelled() => {
                    // TODO: in the future don't do this with a sleep

                    // wait for all cranks to finish their games
                    loop {
                        let matches_len = shared_server.matches.read().await.len();
                        if matches_len > 0 {
                            info!("Waiting for {} active match(es) to finish...", matches_len);
                            sleep(Duration::from_secs(1)).await;
                        } else {
                            break;
                        }
                    }

                    break;
                }
            })
        }

        Ok(())
    }

    async fn register_signal(cancellation_token: CancellationToken) {
        tokio::spawn(async move {
            let mut sigterm = signal(SignalKind::terminate())
                .with_context(|| "Failed to register SIGTERM handler")
                .unwrap();

            // wait for ctrl_c or sigterm to be received
            tokio_select!(match .. {
                .. if let result = tokio::signal::ctrl_c() => {
                    if result.is_err() {
                        error!("Failed to listen for ctrl_c signal");
                        return;
                    }
                }
                .. if let _ = sigterm.recv() => {}
            });

            info!(
                "Shutdown signal received. Rejecting new matches, waiting for active ones to finish..."
            );
            cancellation_token.cancel();
        });
    }

    /// Does a quick filtering of the connection before spawning a task to process it
    pub async fn handle_incoming(server: Arc<Self>, incoming: quinn::Incoming) {
        // do some quick filtering to check that the connection is allowed before spawning the handler task
        let client_ip = incoming.remote_address().ip();
        let is_loopback = client_ip.is_loopback();

        // TODO: is this needed / is this how this should be done?
        // Force address validation: if the client hasn't echoed
        // a Retry token yet, send one and discard this attempt.
        // This makes spoofed-source-IP floods essentially free
        // to defend against. Loopback is exempt: it cannot be
        // spoofed and the extra round-trip would break local
        // dev/test workflows.
        if !is_loopback && !incoming.remote_address_validated() {
            if let Err(e) = incoming.retry() {
                warn!("Failed to send QUIC Retry: {}", e);
            }
            return;
        }

        // refuse connection if too many connections
        // it is checked here, but not modified!! only incremented once connection is actually accepted
        if !is_loopback {
            let num_connections_per_ip_guard = server.num_connections_per_ip.read().await;
            if let Some(num_connections) = num_connections_per_ip_guard.get(&client_ip) {
                if *num_connections >= server.max_conn_per_ip {
                    warn!(
                        "Refusing connection from {}: too many connections",
                        client_ip
                    );
                    incoming.refuse();
                    return;
                }
            }
        }

        let matches = server.matches.clone();
        let num_connections_per_ip = server.num_connections_per_ip.clone();
        let server = server.clone();

        tokio::spawn(async move {
            let connection = match incoming.await {
                Ok(connection) => connection,
                Err(e) => {
                    debug!("Incoming connection failed from {}: {}", client_ip, e);
                    return;
                }
            };

            let (mut send_stream, mut recv_stream) = match connection.accept_bi().await {
                Ok(streams) => streams,
                Err(e) => {
                    let msg = e.to_string();
                    if msg.contains("cryptographic handshake failed") {
                        debug!("Rejected probe from {}: {}", client_ip, msg);
                    } else {
                        warn!("Failed to accept bi-stream from {}: {}", client_ip, msg);
                    }
                    return;
                }
            };

            if let Err(e) = Self::process_connection(
                server,
                connection.clone(),
                client_ip.clone(),
                is_loopback,
                matches,
                num_connections_per_ip.clone(),
                &mut send_stream,
                &mut recv_stream,
            )
            .await
            {
                warn!("Sending error to client {}: {}", client_ip, e);
                let _ = ReliableMessage::<Q>::Error(e).write(&mut send_stream).await;
                let _ = send_stream.finish();
                connection.close(quinn::VarInt::from_u32(1), b"error");
            }

            // Decrement per-IP count when the connection ends.
            // Loopback connections were never counted, skip them.
            if !is_loopback {
                let mut num_connections_per_ip_guard = num_connections_per_ip.write().await;

                if let Some(count) = num_connections_per_ip_guard.get_mut(&client_ip) {
                    if *count > 0 {
                        *count -= 1;
                    }
                    if *count == 0 {
                        num_connections_per_ip_guard.remove(&client_ip);
                    }
                }
            }
        });
    }

    /// Checks auth and join a match, handling the player handling loop to client_loop()
    pub async fn process_connection(
        server: Arc<Self>,
        connection: Connection,
        client_ip: IpAddr,
        is_loopback: bool,
        matches: Arc<RwLock<HashMap<u64, MatchInfo<Q>>>>,
        num_connections_per_ip: Arc<RwLock<HashMap<IpAddr, u64>>>,
        send_stream: &mut SendStream,
        recv_stream: &mut RecvStream,
    ) -> UserFacingResult<Q::UserLogic> {
        if !is_loopback {
            let mut num_connections_per_ip_guard = num_connections_per_ip.write().await;
            let entry = num_connections_per_ip_guard.entry(client_ip).or_insert(0);
            *entry += 1;
        }

        let identification = match ReliableMessage::<Q>::read(recv_stream).await? {
            ReliableMessage::Identification(identification) => identification,
            _ => Err(DeformError::Auth(
                "Expected an auth message as the first message".into(),
            ))?,
        };

        Q::authorize_connection(&identification).map_err(|e| UserFacingError::User(e))?;

        // if an existing match does exist, a read lock would be enough
        // however, if it does not exist, we need an atomic way to insert things into the match while guaranteeing that the same match is not created in parallel
        // so the workaround is to keep this write lock, which gets released as soon as possible
        let mut matches_guard = matches.write().await;

        // check if lobby already has an existing match
        if let Some(existing_match) = matches_guard.get(&identification.lobby_id).cloned() {
            // the matches_guard exists on the outside so it can be used in the `else`
            // we can drop it now, it is no longer needed
            drop(matches_guard);

            let started_match = match existing_match {
                MatchInfo::Initializing(init_notify) => {
                    let init_notify_clone = init_notify.clone();
                    init_notify_clone.cancelled().await;

                    if let Some(MatchInfo::Started(started)) =
                        matches.read().await.get(&identification.lobby_id).cloned()
                    {
                        Ok(started)
                    } else {
                        Err(UserFacingError::Deform(DeformError::InvalidState(
                            "The given match has not started or does not exist".into(),
                        )))
                    }
                }
                MatchInfo::Started(started_match) => Ok(started_match),
            }?;

            // check that user belongs in the match
            if !started_match
                .expected_players
                .contains(&identification.user)
            {
                Err(UserFacingError::Deform(DeformError::Auth(
                    "User does not belong in this match!".into(),
                )))?;
            } else {
                // only now can we send Authorized
                let _ = ReliableMessage::<Q>::Authorized.write(send_stream).await?;
            }

            if started_match
                .game_ended
                .load(std::sync::atomic::Ordering::SeqCst)
            {
                Err(UserFacingError::Deform(DeformError::InvalidState(
                    "Match has already ended!".into(),
                )))?;
            }

            let match_sender = started_match.match_sender.clone();
            let state_receiver = started_match.state_sender.subscribe();

            Self::client_loop(
                identification.user,
                match_sender,
                connection,
                send_stream,
                state_receiver,
            )
            .await?;
        } else {
            let match_started_token = CancellationToken::new();
            matches_guard.insert(
                identification.lobby_id,
                MatchInfo::Initializing(match_started_token.clone()),
            );
            drop(matches_guard);

            // WARN: ---------------------------------------------------------------
            // between these two points, any error must remove the match and call the cancellation token
            // TODO: this solution is messy but a function could be worse, what to do?
            let init_result: UserFacingResult<Q::UserLogic, _> = async {
                let (lobby_metadata, not_started) = Self::check_lobby(
                    &server.rpc_client,
                    identification.lobby_id,
                    &server.game_program_client.game_program(),
                    10,
                )
                .await?;

                ReliableMessage::<Q>::Authorized.write(send_stream).await?;

                let (state_sender, state_receiver) =
                    broadcast::channel::<InternalServerResponse<Q>>(64);
                let (match_sender, match_receiver) =
                    mpsc::channel::<MatchMessage<Q::UserLogic>>(256);

                let mut expected_players = HashSet::new();
                for player in not_started.player_status.keys() {
                    expected_players.insert(*player);
                }

                let match_info = MatchInfo::Started(Match {
                    state_sender: state_sender.clone(),
                    match_sender: match_sender.clone(),
                    game_ended: Arc::new(std::sync::atomic::AtomicBool::new(false)),
                    expected_players: Arc::new(expected_players),
                });

                Ok((
                    lobby_metadata,
                    not_started,
                    match_info,
                    state_sender,
                    state_receiver,
                    match_sender,
                    match_receiver,
                ))
            }
            .await;

            let (
                lobby_metadata,
                not_started,
                match_info,
                state_sender,
                state_receiver,
                match_sender,
                match_receiver,
            ) = match init_result {
                Ok(val) => val,
                Err(e) => {
                    match_started_token.cancel();
                    matches.write().await.remove(&identification.lobby_id);
                    return Err(e);
                }
            };
            // WARN: see above ------------------------------------------------------

            let lobby_id = identification.lobby_id;

            matches.write().await.insert(lobby_id, match_info);

            match_started_token.cancel();

            let error_sender = state_sender.clone();
            tokio::spawn(async move {
                if let Err(e) = matches::match_loop(
                    server,
                    lobby_metadata,
                    not_started,
                    state_sender,
                    match_receiver,
                )
                .await
                {
                    error!(lobby_id, "Match ended with error: {e}");
                    let _ = error_sender.send(InternalServerResponse::SendReliableMessage(
                        ReliableMessage::Error(e),
                    ));
                }
            });

            Self::client_loop(
                identification.user,
                match_sender,
                connection,
                send_stream,
                state_receiver,
            )
            .await?;
        }

        Ok(())
    }

    /// Checks that lobby meets all preconditions, but does NOT check the player connecting
    // TODO: use min fetch slot here
    pub async fn check_lobby(
        rpc_client: &RpcClient,
        lobby_id: u64,
        game_program: &Pubkey,
        max_attempts: usize,
    ) -> UserFacingResult<Q::UserLogic, (LobbyMetadata, LobbyNotStarted)> {
        let (lobby_pda, _) = Lobby::<Q::UserLogic>::find_program_address(lobby_id, game_program);

        for attempt in 1..=max_attempts {
            info!(
                "Attempting to fetch lobby {} (attempt {}/{})",
                lobby_id, attempt, max_attempts
            );

            match rpc_client.get_account_data(&lobby_pda).await {
                Ok(lobby_bytes) => {
                    let account = DeformAccount::<Q::UserLogic>::from_bytes(&lobby_bytes)?;

                    let DeformAccount::Lobby(lobby) = account else {
                        return Err(UserFacingError::Deform(InvalidState(
                            "Account is not a Lobby".into(),
                        )));
                    };

                    let LobbyState::NotStarted(not_started) = lobby.state else {
                        warn!(
                            "Preconditions not met for lobby {}, retrying... (attempt {}/{})",
                            lobby_id, attempt, max_attempts
                        );
                        sleep(Duration::from_millis(400)).await;
                        continue;
                    };

                    let all_ready = not_started
                        .player_status
                        .values()
                        .copied()
                        .all(|player_info| player_info == PlayerStatus::Ready);

                    if !all_ready {
                        warn!(
                            "Preconditions not met for lobby {}, retrying... (attempt {}/{})",
                            lobby_id, attempt, max_attempts
                        );
                        sleep(Duration::from_millis(400)).await;
                        continue;
                    };

                    // TODO: allow custom checks from the user

                    return Ok((lobby.metadata, not_started));
                }
                Err(e) => {
                    warn!(
                        "Failed to fetch lobby {}: {} (attempt {}/{})",
                        lobby_id, e, attempt, max_attempts
                    );
                    sleep(Duration::from_millis(400)).await;
                }
            }
        }

        Err(UserFacingError::Deform(DeformError::InvalidState(
            "Lobby is not in a valid state!".into(),
        )))
    }
}
