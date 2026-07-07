use std::collections::HashMap;

use anchor_lang::prelude::*;
use deform_core::accounts::{
    lobby::{Lobby, LobbyStatus, PLayerStatus, PlayerInfo},
    AccountType,
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

pub fn handler(ctx: Context<CreateLobbyAccounts>, id: u64) -> Result<()> {
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

    let mut player_infos = HashMap::new();
    player_infos.insert(
        creator,
        PlayerInfo {
            status: PLayerStatus::NotReady,
            inputs: Inputs::default(),
        },
    );

    let lobby_account = Lobby::<UserLogic> {
        account_type: AccountType::Lobby,
        id,
        tick: 0,
        creator,
        status: LobbyStatus::NotStarted,
        game_state: None,
        player_infos,
        bump,
    };

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
        data.len(),
        seeds,
    )?;

    lobby_info.data.borrow_mut()[..data.len()].copy_from_slice(&data);

    Ok(())
}
