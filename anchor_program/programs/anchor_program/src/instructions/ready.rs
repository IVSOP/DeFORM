use anchor_lang::prelude::*;
use deform_core::lobby::{LobbyStatus, PLayerStatus};

use crate::error::GameError;
use crate::state::{AccountTypes, LobbyAccount};

#[derive(Accounts)]
pub struct ReadyAccounts<'info> {
    pub user: Signer<'info>,
    /// CHECK: PDA derived and verified manually because LobbyAccount uses wincode, not borsh.
    #[account(mut)]
    pub lobby: UncheckedAccount<'info>,
}

pub fn handler(ctx: Context<ReadyAccounts>, id: u64) -> Result<()> {
    let lobby_info = ctx.accounts.lobby.to_account_info();
    let user_key = *ctx.accounts.user.key;

    // deser
    let mut lobby_account = {
        let data = lobby_info.data.borrow();
        LobbyAccount::from_bytes(&data).map_err(|_| error!(GameError::DeserializeLobby))?
    };

    // check account type
    match lobby_account.account_type {
        AccountTypes::Lobby => {}
        _ => return Err(error!(GameError::InvalidAccountType)),
    }

    // check pda
    let pda = LobbyAccount::create_program_address(id, lobby_account.bump, ctx.program_id)?;
    require_keys_eq!(lobby_info.key(), pda, GameError::InvalidPda);

    // lobby not started
    require!(
        lobby_account.lobby.status == LobbyStatus::NotStarted,
        GameError::LobbyNotJoinable
    );

    // user in lobby
    let player_info = lobby_account
        .lobby
        .player_infos
        .get_mut(&user_key)
        .ok_or_else(|| error!(GameError::PlayerNotInLobby))?;

    // user not ready
    require!(
        player_info.status == PLayerStatus::NotReady,
        GameError::PlayerAlreadyReady
    );

    player_info.status = PLayerStatus::Ready;

    // serialize. account rent should be the same
    let mut data = lobby_info.data.borrow_mut();
    lobby_account
        .write_into(&mut data)
        .map_err(|_| error!(GameError::SerializeLobby))?;

    Ok(())
}
