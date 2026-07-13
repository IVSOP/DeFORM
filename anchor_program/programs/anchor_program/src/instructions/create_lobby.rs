use std::collections::BTreeMap;

use anchor_lang::prelude::*;
use deform_core::{
    accounts::{
        lobby::{
            not_started::LobbyNotStarted, Lobby, LobbyMetadata, LobbyState, Network, PlayerStatus,
        },
        DeformAccount,
    },
    DeformUserLogic,
};

use crate::{error::GameProgramError, state::*, util::create_pda_account};

#[derive(Accounts)]
pub struct CreateLobbyAccounts<'info> {
    #[account(mut)]
    pub user: Signer<'info>,
    /// CHECK: PDA derived and verified manually because LobbyAccount uses wincode, not borsh.
    #[account(mut)]
    pub lobby: UncheckedAccount<'info>,
    pub system_program: Program<'info, System>,
}

pub fn handler(ctx: Context<CreateLobbyAccounts>, id: u64, network: Network) -> Result<()> {
    let program_id = ctx.program_id;
    let lobby_info = ctx.accounts.lobby.to_account_info();

    let (pda, bump) = Lobby::<UserLogic>::find_program_address(id, program_id);
    require_keys_eq!(lobby_info.key(), pda);

    // must be uninitialized (still owned by the system program, no data)
    require!(
        lobby_info.data_is_empty() && lobby_info.owner == &ctx.accounts.system_program.key(),
        GameProgramError::LobbyAlreadyInitialized
    );

    let creator = *ctx.accounts.user.key;

    let mut player_status = BTreeMap::new();
    player_status.insert(creator, PlayerStatus::NotReady);

    let lobby_account = DeformAccount::Lobby(Lobby {
        metadata: LobbyMetadata {
            id,
            creator,
            network,
            bump,
        },
        state: LobbyState::<UserLogic>::NotStarted(LobbyNotStarted { player_status }),
    });

    // serialize into a Vec
    let data =
        wincode::serialize(&lobby_account).map_err(|_| error!(GameProgramError::SerializeLobby))?;

    // TODO: this should be a call to Lobby
    let seeds: &[&[u8]] = &[b"lobby", &id.to_le_bytes(), &[bump]];
    create_pda_account(
        &ctx.accounts.user.to_account_info(),
        &lobby_info,
        ctx.accounts.system_program.key(),
        program_id,
        // create account already using the max space
        UserLogic::MAX_LOBBY_ACCOUNT_BYTES,
        seeds,
    )?;

    lobby_info.data.borrow_mut()[..data.len()].copy_from_slice(&data);

    Ok(())
}
