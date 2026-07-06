use std::collections::HashMap;

use anchor_lang::{prelude::*, system_program};
use deform_core::accounts::{
    lobby::{Lobby, LobbyStatus, PLayerStatus, PlayerInfo},
    AccountType,
};

use crate::{error::GameProgramError, state::*};

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
    let (pda, bump) = Lobby::<UserLogic>::find_program_address(id, program_id);
    require_keys_eq!(ctx.accounts.lobby.key(), pda);

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

    let rent = Rent::get()?;
    let lamports = rent.minimum_balance(data.len());

    let seeds: &[&[u8]] = &[b"lobby", &id.to_le_bytes(), &[bump]];
    // NOTE: this will ensure the account is not already initialized
    system_program::create_account(
        CpiContext::new_with_signer(
            ctx.accounts.system_program.key(),
            system_program::CreateAccount {
                from: ctx.accounts.user.to_account_info(),
                to: ctx.accounts.lobby.to_account_info(),
            },
            &[seeds],
        ),
        lamports,
        data.len() as u64,
        program_id,
    )?;

    ctx.accounts.lobby.to_account_info().data.borrow_mut()[..data.len()].copy_from_slice(&data);

    Ok(())
}
