use std::path::PathBuf;

use clap::{Parser, Subcommand};
use deform_core::{
    Pubkey,
    accounts::{
        DeformAccount, DeformAccountType,
        inputs::InputsAccount,
        lobby::{Lobby, LobbyFinished, LobbyState, Network},
    },
    game_program_client::GameProgramClient,
};
use soccer::{
    soccer_logic::SoccerGame,
    solana::anchor_client::{GAME_PROGRAM, SoccerAnchorClient},
};
use solana_address::Address;
use solana_client::{
    rpc_client::RpcClient,
    rpc_config::{
        RpcAccountInfoConfig, RpcProgramAccountsConfig, UiAccountEncoding, UiDataSliceConfig,
    },
    rpc_filter::{Memcmp, RpcFilterType},
};
use solana_instruction::AccountMeta;
use solana_sdk::{
    message::Message,
    signature::{Keypair, Signature, read_keypair_file},
    signer::Signer,
    transaction::Transaction,
};
use tracing::info;

#[cfg(feature = "client")]
pub mod client;
#[cfg(feature = "client")]
pub mod menu;
#[cfg(feature = "server")]
pub mod server;

#[derive(Parser)]
#[command(name = "soccer")]
struct Cli {
    #[arg(
        long,
        global = true,
        default_value = "http://127.0.0.1:8899",
        env = "RPC_URL"
    )]
    rpc_url: String,
    #[command(subcommand)]
    command: CliCommand,
}

#[derive(Subcommand)]
enum CliCommand {
    #[cfg(feature = "client")]
    #[command(about = "Run the soccer game")]
    Run {
        #[arg(long, env = "WALLET")]
        wallet: Option<PathBuf>,
    },
    #[command(about = "Fetch all lobby accounts from the chain and print as JSON")]
    FetchLobbies,
    #[command(about = "Print the address of every account owned by the game program")]
    FetchAccounts,
    #[command(about = "Write the final scores of a lobby on-chain and close its accounts")]
    CloseLobby {
        #[arg(long)]
        id: u64,
        #[arg(
            long,
            default_value = "../../../anchor_program/PRIVATE_DO_NOT_PUBLISH_THIS/admin.json",
            env = "KEYPAIR_PATH"
        )]
        admin: PathBuf,
    },
    #[command(
        about = "Close any account owned by the game program, refunding its rent to the admin, with no checks at all"
    )]
    ForceClose {
        #[arg(long)]
        account: String,
        #[arg(
            long,
            default_value = "../../../anchor_program/PRIVATE_DO_NOT_PUBLISH_THIS/admin.json",
            env = "KEYPAIR_PATH"
        )]
        admin: PathBuf,
    },
    #[cfg(feature = "server")]
    #[command(about = "Run the QUIC game server")]
    Serve {
        #[arg(long, default_value = "4433", env = "PORT")]
        port: u16,
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

    let rpc_url = cli.rpc_url;
    match cli.command {
        #[cfg(feature = "client")]
        CliCommand::Run { wallet } => crate::client::run_game(wallet),
        CliCommand::FetchLobbies => fetch_lobbies(&rpc_url)?,
        CliCommand::FetchAccounts => fetch_accounts(&rpc_url)?,
        CliCommand::CloseLobby { id, admin } => close_lobby(id, &admin, &rpc_url)?,
        CliCommand::ForceClose { account, admin } => force_close(&account, &admin, &rpc_url)?,
        #[cfg(feature = "server")]
        CliCommand::Serve { port, keypair } => crate::server::serve(port, &rpc_url, &keypair)?,
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
            match DeformAccount::<SoccerGame>::from_bytes(&data) {
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

fn fetch_accounts(rpc_url: &str) -> anyhow::Result<()> {
    let rpc_client = RpcClient::new(rpc_url.to_string());

    let config = RpcProgramAccountsConfig {
        filters: None,
        account_config: RpcAccountInfoConfig {
            encoding: Some(UiAccountEncoding::Base64),
            // we only want the addresses, so skip pulling the data down
            data_slice: Some(UiDataSliceConfig {
                offset: 0,
                length: 0,
            }),
            commitment: None,
            min_context_slot: None,
        },
        with_context: None,
        sort_results: None,
    };

    let accounts = rpc_client.get_program_ui_accounts_with_config(&GAME_PROGRAM, config)?;

    for (pubkey, _) in accounts.iter() {
        println!("{pubkey}");
    }
    Ok(())
}

fn close_lobby(id: u64, admin_path: &std::path::Path, rpc_url: &str) -> anyhow::Result<()> {
    let rpc_client = RpcClient::new(rpc_url.to_string());
    let admin = read_keypair_file(admin_path).map_err(|e| {
        anyhow::anyhow!("failed to read admin keypair {}: {e}", admin_path.display())
    })?;
    let admin_pubkey = Pubkey::from(admin.pubkey().to_bytes());

    let (lobby_pda, _) = Lobby::<SoccerGame>::find_program_address(id, &GAME_PROGRAM);

    let data = rpc_client.get_account_data(&Address::new_from_array(lobby_pda.to_bytes()))?;
    let lobby = match DeformAccount::<SoccerGame>::from_bytes(&data)? {
        DeformAccount::Lobby(lobby) => lobby,
        _ => anyhow::bail!("account {lobby_pda} is not a lobby"),
    };

    // same order the program walks the players in, so the remaining accounts line up
    let players: Vec<Pubkey> = match &lobby.state {
        LobbyState::NotStarted(not_started) => not_started.player_status.keys().copied().collect(),
        LobbyState::Ongoing(ongoing) => ongoing.tick_info.inputs.keys().copied().collect(),
        LobbyState::Finished(LobbyFinished(finished)) => {
            finished.tick_info.inputs.keys().copied().collect()
        }
    };
    info!(
        "closing lobby {id} ({lobby_pda}) with {} players",
        players.len()
    );

    let mut ix = SoccerAnchorClient.write_and_close_ix(
        admin_pubkey,
        lobby_pda,
        lobby.metadata.creator,
        &lobby,
    )?;

    // fully-on-chain lobbies also own one inputs account per player, which the
    // program closes and refunds through the remaining accounts: [inputs, player]
    if matches!(lobby.metadata.network, Network::FullyOnChain(_)) {
        for player in &players {
            let (inputs_pda, _) =
                InputsAccount::<SoccerGame>::find_program_address(id, player, &GAME_PROGRAM);
            ix.accounts.push(AccountMeta::new(inputs_pda, false));
            ix.accounts.push(AccountMeta::new(*player, false));
        }
    }

    let is_localhost = rpc_url.contains("127.0.0.1") || rpc_url.contains("localhost");
    let sig = send_and_confirm_tx(&rpc_client, ix, &admin, is_localhost)?;
    println!("{sig}");

    Ok(())
}

/// Escape hatch for accounts the program can no longer deserialize (e.g. lobbies written
/// by an older layout), which is what makes [`close_lobby`] fail on them.
fn force_close(account: &str, admin_path: &std::path::Path, rpc_url: &str) -> anyhow::Result<()> {
    let rpc_client = RpcClient::new(rpc_url.to_string());
    let admin = read_keypair_file(admin_path).map_err(|e| {
        anyhow::anyhow!("failed to read admin keypair {}: {e}", admin_path.display())
    })?;
    let admin_pubkey = Pubkey::from(admin.pubkey().to_bytes());
    let account: Pubkey = account.parse()?;

    let ix = SoccerAnchorClient.force_close_ix(admin_pubkey, account)?;

    info!("force closing {account}, refunding its rent to {admin_pubkey}");
    let is_localhost = rpc_url.contains("127.0.0.1") || rpc_url.contains("localhost");
    let sig = send_and_confirm_tx(&rpc_client, ix, &admin, is_localhost)?;
    println!("{sig}");

    Ok(())
}

pub fn to_sdk_ix(ix: solana_instruction::Instruction) -> solana_sdk::instruction::Instruction {
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

pub fn send_and_confirm_tx(
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
        rpc.send_transaction(&tx)?
    } else {
        rpc.send_and_confirm_transaction(&tx)?
    };
    Ok(sig)
}
