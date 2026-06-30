use std::{
    collections::{HashMap, HashSet},
    net::{IpAddr, SocketAddr},
    sync::Arc,
    time::Duration,
};

use anyhow::Context;
use better_tokio_select::tokio_select;
use deform_core::{
    DeformError, DeformUserLogic, Pubkey,
    accounts::lobby::{Lobby, LobbyStatus, PLayerStatus},
    error::{UserFacingError, UserFacingResult},
};
use quinn::{Connection, RecvStream, SendStream, ServerConfig, crypto::rustls::QuicServerConfig};
use solana_rpc_client::nonblocking::rpc_client::RpcClient;
use tokio::{
    signal::unix::{SignalKind, signal},
    sync::{RwLock, broadcast, mpsc},
    time::sleep,
};
use tokio_util::sync::CancellationToken;

use crate::{
    DeformQuicLogic, ReliableMessage,
    server::{
        auth_config::{AuthConfig, build_tls_config},
        matches::{InternalServerResponse, Match, MatchConfig, MatchInfo, MatchMessage},
    },
};

mod auth_config;
mod matches;

// TODO: how to let the client have full custom behaviour?? hooks?
// TODO: tracing and logs
// TODO: return errors that are not anyhow

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

    pub user_server_logic: Arc<Q>,
}

impl<Q: DeformQuicLogic> DeformQuicServer<Q> {
    pub fn new_with_defaults(
        auth_config: &AuthConfig,
        user_server_logic: Q,
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
            user_server_logic: Arc::new(user_server_logic),
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

    pub async fn init_server(self, rpc_client: Arc<RpcClient>) -> anyhow::Result<()> {
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
                            Self::handle_incoming(
                                shared_server.clone(),
                                incoming,
                                rpc_client.clone(),
                            )
                            .await;
                        }
                        None => {
                            // info!("QUIC endpoint closed");
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
                            // info!("Waiting for {} active crank(s) to finish...", matches_len);
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
            // TODO: make this return error somehow
            let mut sigterm = signal(SignalKind::terminate())
                .with_context(|| "Failed to register SIGTERM handler")
                .unwrap();

            // wait for ctrl_c or sigterm to be received
            tokio_select!(match .. {
                .. if let result = tokio::signal::ctrl_c() => {
                    if result.is_err() {
                        // error!("Failed to listen for ctrl_c signal");
                        return;
                    }
                }
                .. if let _ = sigterm.recv() => {}
            });

            // set an atomic bool so that new connections can be rejected
            // info!(
            //     "Shutdown signal received. Rejecting new cranks, waiting for active ones to finish..."
            // );
            cancellation_token.cancel();
        });
    }

    /// Does a quick filtering of the connection before spawning a task to process it
    pub async fn handle_incoming(
        server: Arc<Self>,
        incoming: quinn::Incoming,
        rpc_client: Arc<RpcClient>,
    ) {
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
            if let Err(_e) = incoming.retry() {
                // warn!("Failed to send QUIC Retry: {}", e);
            }
            return;
        }

        // refuse connection if too many connections
        // it is checked here, but not modified!! only incremented once connection is actually accepted
        if !is_loopback {
            let num_connections_per_ip_guard = server.num_connections_per_ip.read().await;
            if let Some(num_connections) = num_connections_per_ip_guard.get(&client_ip) {
                if *num_connections >= server.max_conn_per_ip {
                    // log...
                    incoming.refuse();
                    return;
                }
            }
        }

        let rpc_client = rpc_client.clone();
        let matches = server.matches.clone();
        let num_connections_per_ip = server.num_connections_per_ip.clone();
        let server = server.clone();

        tokio::spawn(async move {
            let connection = match incoming.await {
                Ok(connection) => connection,
                Err(_e) => {
                    // TODO: handle error. maybe just debug log it and close the connection?
                    return;
                }
            };

            let (mut send_stream, mut recv_stream) = match connection.accept_bi().await {
                Ok(streams) => streams,
                Err(e) => {
                    let msg = e.to_string();
                    if msg.contains("cryptographic handshake failed") {
                        // debug!("Rejected probe from {}: {}", client_ip, msg);
                    } else {
                        //
                    }
                    return;
                }
            };

            if let Err(e) = Self::process_connection(
                server,
                connection.clone(),
                client_ip.clone(),
                is_loopback,
                rpc_client,
                matches,
                num_connections_per_ip.clone(),
                &mut send_stream,
                &mut recv_stream,
            )
            .await
            {
                // warn!("Sending error to client {}: {}", remote, e);
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
        rpc_client: Arc<RpcClient>,
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

            if started_match.game_ended {
                Err(UserFacingError::Deform(DeformError::InvalidState(
                    "Match has already ended!".into(),
                )))?;
            }

            let match_sender = started_match.match_sender.clone();
            let state_receiver = started_match.state_sender.subscribe();
            let state_sender = started_match.state_sender.clone();

            Self::client_loop().await?;
        } else {
            let match_started_token = CancellationToken::new();
            matches_guard.insert(
                identification.lobby_id,
                MatchInfo::Initializing(match_started_token.clone()),
            );
            drop(matches_guard);

            let lobby_state = Self::check_lobby(
                &rpc_client,
                identification.lobby_id,
                &<Q::UserLogic as DeformUserLogic>::game_program(),
                10,
            )
            .await?;

            let _ = ReliableMessage::<Q>::Authorized.write(send_stream).await?;

            let (state_sender, state_receiver) =
                broadcast::channel::<InternalServerResponse<Q>>(64);
            let (match_sender, match_receiver) = mpsc::channel::<MatchMessage<Q::UserLogic>>(256);
            let release_notify = Arc::new(tokio::sync::Notify::new());

            let mut expected_players = HashSet::new();
            for player in lobby_state.player_infos.keys() {
                expected_players.insert(*player);
            }

            let match_info = MatchInfo::Started(Match {
                state_sender: state_sender.clone(),
                match_sender: match_sender.clone(),
                game_ended: false,
                release_notify: release_notify.clone(),
                expected_players,
            });

            matches
                .write()
                .await
                .insert(identification.lobby_id, match_info);

            match_started_token.cancel();

            tokio::spawn(matches::match_loop(
                server,
                lobby_state,
                state_sender,
                release_notify,
                match_receiver,
            ));

            Self::client_loop().await?;
        }

        Ok(())
    }

    async fn client_loop() -> UserFacingResult<Q::UserLogic> {
        // FIX:
        Ok(())
    }

    /// Checks that lobby meets all preconditions, but does NOT check the player connecting
    // TODO: use min fetch slot here
    pub async fn check_lobby(
        rpc_client: &RpcClient,
        lobby_id: u64,
        game_program: &Pubkey,
        max_attempts: usize,
    ) -> UserFacingResult<Q::UserLogic, Lobby<Q::UserLogic>> {
        let (lobby_pda, _) = Lobby::<Q::UserLogic>::find_program_address(lobby_id, game_program);

        for _attempt in 1..=max_attempts {
            // info!("Attempting to fetch lobby {}", connection_info.lobby_id);

            match rpc_client.get_account_data(&lobby_pda).await {
                Ok(lobby_bytes) => {
                    let info = Lobby::<Q::UserLogic>::from_bytes(&lobby_bytes)?;

                    let all_ready = info
                        .player_infos
                        .values()
                        .all(|player_info| player_info.status == PLayerStatus::Ready);

                    if !(info.status == LobbyStatus::NotStarted && all_ready) {
                        // warn!(
                        //     "Preconditions not met for lobby {}, retrying... (attempt {}/{})",
                        //     connection_info.lobby_id, attempt, max_attempts
                        // );
                        sleep(Duration::from_millis(400)).await;
                        continue;
                    }

                    // TODO: allow custom checks from the user

                    return Ok(info);
                }
                Err(_e) => {
                    // warn!("Failed to fetch lobby {}: {}", connection_info.lobby_id, e);
                    sleep(Duration::from_millis(400)).await;
                }
            }
        }

        // anyhow::bail!(
        //     "Lobby not meeting preconditions after {} attempts",
        //     max_attempts
        // )

        Err(UserFacingError::Deform(DeformError::InvalidState(
            "Lobby is not in a valid state!".into(),
        )))
    }
}
