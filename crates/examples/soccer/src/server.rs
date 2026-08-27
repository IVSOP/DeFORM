use std::sync::Arc;

use deform_quic::server::{DeformQuicServer, auth_config::AuthConfig};
use soccer::{soccer_logic::SoccerQuicLogic, solana::anchor_client::SoccerAnchorClient};
use solana_sdk::signer::keypair::read_keypair_file;

pub fn serve(port: u16, rpc_url: &str, keypair_path: &str) -> anyhow::Result<()> {
    let rpc_client = Arc::new(solana_rpc_client::nonblocking::rpc_client::RpcClient::new(
        rpc_url.to_string(),
    ));
    let admin_keypair = Arc::new(
        read_keypair_file(keypair_path)
            .map_err(|e| anyhow::anyhow!("failed to read admin keypair: {e}"))?,
    );

    let mut server = DeformQuicServer::<SoccerQuicLogic>::new_with_defaults(
        &AuthConfig::DebugConfig,
        rpc_client,
        admin_keypair,
        SoccerQuicLogic,
        SoccerAnchorClient,
    )?;
    server.addr = format!("0.0.0.0:{port}").parse()?;

    tracing::info!("Starting soccer server on {}", server.addr);

    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(server.init_server())?;

    Ok(())
}
