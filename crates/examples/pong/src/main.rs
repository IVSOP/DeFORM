#[cfg(feature = "client")]
use std::path::PathBuf;

use clap::{Parser, Subcommand};
use deform_core::accounts::{DeformAccount, DeformAccountType};
use pong::{pong_logic::PongGame, solana::anchor_client::GAME_PROGRAM};
use solana_address::Address;
use solana_client::{
    rpc_client::RpcClient,
    rpc_config::{RpcAccountInfoConfig, RpcProgramAccountsConfig, UiAccountEncoding},
    rpc_filter::{Memcmp, RpcFilterType},
};
use solana_sdk::{message::Message, signature::Keypair, signer::Signer, transaction::Transaction};
use tracing::info;

#[cfg(feature = "client")]
pub mod client;
#[cfg(feature = "client")]
pub mod menu;
#[cfg(feature = "server")]
pub mod server;

#[derive(Parser)]
#[command(name = "pong")]
struct Cli {
    #[command(subcommand)]
    command: CliCommand,
}

#[derive(Subcommand)]
enum CliCommand {
    #[cfg(feature = "client")]
    #[command(about = "Run the pong game")]
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
        #[arg(long, default_value = "http://127.0.0.1:8899", env = "RPC_URL")]
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
            match DeformAccount::<PongGame>::from_bytes(&data) {
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
) -> anyhow::Result<()> {
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
    info!("tx confirmed: {sig}");
    Ok(())
}
