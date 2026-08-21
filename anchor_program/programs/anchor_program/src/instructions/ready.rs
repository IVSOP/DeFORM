use anchor_lang::prelude::*;
use deform_core::{
    accounts::{
        inputs::InputsAccount,
        lobby::{LobbyState, Network, PlayerStatus},
        DeformAccount,
    },
    DeformUserLogic,
};

use crate::{
    error::GameProgramError,
    state::UserLogic,
    util::{create_pda_account, deser_and_check_lobby},
};

#[derive(Accounts)]
pub struct ReadyAccounts<'info> {
    #[account(mut)]
    pub user: Signer<'info>,
    /// CHECK: PDA derived and verified manually because LobbyAccount uses wincode, not borsh.
    #[account(mut)]
    pub lobby: UncheckedAccount<'info>,
    /// CHECK: PDA derived and verified manually because InputsAccount uses wincode, not borsh.
    #[account(mut)]
    pub inputs: Option<UncheckedAccount<'info>>,
    pub system_program: Program<'info, System>,
}

pub fn handler(ctx: Context<ReadyAccounts>, id: u64) -> Result<()> {
    let lobby_info = ctx.accounts.lobby.to_account_info();
    let user_key = *ctx.accounts.user.key;

    // deser
    let mut lobby = deser_and_check_lobby(&lobby_info, id, *ctx.program_id)?;

    // lobby not started
    let LobbyState::NotStarted(not_started) = &mut lobby.state else {
        return Err(GameProgramError::LobbyNotJoinable)?;
    };

    // user in lobby
    let player_status = not_started
        .player_status
        .get_mut(&user_key)
        .ok_or_else(|| error!(GameProgramError::PlayerNotInLobby))?;

    // user must not be ready
    require!(
        *player_status == PlayerStatus::NotReady,
        GameProgramError::PlayerAlreadyReady
    );

    // set ready
    *player_status = PlayerStatus::Ready;

    if !matches!(lobby.metadata.network, Network::Web2(_)) {
        // player inputs account
        let inputs_info = ctx
            .accounts
            .inputs
            .as_ref()
            .ok_or_else(|| error!(GameProgramError::MissingInputsAccount))?
            .to_account_info();

        // 1) must be uninitialized (still owned by the system program, no data)
        require!(
            inputs_info.data_is_empty() && inputs_info.owner == &ctx.accounts.system_program.key(),
            GameProgramError::InputsAccountAlreadyInitialized
        );

        // 2) check pda
        let (pda, inputs_bump) =
            InputsAccount::<UserLogic>::find_program_address(id, &user_key, ctx.program_id);
        require_keys_eq!(inputs_info.key(), pda, GameProgramError::InvalidPda);

        // 3) create the account, initialize and serialize it
        let inputs_account =
            DeformAccount::Inputs(InputsAccount::<UserLogic>::new(id, user_key, inputs_bump));
        let inputs_data = wincode::serialize(&inputs_account)
            .map_err(|_| error!(GameProgramError::SerializeInputsAccount))?;

        // TODO: this should be a call to PlayerInputs
        let seeds: &[&[u8]] = &[
            b"inputs",
            &id.to_le_bytes(),
            user_key.as_array(),
            &[inputs_bump],
        ];
        create_pda_account(
            &ctx.accounts.user.to_account_info(),
            &inputs_info,
            ctx.accounts.system_program.key(),
            ctx.program_id,
            // create account already using the max space
            UserLogic::MAX_INPUTS_ACCOUNT_BYTES,
            seeds,
        )?;

        inputs_info.data.borrow_mut()[..inputs_data.len()].copy_from_slice(&inputs_data);
    }

    // serialize. account rent should be the same
    {
        let mut data = lobby_info.data.borrow_mut();
        DeformAccount::Lobby(lobby)
            .write_into(&mut data)
            .map_err(|_| error!(GameProgramError::SerializeLobby))?;
    }

    Ok(())
}
