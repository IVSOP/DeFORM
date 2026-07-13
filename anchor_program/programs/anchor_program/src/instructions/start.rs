use std::collections::HashMap;

use anchor_lang::prelude::*;
use deform_core::{
    accounts::{
        lobby::{started::LobbyOngoing, LobbyState, Network, PlayerStatus},
        DeformAccount,
    },
    DeformUserLogic, TickInfo,
};

use crate::{
    error::GameProgramError,
    state::{Inputs, UserLogic},
    util::{deser_and_check_inputs, deser_and_check_lobby},
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
    let lobby_info = ctx.accounts.lobby.to_account_info();
    let user_key = *ctx.accounts.user.key;

    // deser
    let mut lobby = deser_and_check_lobby(&lobby_info, id, *ctx.program_id)?;

    // lobby not started
    let not_started = match lobby.state {
        LobbyState::NotStarted(not_started) => not_started,
        _ => Err(GameProgramError::LobbyAlreadyStarted)?,
    };

    // lobby must be in web3 mode, and extract the iner network
    let web3_network = match lobby.metadata.network.clone() {
        Network::FullyOnChain(network) => network,
        Network::Web2 => Err(GameProgramError::NotFullyOnChain)?,
    };

    // TODO: user must be creator?
    // user in lobby
    if !not_started.player_status.contains_key(&user_key) {
        Err(GameProgramError::PlayerNotInLobby)?
    };

    let mut inputs = HashMap::new();

    for ((user, user_status), inputs_account) in not_started
        .player_status
        .iter()
        .zip(ctx.remaining_accounts.iter())
    {
        // check that all users are ready
        require_eq!(
            *user_status,
            PlayerStatus::Ready,
            GameProgramError::PlayerNotReady
        );

        // check that all inputs accounts are correct
        let _inputs_account = deser_and_check_inputs(inputs_account, *user, id, *ctx.program_id)?;

        inputs.insert(*user, Inputs::default());
    }

    let user_logic = UserLogic::new_from_lobby(&lobby.metadata, &not_started).map_err(|e| {
        msg!("Error creating user logic: {}", e);
        GameProgramError::InitUserLogic
    })?;
    let game_state =
        UserLogic::new_game_from_lobby(&lobby.metadata, &not_started).map_err(|e| {
            msg!("Error creating game state: {}", e);
            GameProgramError::InitGameState
        })?;

    lobby.state = LobbyState::Ongoing(LobbyOngoing {
        tick: 0,
        tick_info: TickInfo { inputs, game_state },
        user_logic,
    });

    // serialize. account rent should be the same
    {
        let mut data = lobby_info.data.borrow_mut();
        DeformAccount::Lobby(lobby)
            .write_into(&mut data)
            .map_err(|_| error!(GameProgramError::SerializeLobby))?;
    }

    // FIX: delegate lobby and inputs accounts

    Ok(())
}
