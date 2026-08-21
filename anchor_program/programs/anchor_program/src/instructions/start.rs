use std::collections::BTreeMap;

use anchor_lang::prelude::*;
use deform_core::{
    accounts::{
        lobby::{ongoing::LobbyOngoing, LobbyState, Network, PlayerStatus},
        DeformAccount,
    },
    DeformUserLogic, TickInfo,
};
use ephemeral_rollups_sdk::cpi::{delegate_account, DelegateAccounts, DelegateConfig};

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

    // --- ephemeral-rollups delegation plumbing for the lobby ---
    /// CHECK: this program, passed as the delegated account's owner program.
    #[account(address = crate::ID)]
    pub owner_program: UncheckedAccount<'info>,
    /// CHECK: lobby delegation buffer PDA, validated by the delegation CPI via its seeds.
    #[account(mut)]
    pub lobby_buffer: UncheckedAccount<'info>,
    /// CHECK: lobby delegation record, validated by the delegation program.
    #[account(mut)]
    pub lobby_delegation_record: UncheckedAccount<'info>,
    /// CHECK: lobby delegation metadata, validated by the delegation program.
    #[account(mut)]
    pub lobby_delegation_metadata: UncheckedAccount<'info>,
    /// CHECK: the delegation program.
    #[account(address = ephemeral_rollups_sdk::id())]
    pub delegation_program: UncheckedAccount<'info>,
    pub system_program: Program<'info, System>,
    // remaining accounts are grouped per player, in the same order players appear in the
    // lobby: [inputs, inputs_buffer, inputs_delegation_record, inputs_delegation_metadata]
}

pub fn handler<'info>(ctx: Context<'info, StartGameAccounts<'info>>, id: u64) -> Result<()> {
    let lobby_info = ctx.accounts.lobby.to_account_info();
    let payer = ctx.accounts.user.to_account_info();
    let owner_program = ctx.accounts.owner_program.to_account_info();
    let system_program = ctx.accounts.system_program.to_account_info();
    let delegation_program = ctx.accounts.delegation_program.to_account_info();
    let lobby_buffer = ctx.accounts.lobby_buffer.to_account_info();
    let lobby_delegation_record = ctx.accounts.lobby_delegation_record.to_account_info();
    let lobby_delegation_metadata = ctx.accounts.lobby_delegation_metadata.to_account_info();

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
        Network::Web2(_) => Err(GameProgramError::NotFullyOnChain)?,
    };
    // every account (lobby + all inputs) is pinned to this validator so tick/crank
    // txs on the ER can touch them together.
    let validator = web3_network.address();

    // TODO: user must be creator?
    // user in lobby
    if !not_started.player_status.contains_key(&user_key) {
        Err(GameProgramError::PlayerNotInLobby)?
    };

    let mut inputs = BTreeMap::new();

    // one inputs account plus its three delegation companion accounts per player
    const ACCOUNTS_PER_PLAYER: usize = 4;
    if ctx.remaining_accounts.len() != not_started.player_status.len() * ACCOUNTS_PER_PLAYER {
        // TODO: improve error
        Err(GameProgramError::MissingInputsAccount)?;
    }

    for ((user, user_status), accounts) in not_started
        .player_status
        .iter()
        .zip(ctx.remaining_accounts.chunks_exact(ACCOUNTS_PER_PLAYER))
    {
        // check that all users are ready
        require_eq!(
            *user_status,
            PlayerStatus::Ready,
            GameProgramError::PlayerNotReady
        );

        let [inputs_account, inputs_buffer, inputs_delegation_record, inputs_delegation_metadata] =
            accounts
        else {
            return Err(GameProgramError::MissingInputsAccount.into());
        };

        // check that all inputs accounts are correct. Must happen before delegation,
        // which zeroes the account data.
        deser_and_check_inputs(inputs_account, *user, id, *ctx.program_id)?;

        inputs.insert(*user, Inputs::default());

        // Delegate this player's inputs account, pinned to the same validator as the
        // lobby so tick/crank txs can touch the lobby and every inputs account together.
        delegate_account(
            DelegateAccounts {
                payer: &payer,
                pda: inputs_account,
                owner_program: &owner_program,
                buffer: inputs_buffer,
                delegation_record: inputs_delegation_record,
                delegation_metadata: inputs_delegation_metadata,
                delegation_program: &delegation_program,
                system_program: &system_program,
            },
            &[b"inputs", &id.to_le_bytes(), user.as_array()],
            DelegateConfig {
                commit_frequency_ms: u32::MAX,
                validator: Some(validator),
            },
        )
        .map_err(|e| {
            msg!("Error delegating inputs account: {:?}", e);
            GameProgramError::DelegateInputs
        })?;
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
        slot: None, // we can only set it once we are in the other validator
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

    // Delegate the lobby to the ephemeral rollup. This must be the last thing we do
    // to the account: delegation zeroes its data and reassigns ownership to the
    // delegation program, so no further writes are possible afterwards.
    delegate_account(
        DelegateAccounts {
            payer: &payer,
            pda: &lobby_info,
            owner_program: &owner_program,
            buffer: &lobby_buffer,
            delegation_record: &lobby_delegation_record,
            delegation_metadata: &lobby_delegation_metadata,
            delegation_program: &delegation_program,
            system_program: &system_program,
        },
        &[b"lobby", &id.to_le_bytes()],
        DelegateConfig {
            commit_frequency_ms: u32::MAX,
            validator: Some(validator),
        },
    )
    .map_err(|e| {
        msg!("Error delegating lobby: {:?}", e);
        GameProgramError::DelegateLobby
    })?;

    Ok(())
}
