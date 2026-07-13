use crate::state::UserLogic;
use crate::util::create_pda_account;
use crate::{error::GameProgramError, util::deser_and_check_lobby};
use anchor_lang::prelude::*;
use deform_core::{
    accounts::{
        inputs::InputsAccount,
        lobby::{Lobby, Network},
    },
    DeformUserLogic,
};

#[derive(Accounts)]
pub struct StartGameAccounts<'info> {
    #[account(mut)]
    pub user: Signer<'info>,
    /// CHECK: PDA derived and verified manually because LobbyAccount uses wincode, not borsh.
    #[account(mut)]
    pub lobby: UncheckedAccount<'info>,
    // remaining accounts are an array of (player, inputs), in the same order as players appear in the lobby
}

pub fn handler(ctx: Context<StartGameAccounts>, id: u64) -> Result<()> {
    // let lobby_info = ctx.accounts.lobby.to_account_info();
    // let user_key = *ctx.accounts.user.key;

    // // deser
    // let mut lobby_account =
    //     deser_and_check_lobby(ctx.accounts.lobby.to_account_info(), id, *ctx.program_id)?;

    // // lobby not started
    // require!(
    //     lobby_account.status == LobbyStatus::NotStarted,
    //     GameProgramError::LobbyNotJoinable
    // );

    // // lobby must be in web3 mode, and extract the iner network
    // let web3_network = match lobby_account.network {
    //     Network::FullyOnChain(network) => network,
    //     Network::Web2 => Err(GameProgramError::NotFullyOnChain)?
    // };

    // // TODO: user must be creator?
    // // user in lobby
    // let _player_info = lobby_account
    //     .player_infos
    //     .get_mut(&user_key)
    //     .ok_or_else(|| error!(GameProgramError::PlayerNotInLobby))?;

    // for (_user, user_info) in lobby_account.player_infos.iter() {
    //     // check that all users are ready
    //     require_eq!(user_info.status, PLayerStatus::Ready, GameProgramError::PlayerNotReady);
    // }

    // // FIX: write lobby state, with lobby started set to true, and the initial game state initialized
    // // FIX: delegate lobby and inputs accounts

    // let game_state = UserLogic
    // // lobby_info.data.borrow_mut()[..data.len()].copy_from_slice(&data);

    Ok(())
}
