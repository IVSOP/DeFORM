use anchor_lang::prelude::*;
use deform_core::accounts::{
    lobby::{LobbyState, PlayerStatus},
    DeformAccount,
};

use crate::{error::GameProgramError, util::deser_and_check_lobby};

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
    let mut lobby = deser_and_check_lobby(&lobby_info, id, *ctx.program_id)?;

    // lobby must not be started
    let LobbyState::NotStarted(not_started) = &mut lobby.state else {
        return Err(GameProgramError::PlayerAlreadyInLobby)?;
    };

    // add the player
    not_started
        .player_status
        .insert(user_key, PlayerStatus::NotReady);

    // serialize. account rent should be the same
    {
        let mut data = lobby_info.data.borrow_mut();
        DeformAccount::Lobby(lobby)
            .write_into(&mut data)
            .map_err(|_| error!(GameProgramError::SerializeLobby))?;
    }

    Ok(())
}
