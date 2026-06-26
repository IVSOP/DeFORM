use std::collections::{HashMap, HashSet};

use anchor_lang::{prelude::*, system_program};
use deform_core::{
    lobby::{LobbyData, LobbyStatus, PLayerStatus, PlayerInfo},
    DeformGameState,
};

use crate::{error::GameError, state::*};

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
    let (pda, bump) = LobbyAccount::find_program_address(id, program_id);
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

    let mut users = HashSet::new();
    users.insert(creator);

    let lobby_account = LobbyAccount {
        account_type: AccountTypes::Lobby,
        bump,
        lobby: LobbyData::<UserLogic> {
            id,
            tick: 0,
            creator,
            status: LobbyStatus::NotStarted,
            game_state: GameState::new(&users),
            player_infos,
        },
    };

    // serialize into a Vec
    let data = wincode::serialize(&lobby_account).map_err(|_| error!(GameError::SerializeLobby))?;

    let rent = Rent::get()?;
    let lamports = rent.minimum_balance(data.len());

    let seeds: &[&[u8]] = &[b"lobby", &id.to_le_bytes(), &[bump]];
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
