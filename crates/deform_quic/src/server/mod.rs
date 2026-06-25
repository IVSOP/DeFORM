use std::{
    collections::HashMap, marker::PhantomData, net::{IpAddr, SocketAddr}, sync::Arc, time::Duration
};

use anyhow::Context;
use better_tokio_select::tokio_select;
use deform_core::DeformUserLogic;
use quinn::{Connection, ServerConfig, crypto::rustls::QuicServerConfig};
use solana_rpc_client::nonblocking::rpc_client::RpcClient;
use tokio::{
    signal::unix::{SignalKind, signal},
    sync::RwLock,
    time::sleep,
};
use tokio_util::sync::CancellationToken;

use crate::{
    DeformQuicLogic,
    server::{
        auth_config::{AuthConfig, build_tls_config},
        matches::MatchInfo,
    },
};

mod auth_config;
mod matches;

// TODO: how to let the client have full custom behaviour?? hooks?
// TODO: tracing and logs
// TODO: return errors that are not anyhow

pub struct DeformQuicServer<T: DeformQuicLogic + DeformUserLogic> {
    /// NOTE: you can use [`build_tls_config()`] as a helper:
    /// ```ignore
    /// let tls_config = build_tls_config(auth_config)?;
    /// let quic_server_config =
    ///     ServerConfig::with_crypto(Arc::new(QuicServerConfig::try_from(tls_config)?));
    /// ```
    pub quinn_config: ServerConfig,
    pub addr: SocketAddr,
    pub max_conn_per_ip: u64,

    pub matches: Arc<RwLock<HashMap<u64, MatchInfo<T>>>>,
    pub num_connections_per_ip: Arc<RwLock<HashMap<IpAddr, u64>>>,
}

impl <T: DeformQuicLogic + DeformUserLogic> DeformQuicServer<T> {
    pub fn new_with_defaults(auth_config: &AuthConfig) -> anyhow::Result<Self> {
        let tls_config = build_tls_config(auth_config)?;
        let quic_server_config =
            ServerConfig::with_crypto(Arc::new(QuicServerConfig::try_from(tls_config)?));

        let mut config = Self {
            quinn_config: quic_server_config,
            addr: "0.0.0.0:443".parse()?,
            max_conn_per_ip: 5,
            matches: Arc::new(RwLock::new(HashMap::new())),
            num_connections_per_ip: Arc::new(RwLock::new(HashMap::new())),
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

    pub async fn init_server(
        mut self,
        logic: T,
        rpc_client: Arc<RpcClient>,
    ) -> anyhow::Result<()> {
        // // TODO: get this out of here, the client has to call it
        // rustls::crypto::ring::default_provider()
        //     .install_default()
        //     .expect("Failed to install rustls crypto provider");
        let endpoint = quinn::Endpoint::server(self.quinn_config.clone(), self.addr.clone())?;
    
        // I will have many different tokio selects doing different things, so shutdown will be through a cancellation token
        // TODO: pass this from outside so the user can also call it
        let cancellation_token = CancellationToken::new();
        Self::register_signal(cancellation_token.clone()).await;
    
        loop {
            tokio_select!(match .. {
                .. if let incoming = endpoint.accept() => {
                    match incoming {
                        Some(incoming) => {
                            self.handle_incoming(incoming, rpc_client.clone()).await;
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
                        let matches_len = self.matches.read().await.len();
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
        &mut self,
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
            let mut num_connections_per_ip_guard = self.num_connections_per_ip.read().await;
            if let Some(num_connections) = num_connections_per_ip_guard.get(&client_ip) {
                if *num_connections >= self.max_conn_per_ip {
                    // log...
                    incoming.refuse();
                    return;
                }
            }
        }
    
        let rpc_client = rpc_client.clone();
        let matches = self.matches.clone();
        let num_connections_per_ip = self.num_connections_per_ip.clone();

        tokio::spawn(async move {
            let connection = match incoming.await {
                Ok(connection) => connection,
                Err(_e) => {
                    // TODO: handle error. maybe just debug log it and close the connection?
                    return;
                }
            };

            let (mut send_stream, recv_stream) = match connection.accept_bi().await {
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
                connection,
                client_ip.clone(),
                is_loopback,
                rpc_client,
                matches,
                num_connections_per_ip.clone(),
            )
            .await
            {
                // TODO: SEND ERROR TO THE CLIENT AND LOG IT!
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

    pub async fn process_connection(
        connection: Connection,
        client_ip: IpAddr,
        is_loopback: bool,
        rpc_client: Arc<RpcClient>,
        matches: Arc<RwLock<HashMap<u64, MatchInfo<T>>>>,
        num_connections_per_ip: Arc<RwLock<HashMap<IpAddr, u64>>>,
    ) -> anyhow::Result<()> {
        // FIX: increment IP, call process_connection, decrement IP, treat errors as needed

        // TODO: do this with the Incoming instead of the Connection?
        if !is_loopback {
            let mut num_connections_per_ip_guard = num_connections_per_ip.write().await;
            let entry = num_connections_per_ip_guard.entry(client_ip).or_insert(0);
            *entry += 1;
        }

        Ok(())
    }
}


// FIX: read data from lobby
// FIX: perform auth, connect clients to matches, run game logic, read and send messages
// FIX: problema. quando jogo acaba, eu meto a match a false, mas ela nunca chega a ser removida! tenho de fazer com que o utilizador possa, usando um channel ou assim, apagar a crank. ou simplesmente retorno o seu arc para fora? acho que channel é mais clean.
