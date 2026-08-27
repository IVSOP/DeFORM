use std::sync::Arc;

use deform_quic::server::{DeformQuicServer, auth_config::AuthConfig};
use shooter::{shooter_logic::ShooterQuicLogic, solana::anchor_client::ShooterAnchorClient};
use solana_sdk::signer::keypair::read_keypair_file;

/// Identical shape to pong's server: the QUIC server is the match authority and
/// runs the same avian/tnua `advance_frame` as the clients — just headless.
pub fn serve(port: u16, rpc_url: &str, keypair_path: &str) -> anyhow::Result<()> {
    let rpc_client = Arc::new(solana_rpc_client::nonblocking::rpc_client::RpcClient::new(
        rpc_url.to_string(),
    ));
    let admin_keypair = Arc::new(
        read_keypair_file(keypair_path)
            .map_err(|e| anyhow::anyhow!("failed to read admin keypair: {e}"))?,
    );

    let mut server = DeformQuicServer::<ShooterQuicLogic>::new_with_defaults(
        &AuthConfig::DebugConfig,
        rpc_client,
        admin_keypair,
        ShooterQuicLogic,
        ShooterAnchorClient,
    )?;
    server.addr = format!("0.0.0.0:{port}").parse()?;

    tracing::info!("Starting shooter server on {}", server.addr);

    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(server.init_server())?;

    Ok(())
}
