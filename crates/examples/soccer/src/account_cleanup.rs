//! Base-chain inventory and admin cleanup, including accounts delegated to an ER.
use std::{
    collections::BTreeMap,
    path::Path,
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail, ensure};
use deform_core::{
    Pubkey,
    accounts::{
        DeformAccount,
        lobby::{DevnetRegion, Lobby, LocalRegion, MainnetRegion},
    },
    game_program_client::GameProgramClient,
};
use ephemeral_rollups_sdk::{
    consts::{DELEGATION_PROGRAM_ID, MAGIC_CONTEXT_ID, MAGIC_PROGRAM_ID},
    delegate_args::DelegateAccounts,
    dlp_api::{
        args::CommitFinalizeArgs,
        instruction_builder,
        state::{
            DelegationMetadata, DelegationRecord, UndelegationRequester,
            discriminator::AccountDiscriminator,
        },
    },
};
use soccer::{
    generated::instructions::{Undelegate, UndelegateInstructionArgs},
    soccer_logic::SoccerGame,
    solana::anchor_client::{GAME_PROGRAM, SoccerAnchorClient},
};
use solana_client::{
    rpc_client::RpcClient,
    rpc_config::{
        RpcAccountInfoConfig, RpcProgramAccountsConfig, RpcSimulateTransactionConfig,
        UiAccountEncoding,
    },
    rpc_filter::{Memcmp, RpcFilterType},
};
use solana_instruction::AccountMeta;
use solana_sdk::{
    message::Message,
    signature::{Keypair, read_keypair_file},
    signer::Signer,
    transaction::Transaction,
};

#[derive(Clone, Debug)]
struct Entry {
    address: Pubkey,
    lobby_id: Option<u64>,
    validator: Option<Pubkey>,
}

fn config(filters: Vec<RpcFilterType>) -> RpcProgramAccountsConfig {
    RpcProgramAccountsConfig {
        filters: Some(filters),
        account_config: RpcAccountInfoConfig {
            encoding: Some(UiAccountEncoding::Base64),
            ..Default::default()
        },
        ..Default::default()
    }
}

// Metadata stores discriminator (8), commit id (8), requester (1), seed count (4),
// then the first seed's Borsh length and bytes. Match both its length and contents.
fn metadata_filters(tag: &[u8]) -> Vec<RpcFilterType> {
    let mut seed = (tag.len() as u32).to_le_bytes().to_vec();
    seed.extend_from_slice(tag);
    vec![
        RpcFilterType::Memcmp(Memcmp::new_raw_bytes(
            0,
            AccountDiscriminator::DelegationMetadata.to_bytes().to_vec(),
        )),
        RpcFilterType::Memcmp(Memcmp::new_raw_bytes(21, seed)),
    ]
}

fn metadata_entry(address: &Pubkey, data: &[u8]) -> Result<Option<Entry>> {
    let metadata = DelegationMetadata::try_from_bytes_with_discriminator(data)
        .map_err(|e| anyhow::anyhow!("Invalid delegation metadata {address}: {e}"))?;
    let seeds = &metadata.seeds;
    let recognized = match seeds.as_slice() {
        [tag, id] => tag == b"lobby" && id.len() == 8,
        [tag, id, player] => tag == b"inputs" && id.len() == 8 && player.len() == 32,
        _ => false,
    };
    if !recognized {
        return Ok(None);
    }
    let refs: Vec<&[u8]> = seeds.iter().map(Vec::as_slice).collect();
    let (account, _) = Pubkey::find_program_address(&refs, &GAME_PROGRAM);
    // Other applications can use the same seed names. Only accept our exact PDA.
    if DelegateAccounts::new(account, GAME_PROGRAM).delegation_metadata != *address {
        return Ok(None);
    }
    Ok(Some(Entry {
        address: account,
        lobby_id: Some(u64::from_le_bytes(seeds[1].as_slice().try_into()?)),
        validator: None,
    }))
}

fn inventory(rpc: &RpcClient) -> Result<Vec<Entry>> {
    let mut entries = BTreeMap::new();
    for (address, account) in rpc.get_program_accounts(&GAME_PROGRAM)? {
        let lobby_id = match DeformAccount::<SoccerGame>::from_bytes(&account.data) {
            Ok(DeformAccount::Lobby(lobby)) => Some(lobby.metadata.id),
            Ok(DeformAccount::Inputs(inputs)) => Some(inputs.lobby_id),
            Err(_) => None, // Force-close still supports accounts from old layouts.
        };
        entries.insert(
            address,
            Entry {
                address,
                lobby_id,
                validator: None,
            },
        );
    }
    // Delegated base accounts have zeroed data and a different owner. Discover
    // them from DLP metadata, which preserves their original PDA seeds.
    for tag in [b"lobby".as_slice(), b"inputs".as_slice()] {
        for (metadata_address, metadata) in rpc.get_program_ui_accounts_with_config(
            &DELEGATION_PROGRAM_ID,
            config(metadata_filters(tag)),
        )? {
            let data = metadata
                .data
                .decode()
                .context("Cannot decode delegation metadata")?;
            let Some(mut entry) = metadata_entry(&metadata_address, &data)? else {
                continue;
            };
            let companions = DelegateAccounts::new(entry.address, GAME_PROGRAM);
            let accounts =
                rpc.get_multiple_accounts(&[entry.address, companions.delegation_record])?;
            let Some(base) = &accounts[0] else {
                continue;
            };
            if base.owner == GAME_PROGRAM {
                entries.insert(entry.address, entry);
                continue;
            }
            ensure!(
                base.owner == DELEGATION_PROGRAM_ID,
                "Unexpected owner for {}: {}",
                entry.address,
                base.owner
            );
            let record_account = accounts[1].as_ref().context(
                "Delegated account has no delegation record; retry after synchronization",
            )?;
            ensure!(
                record_account.owner == DELEGATION_PROGRAM_ID,
                "Invalid delegation record owner for {}",
                entry.address
            );
            let record = DelegationRecord::try_from_bytes_with_discriminator(&record_account.data)
                .map_err(|e| {
                    anyhow::anyhow!("Invalid delegation record for {}: {e}", entry.address)
                })?;
            ensure!(
                record.owner.to_bytes() == GAME_PROGRAM.to_bytes(),
                "Delegation record owner mismatch for {}",
                entry.address
            );
            entry.validator = Some(Pubkey::new_from_array(record.authority.to_bytes()));
            entries.insert(entry.address, entry);
        }
    }
    Ok(entries.into_values().collect())
}

fn print_entries(entries: &[Entry]) {
    println!("ACCOUNT\tSTATUS\tLOBBY\tVALIDATOR");
    for entry in entries {
        println!(
            "{}\t{}\t{}\t{}",
            entry.address,
            if entry.validator.is_some() {
                "delegated"
            } else {
                "undelegated"
            },
            entry
                .lobby_id
                .map(|id| id.to_string())
                .unwrap_or_else(|| "unknown".into()),
            entry
                .validator
                .map(|key| key.to_string())
                .unwrap_or_else(|| "-".into())
        );
    }
    if entries.is_empty() {
        eprintln!("No soccer accounts found.");
    }
}

pub fn list(rpc_url: &str) -> Result<()> {
    print_entries(&inventory(&RpcClient::new(rpc_url.to_owned()))?);
    Ok(())
}

fn endpoint(validator: &Pubkey, genesis_hash: &str) -> Result<&'static str> {
    let devnet = genesis_hash == "EtWTRABZaYq6iMfeYKouRu166VU2xqa1wcaWoxPkrZBG";
    for region in [
        DevnetRegion::Asia,
        DevnetRegion::EU,
        DevnetRegion::US,
        DevnetRegion::TEE,
    ] {
        if region.address() == *validator && devnet {
            return Ok(region.er_endpoints().rpc);
        }
    }
    for region in [
        MainnetRegion::Asia,
        MainnetRegion::EU,
        MainnetRegion::US,
        MainnetRegion::TEE,
    ] {
        if region.address() == *validator && !devnet {
            return Ok(region.er_endpoints().rpc);
        }
    }
    if LocalRegion::Local.address() == *validator {
        ensure!(
            !devnet,
            "Devnet accounts are delegated to the Localhost validator identity ({validator}). Supply --er-rpc-url for a validator with that identity backed by devnet; the ordinary localhost stack uses a different base chain"
        );
        return Ok(LocalRegion::Local.er_endpoints().rpc);
    }
    bail!("Unknown validator {validator}; supply --er-rpc-url for its ER endpoint")
}

fn undelegate_lobby(
    base: &RpcClient,
    entry: &Entry,
    all: &[Entry],
    admin: &Keypair,
    er_override: Option<&str>,
    genesis_hash: &str,
    timeout: Duration,
) -> Result<()> {
    let id = entry
        .lobby_id
        .context("Cannot determine lobby ID for delegated account")?;
    let validator = entry.validator.context("Missing delegation validator")?;
    let lobby_address = Lobby::<SoccerGame>::find_program_address(id, &GAME_PROGRAM).0;
    let group: Vec<&Entry> = all
        .iter()
        .filter(|item| item.lobby_id == Some(id) && item.validator.is_some())
        .collect();
    ensure!(
        group.iter().any(|item| item.address == lobby_address),
        "Lobby {id} is missing or already undelegated, but inputs remain delegated; cannot recover them with the lobby undelegate instruction"
    );
    ensure!(
        group.iter().all(|item| item.validator == Some(validator)),
        "Lobby {id} has accounts delegated to different validators"
    );
    let url = match er_override {
        Some(url) => url,
        None => endpoint(&validator, genesis_hash)?,
    };
    let er = RpcClient::new(url.to_owned());
    ensure!(
        er.get_identity()? == validator,
        "ER endpoint {url} does not match delegated validator {validator}"
    );
    let account = er.get_account(&lobby_address)?;
    ensure!(
        account.owner == GAME_PROGRAM,
        "Lobby {id} is not available with soccer ownership at {url}"
    );
    let lobby = match DeformAccount::<SoccerGame>::from_bytes(&account.data)? {
        DeformAccount::Lobby(lobby) => lobby,
        _ => bail!("Account {lobby_address} is not a lobby"),
    };
    // Preserve the player ordering expected by the on-chain handler.
    let players: Vec<Pubkey> = match &lobby.state {
        deform_core::accounts::lobby::LobbyState::NotStarted(state) => {
            state.player_status.keys().copied().collect()
        }
        deform_core::accounts::lobby::LobbyState::Ongoing(state) => {
            state.tick_info.inputs.keys().copied().collect()
        }
        deform_core::accounts::lobby::LobbyState::Finished(state) => {
            state.0.tick_info.inputs.keys().copied().collect()
        }
    };
    let inputs: Vec<Pubkey> = players
        .iter()
        .map(|player| {
            deform_core::accounts::inputs::InputsAccount::<SoccerGame>::find_program_address(
                id,
                player,
                &GAME_PROGRAM,
            )
            .0
        })
        .collect();
    ensure!(
        inputs
            .iter()
            .all(|address| group.iter().any(|item| item.address == *address)),
        "Lobby {id} is only partially delegated; refusing an incomplete undelegation transaction"
    );
    ensure!(
        group.len() == inputs.len() + 1,
        "Lobby {id} has orphan delegated inputs; manual recovery is required"
    );
    let remaining: Vec<_> = inputs
        .iter()
        .map(|address| AccountMeta::new(*address, false))
        .collect();
    let ix = Undelegate {
        admin: admin.pubkey(),
        lobby: lobby_address,
        magic_context: MAGIC_CONTEXT_ID,
        magic_program: MAGIC_PROGRAM_ID,
    }
    .instruction_with_remaining_accounts(UndelegateInstructionArgs { id }, &remaining);
    eprintln!(
        "Undelegating lobby {id} and {} inputs via {url}",
        inputs.len()
    );
    let signature = crate::send_and_confirm_tx(&er, ix, admin, false)?;
    eprintln!("Undelegation submitted: {signature}; waiting for base-chain ownership");
    let addresses: Vec<_> = group.iter().map(|item| item.address).collect();
    let started = Instant::now();
    loop {
        let accounts = base.get_multiple_accounts(&addresses)?;
        let owners: Vec<_> = accounts
            .iter()
            .map(|account| account.as_ref().map(|account| account.owner))
            .collect();
        if returned_to_base(&owners)? {
            return Ok(());
        }
        ensure!(
            started.elapsed() < timeout,
            "Timed out waiting for lobby {id} undelegation ({signature}); no accounts in this lobby were closed. Check base-chain ownership before retrying"
        );
        thread::sleep(Duration::from_secs(2));
    }
}

fn returned_to_base(owners: &[Option<Pubkey>]) -> Result<bool> {
    for owner in owners.iter().flatten() {
        ensure!(
            *owner == GAME_PROGRAM || *owner == DELEGATION_PROGRAM_ID,
            "Unexpected account owner during undelegation: {owner}"
        );
    }
    // An absent account may have already been closed by another cleanup.
    Ok(owners
        .iter()
        .all(|owner| owner.is_none() || *owner == Some(GAME_PROGRAM)))
}

/// Cleanup-only recovery: the known local validator key signs an empty final
/// commit, undelegation and admin force-close in ONE atomic base-chain transaction.
/// No running validator or ER endpoint is needed; no game state is retained.
fn recover_local_account(
    base: &RpcClient,
    entry: &Entry,
    admin: &Keypair,
    validator: &Keypair,
    simulate: bool,
) -> Result<()> {
    ensure!(
        base.get_genesis_hash()?.to_string() == "EtWTRABZaYq6iMfeYKouRu166VU2xqa1wcaWoxPkrZBG",
        "Direct local-validator recovery is restricted to devnet"
    );
    ensure!(
        validator.pubkey() == LocalRegion::Local.address(),
        "Recovery key must match the repository's local validator identity"
    );
    ensure!(
        entry.validator == Some(validator.pubkey()),
        "Recovery key does not match {}'s delegated authority",
        entry.address
    );
    let companions = DelegateAccounts::new(entry.address, GAME_PROGRAM);
    let metadata_account = base.get_account(&companions.delegation_metadata)?;
    let metadata = DelegationMetadata::try_from_bytes_with_discriminator(&metadata_account.data)
        .map_err(|e| anyhow::anyhow!("Cannot decode recovery metadata: {e}"))?;
    let record_account = base.get_account(&companions.delegation_record)?;
    let record = DelegationRecord::try_from_bytes_with_discriminator(&record_account.data)
        .map_err(|e| anyhow::anyhow!("Cannot decode recovery delegation record: {e}"))?;
    ensure!(
        record.owner.to_bytes() == GAME_PROGRAM.to_bytes(),
        "Recovery account is not delegated from soccer"
    );
    ensure!(
        record.authority.to_bytes() == validator.pubkey().to_bytes(),
        "Delegation authority changed since listing"
    );
    ensure!(
        metadata.undelegation_requester == UndelegationRequester::None,
        "{} already has an undelegation request; use the normal cleanup path",
        entry.address
    );
    let pending = [b"state-diff".as_slice(), b"commit-state-record".as_slice()].map(|tag| {
        Pubkey::find_program_address(&[tag, entry.address.as_array()], &DELEGATION_PROGRAM_ID).0
    });
    ensure!(
        base.get_multiple_accounts(&pending)?
            .iter()
            .all(Option::is_none),
        "{} has a pending commit; refusing to discard it during local-validator recovery",
        entry.address
    );

    let mut args = CommitFinalizeArgs {
        commit_id: metadata
            .last_commit_id
            .checked_add(1)
            .context("Commit ID overflow")?,
        lamports: record.lamports,
        allow_undelegation: true.into(),
        data_is_diff: false.into(),
        bumps: Default::default(),
        reserved_padding: [0; 3],
    };
    let validator_address = validator.pubkey().to_bytes().into();
    let account_address = entry.address.to_bytes().into();
    let (commit, _) =
        instruction_builder::commit_finalize(validator_address, account_address, &mut args, &[]);
    let undelegate = instruction_builder::undelegate(
        validator_address,
        account_address,
        GAME_PROGRAM.to_bytes().into(),
        metadata.rent_payer.to_bytes().into(),
    );
    let close = SoccerAnchorClient.force_close_ix(admin.pubkey(), entry.address)?;
    // dlp-api uses a different Pubkey version at this boundary.
    let convert =
        |ix: ephemeral_rollups_sdk::compat::Instruction| solana_instruction::Instruction {
            program_id: Pubkey::new_from_array(ix.program_id.to_bytes()),
            accounts: ix
                .accounts
                .into_iter()
                .map(|meta| AccountMeta {
                    pubkey: Pubkey::new_from_array(meta.pubkey.to_bytes()),
                    is_signer: meta.is_signer,
                    is_writable: meta.is_writable,
                })
                .collect(),
            data: ix.data,
        };
    let instructions = [convert(commit), convert(undelegate), close].map(crate::to_sdk_ix);
    let message = Message::new(&instructions, Some(&admin.pubkey()));
    let mut tx = Transaction::new_unsigned(message);
    tx.try_sign(&[admin, validator], base.get_latest_blockhash()?)?;
    let result = base
        .simulate_transaction_with_config(
            &tx,
            RpcSimulateTransactionConfig {
                sig_verify: true,
                ..Default::default()
            },
        )?
        .value;
    if let Some(error) = result.err {
        bail!(
            "Recovery simulation failed for {}: {error:?}\n{}",
            entry.address,
            result.logs.unwrap_or_default().join("\n")
        );
    }
    if simulate {
        eprintln!(
            "Recovery simulation passed for {} (no transaction sent)",
            entry.address
        );
    } else {
        eprintln!(
            "Recovering and closing {} directly on devnet; discarding its ER state",
            entry.address
        );
        let signature = base.send_and_confirm_transaction(&tx)?;
        println!("{signature}");
    }
    Ok(())
}

pub fn clear(
    rpc_url: &str,
    dry_run: bool,
    selected: Option<Pubkey>,
    lobby_id: Option<u64>,
    admin_path: &Path,
    er_override: Option<&str>,
    validator_path: Option<&Path>,
    timeout_secs: u64,
) -> Result<()> {
    let base = RpcClient::new(rpc_url.to_owned());
    let all = inventory(&base)?;
    let targets: Vec<_> = all
        .iter()
        .filter(|item| selected.is_none() || selected == Some(item.address))
        .filter(|item| lobby_id.is_none() || lobby_id == item.lobby_id)
        .cloned()
        .collect();
    if let Some(address) = selected {
        ensure!(
            !targets.is_empty(),
            "Account {address} is not in the soccer account inventory"
        );
    }
    print_entries(&targets);
    if (dry_run && validator_path.is_none()) || targets.is_empty() {
        return Ok(());
    }
    ensure!(timeout_secs > 0, "--timeout-secs must be greater than zero");
    let admin = read_keypair_file(admin_path)
        .map_err(|e| anyhow::anyhow!("Cannot read admin keypair {}: {e}", admin_path.display()))?;
    let recovery_validator = validator_path
        .map(|path| {
            read_keypair_file(path)
                .map_err(|_| anyhow::anyhow!("Cannot read validator recovery keypair"))
        })
        .transpose()?;
    let genesis_hash = base.get_genesis_hash()?.to_string();
    let mut completed = std::collections::BTreeSet::new();
    for entry in &targets {
        if let Some(validator) = &recovery_validator {
            if entry.validator == Some(validator.pubkey()) {
                recover_local_account(&base, entry, &admin, validator, dry_run)?;
                continue;
            }
        }
        if dry_run {
            continue;
        }
        // Recheck ownership immediately before acting, since inventory can become stale.
        let account = base
            .get_account_with_commitment(&entry.address, base.commitment())?
            .value;
        let Some(account) = account else {
            eprintln!("{} already closed", entry.address);
            continue;
        };
        if account.owner == DELEGATION_PROGRAM_ID {
            ensure!(
                entry.validator.is_some(),
                "{} was delegated after listing; rerun cleanup",
                entry.address
            );
            let id = entry.lobby_id.context("Missing lobby ID")?;
            if completed.insert(id) {
                undelegate_lobby(
                    &base,
                    entry,
                    &all,
                    &admin,
                    er_override,
                    &genesis_hash,
                    Duration::from_secs(timeout_secs),
                )?;
            }
        }
        let account = base
            .get_account_with_commitment(&entry.address, base.commitment())?
            .value;
        if let Some(account) = account {
            ensure!(
                account.owner == GAME_PROGRAM,
                "{} is not owned by soccer on base chain; refusing to close",
                entry.address
            );
            crate::force_close(&entry.address.to_string(), admin_path, rpc_url)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use ephemeral_rollups_sdk::dlp_api::state::UndelegationRequester;

    use super::*;

    fn metadata(seeds: Vec<Vec<u8>>) -> Vec<u8> {
        let mut data = Vec::new();
        DelegationMetadata {
            last_commit_id: 0,
            undelegation_requester: UndelegationRequester::None,
            seeds,
            rent_payer: Default::default(),
        }
        .to_bytes_with_discriminator(&mut data)
        .unwrap();
        data
    }

    #[test]
    fn discovers_lobby_from_delegation_metadata_and_rejects_other_programs() {
        let id = 42u64;
        let data = metadata(vec![b"lobby".to_vec(), id.to_le_bytes().to_vec()]);
        let lobby = Lobby::<SoccerGame>::find_program_address(id, &GAME_PROGRAM).0;
        let address = DelegateAccounts::new(lobby, GAME_PROGRAM).delegation_metadata;
        let entry = metadata_entry(&address, &data).unwrap().unwrap();
        assert_eq!(entry.address, lobby);
        assert_eq!(entry.lobby_id, Some(id));
        let foreign_lobby = Pubkey::find_program_address(
            &[b"lobby", &id.to_le_bytes()],
            &Pubkey::new_from_array([7; 32]),
        )
        .0;
        let foreign_metadata =
            DelegateAccounts::new(foreign_lobby, GAME_PROGRAM).delegation_metadata;
        assert!(metadata_entry(&foreign_metadata, &data).unwrap().is_none());
    }

    #[test]
    fn discovers_inputs_without_deserializing_zeroed_base_data() {
        let id = 3u64;
        let player = Pubkey::new_from_array([4; 32]);
        let data = metadata(vec![
            b"inputs".to_vec(),
            id.to_le_bytes().to_vec(),
            player.to_bytes().to_vec(),
        ]);
        let account =
            deform_core::accounts::inputs::InputsAccount::<SoccerGame>::find_program_address(
                id,
                &player,
                &GAME_PROGRAM,
            )
            .0;
        let address = DelegateAccounts::new(account, GAME_PROGRAM).delegation_metadata;
        assert_eq!(
            metadata_entry(&address, &data).unwrap().unwrap().address,
            account
        );
        assert_eq!(
            &data[21..31],
            &[6, 0, 0, 0, b'i', b'n', b'p', b'u', b't', b's']
        );
    }

    #[test]
    fn incomplete_or_foreign_ownership_never_allows_closure() {
        assert!(!returned_to_base(&[Some(GAME_PROGRAM), Some(DELEGATION_PROGRAM_ID)]).unwrap());
        assert!(returned_to_base(&[Some(GAME_PROGRAM), None]).unwrap());
        assert!(returned_to_base(&[Some(Pubkey::new_from_array([9; 32]))]).is_err());
    }

    #[test]
    fn routes_by_delegation_authority_instead_of_generic_devnet_endpoint() {
        // Full getGenesisHash result from api.devnet.solana.com. A truncated
        // comparison previously classified devnet as non-devnet and dialed localhost.
        let genesis_hash = "EtWTRABZaYq6iMfeYKouRu166VU2xqa1wcaWoxPkrZBG";
        assert_eq!(
            endpoint(&DevnetRegion::EU.address(), genesis_hash).unwrap(),
            "https://devnet-eu.magicblock.app"
        );
        assert_eq!(
            endpoint(&DevnetRegion::Asia.address(), genesis_hash).unwrap(),
            "https://devnet-as.magicblock.app"
        );
        assert!(endpoint(&Pubkey::new_from_array([9; 32]), genesis_hash).is_err());
        let error = endpoint(&LocalRegion::Local.address(), genesis_hash).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("Devnet accounts are delegated to the Localhost validator identity")
        );
        assert_eq!(
            endpoint(&LocalRegion::Local.address(), "local-test-genesis").unwrap(),
            "http://127.0.0.1:7799"
        );
    }
}
