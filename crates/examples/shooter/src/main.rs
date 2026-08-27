#[cfg(feature = "client")]
use std::path::PathBuf;

use clap::{Parser, Subcommand};
use deform_core::accounts::{DeformAccount, DeformAccountType};
use shooter::{shooter_logic::ShooterGame, solana::anchor_client::GAME_PROGRAM};
use solana_address::Address;
use solana_client::{
    rpc_client::RpcClient,
    rpc_config::{RpcAccountInfoConfig, RpcProgramAccountsConfig, UiAccountEncoding},
    rpc_filter::{Memcmp, RpcFilterType},
};
use solana_sdk::{
    message::Message,
    signature::{Keypair, Signature},
    signer::Signer,
    transaction::Transaction,
};

#[cfg(feature = "client")]
pub mod client;
#[cfg(feature = "client")]
pub mod menu;
#[cfg(feature = "server")]
pub mod server;

#[derive(Parser)]
#[command(name = "shooter")]
struct Cli {
    #[command(subcommand)]
    command: CliCommand,
}

#[derive(Subcommand)]
enum CliCommand {
    #[cfg(feature = "client")]
    #[command(about = "Run the shooter game")]
    Run {
        #[arg(long, env = "WALLET")]
        wallet: Option<PathBuf>,
    },
    #[command(about = "Fetch all lobby accounts from the chain and print as JSON")]
    FetchLobbies {
        #[arg(long, default_value = "https://127.0.0.1:8899", env = "RPC_URL")]
        rpc_url: String,
    },
    #[cfg(feature = "server")]
    #[command(about = "Run the QUIC game server")]
    Serve {
        #[arg(long, default_value = "4433", env = "PORT")]
        port: u16,
        // The server always talks to the devnet base layer; the localhost docker
        // stack overrides this with RPC_URL=http://surfpool:8899.
        #[arg(long, default_value = "https://api.devnet.solana.com", env = "RPC_URL")]
        rpc_url: String,
        #[arg(
            long,
            default_value = "../../../anchor_program/PRIVATE_DO_NOT_PUBLISH_THIS/admin.json",
            env = "KEYPAIR_PATH"
        )]
        keypair: String,
    },
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    // bevy's LogPlugin installs its own global subscriber, so the game client has to
    // be the one command we leave alone; everything else logs through tracing here.
    let bevy_owns_logging = match &cli.command {
        #[cfg(feature = "client")]
        CliCommand::Run { .. } => true,
        _ => false,
    };
    if !bevy_owns_logging {
        tracing_subscriber::fmt()
            .with_env_filter(
                tracing_subscriber::EnvFilter::try_from_default_env()
                    .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
            )
            .init();
    }

    match cli.command {
        #[cfg(feature = "client")]
        CliCommand::Run { wallet } => crate::client::run_game(wallet),
        CliCommand::FetchLobbies { rpc_url } => fetch_lobbies(&rpc_url)?,
        #[cfg(feature = "server")]
        CliCommand::Serve {
            port,
            rpc_url,
            keypair,
        } => crate::server::serve(port, &rpc_url, &keypair)?,
    }
    Ok(())
}

fn fetch_lobbies(rpc_url: &str) -> anyhow::Result<()> {
    let rpc_client = RpcClient::new(rpc_url.to_string());

    let program_id = GAME_PROGRAM;

    let discriminator_bytes = wincode::serialize(&DeformAccountType::Lobby)?;

    let config = RpcProgramAccountsConfig {
        filters: Some(vec![RpcFilterType::Memcmp(Memcmp::new_raw_bytes(
            0,
            discriminator_bytes,
        ))]),
        account_config: RpcAccountInfoConfig {
            encoding: Some(UiAccountEncoding::Base64),
            data_slice: None,
            commitment: None,
            min_context_slot: None,
        },
        with_context: None,
        sort_results: None,
    };

    let accounts = rpc_client.get_program_ui_accounts_with_config(&program_id, config)?;

    let mut results: Vec<serde_json::Value> = Vec::new();

    for (pubkey, account_info) in accounts.iter() {
        if let Some(data) = account_info.data.decode() {
            match DeformAccount::<ShooterGame>::from_bytes(&data) {
                Ok(DeformAccount::Lobby(lobby)) => {
                    let mut obj = serde_json::to_value(&lobby)?;
                    obj.as_object_mut().unwrap().insert(
                        "pubkey".to_string(),
                        serde_json::Value::String(pubkey.to_string()),
                    );
                    results.push(obj);
                }
                Ok(_) => {
                    anyhow::bail!("Failed to deserialize lobby {}: wrong account type", pubkey);
                }
                Err(e) => {
                    anyhow::bail!("Failed to deserialize lobby {}: {}", pubkey, e);
                }
            }
        } else {
            anyhow::bail!("Failed to decode account data for {}", pubkey);
        }
    }

    println!("{}", serde_json::to_string_pretty(&results)?);
    Ok(())
}

fn to_sdk_ix(ix: solana_instruction::Instruction) -> solana_sdk::instruction::Instruction {
    solana_sdk::instruction::Instruction {
        program_id: Address::new_from_array(ix.program_id.to_bytes()),
        accounts: ix
            .accounts
            .iter()
            .map(|a| solana_sdk::instruction::AccountMeta {
                pubkey: Address::new_from_array(a.pubkey.to_bytes()),
                is_signer: a.is_signer,
                is_writable: a.is_writable,
            })
            .collect(),
        data: ix.data,
    }
}

fn send_and_confirm_tx(
    rpc: &RpcClient,
    ix: solana_instruction::Instruction,
    keypair: &Keypair,
    is_localhost: bool,
) -> anyhow::Result<Signature> {
    let ix = to_sdk_ix(ix);
    let blockhash = rpc.get_latest_blockhash()?;
    let msg = Message::new(&[ix], Some(&keypair.pubkey()));
    let mut tx = Transaction::new_unsigned(msg);
    tx.sign(&[keypair], blockhash);
    let sig = if is_localhost {
        // confirming tx is taking wayyyy too long on localhost for some reason
        rpc.send_transaction(&tx)?
    } else {
        rpc.send_and_confirm_transaction(&tx)?
    };
    Ok(sig)
}
