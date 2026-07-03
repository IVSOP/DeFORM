use std::sync::Arc;

use deform_quic::server::{DeformQuicServer, auth_config::AuthConfig};
use pong::{pong_logic::PongQuicLogic, solana::anchor_client::PongAnchorClient};

pub fn serve(port: u16, rpc_url: &str) -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let mut server = DeformQuicServer::<PongQuicLogic>::new_with_defaults(
        &AuthConfig::DebugConfig,
        PongQuicLogic,
        PongAnchorClient,
    )?;
    server.addr = format!("0.0.0.0:{port}").parse()?;

    tracing::info!("Starting pong server on {}", server.addr);

    let rpc_client = Arc::new(solana_rpc_client::nonblocking::rpc_client::RpcClient::new(
        rpc_url.to_string(),
    ));

    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(server.init_server(rpc_client))?;

    Ok(())
}
