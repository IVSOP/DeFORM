use anchor_lang::{prelude::*, system_program};
use deform_core::accounts::lobby::{Lobby, LobbyStatus, PLayerStatus, PlayerInfo};
use deform_core::accounts::AccountType;

use crate::error::GameError;
use crate::state::*;

#[derive(Accounts)]
pub struct JoinLobbyAccounts<'info> {
    #[account(mut)]
    pub user: Signer<'info>,
    /// CHECK: PDA derived and verified manually because LobbyAccount uses wincode, not borsh.
    #[account(mut)]
    pub lobby: UncheckedAccount<'info>,
    pub system_program: Program<'info, System>,
}

pub fn handler(ctx: Context<JoinLobbyAccounts>, id: u64) -> Result<()> {
    let lobby_info = ctx.accounts.lobby.to_account_info();
    let user_key = *ctx.accounts.user.key;

    // deser
    let mut lobby_account = {
        let data = lobby_info.data.borrow();
        Lobby::<UserLogic>::from_bytes(&data).map_err(|_| error!(GameError::DeserializeLobby))?
    };

    // check account type
    match lobby_account.account_type {
        AccountType::Lobby => {}
        _ => return Err(error!(GameError::InvalidAccountType)),
    }

    // check pda
    let pda = Lobby::<UserLogic>::create_program_address(id, &ctx.program_id, lobby_account.bump)
        .map_err(|_| ProgramError::InvalidSeeds)?;
    require_keys_eq!(lobby_info.key(), pda, GameError::InvalidPda);

    // lobby must not be started
    require!(
        lobby_account.status == LobbyStatus::NotStarted,
        GameError::LobbyNotJoinable
    );
    // player must not already be in lobby
    require!(
        !lobby_account.player_infos.contains_key(&user_key),
        GameError::PlayerAlreadyInLobby
    );

    // add the player
    lobby_account.player_infos.insert(
        user_key,
        PlayerInfo {
            status: PLayerStatus::NotReady,
            inputs: Inputs::default(),
        },
    );

    // reserialize
    let new_data =
        wincode::serialize(&lobby_account).map_err(|_| error!(GameError::SerializeLobby))?;

    let new_len = new_data.len();
    let old_len = lobby_info.data_len();

    if new_len > old_len {
        lobby_info.resize(new_len)?;

        let rent = Rent::get()?;
        let new_min = rent.minimum_balance(new_len);
        let deficit = new_min.checked_sub(lobby_info.lamports()).unwrap_or(0);
        if deficit > 0 {
            system_program::transfer(
                CpiContext::new(
                    ctx.accounts.system_program.key(),
                    system_program::Transfer {
                        from: ctx.accounts.user.to_account_info(),
                        to: lobby_info.clone(),
                    },
                ),
                deficit,
            )?;
        }
    }

    lobby_info.data.borrow_mut()[..new_len].copy_from_slice(&new_data);

    Ok(())
}
