use anchor_lang::{prelude::*, system_program};
use deform_core::accounts::lobby::{LobbyState, PlayerStatus};
use deform_core::accounts::DeformAccount;

use crate::error::GameProgramError;
use crate::util::deser_and_check_lobby;

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
    let mut lobby = deser_and_check_lobby(lobby_info.clone(), id, *ctx.program_id)?;

    // lobby must not be started
    let LobbyState::NotStarted(not_started) = &mut lobby.state else {
        return Err(GameProgramError::PlayerAlreadyInLobby)?;
    };

    // add the player
    not_started
        .player_status
        .insert(user_key, PlayerStatus::NotReady);

    let new_account = DeformAccount::Lobby(lobby);

    // reserialize
    let new_data =
        wincode::serialize(&new_account).map_err(|_| error!(GameProgramError::SerializeLobby))?;

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
