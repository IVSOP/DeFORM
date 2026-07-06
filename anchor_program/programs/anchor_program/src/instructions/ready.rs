use crate::error::GameProgramError;
use crate::state::UserLogic;
use anchor_lang::{prelude::*, system_program};
use deform_core::accounts::{
    inputs::InputsAccount,
    lobby::{Lobby, LobbyStatus, PLayerStatus},
    AccountType,
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

pub fn handler(ctx: Context<ReadyAccounts>, id: u64, fully_onchain: bool) -> Result<()> {
    let lobby_info = ctx.accounts.lobby.to_account_info();
    let user_key = *ctx.accounts.user.key;

    // deser
    let mut lobby_account = {
        let data = lobby_info.data.borrow();
        Lobby::<UserLogic>::from_bytes(&data)
            .map_err(|_| error!(GameProgramError::DeserializeLobby))?
    };

    // check account type
    match lobby_account.account_type {
        AccountType::Lobby => {}
        _ => return Err(error!(GameProgramError::InvalidAccountType)),
    }

    // check pda
    let pda = Lobby::<UserLogic>::create_program_address(id, &ctx.program_id, lobby_account.bump)
        .map_err(|_| ProgramError::InvalidSeeds)?;
    require_keys_eq!(lobby_info.key(), pda, GameProgramError::InvalidPda);

    // lobby not started
    require!(
        lobby_account.status == LobbyStatus::NotStarted,
        GameProgramError::LobbyNotJoinable
    );

    // user in lobby
    let player_info = lobby_account
        .player_infos
        .get_mut(&user_key)
        .ok_or_else(|| error!(GameProgramError::PlayerNotInLobby))?;

    // user not ready
    require!(
        player_info.status == PLayerStatus::NotReady,
        GameProgramError::PlayerAlreadyReady
    );

    player_info.status = PLayerStatus::Ready;

    // serialize. account rent should be the same
    {
        let mut data = lobby_info.data.borrow_mut();
        lobby_account
            .write_into(&mut data)
            .map_err(|_| error!(GameProgramError::SerializeLobby))?;
    }

    if fully_onchain {
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
        let inputs_account = InputsAccount::<UserLogic>::new(id, user_key, inputs_bump);
        let inputs_data = wincode::serialize(&inputs_account)
            .map_err(|_| error!(GameProgramError::SerializeInputsAccount))?;

        let rent = Rent::get()?;
        let lamports = rent.minimum_balance(inputs_data.len());

        let seeds: &[&[u8]] = &[
            b"inputs",
            &id.to_le_bytes(),
            user_key.as_array(),
            &[inputs_bump],
        ];
        system_program::create_account(
            CpiContext::new_with_signer(
                ctx.accounts.system_program.key(),
                system_program::CreateAccount {
                    from: ctx.accounts.user.to_account_info(),
                    to: inputs_info.clone(),
                },
                &[seeds],
            ),
            lamports,
            inputs_data.len() as u64,
            ctx.program_id,
        )?;

        inputs_info.data.borrow_mut()[..inputs_data.len()].copy_from_slice(&inputs_data);
    }

    Ok(())
}
