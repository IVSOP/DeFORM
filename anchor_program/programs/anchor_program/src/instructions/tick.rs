use anchor_lang::prelude::*;
use deform_core::{
    accounts::{
        lobby::{LobbyFinished, LobbyState, Network},
        DeformAccount,
    },
    DeformGameState, DeformUserLogic,
};

use crate::{
    error::GameProgramError,
    util::{deser_and_check_inputs, deser_and_check_lobby},
};

#[derive(Accounts)]
pub struct TickAccounts<'info> {
    #[account(mut)]
    pub user: Signer<'info>,
    /// CHECK: PDA derived and verified manually because LobbyAccount uses wincode, not borsh.
    #[account(mut)]
    pub lobby: UncheckedAccount<'info>,
    // remaining accounts are all of the inputs in order of the users that are in the lobby
}

pub fn handler<'info>(ctx: Context<'info, TickAccounts<'info>>, id: u64) -> Result<()> {
    let mut lobby = deser_and_check_lobby(&ctx.accounts.lobby, id, *ctx.program_id)?;

    if !matches!(lobby.metadata.network, Network::FullyOnChain(_)) {
        Err(GameProgramError::NotFullyOnChain)?;
    }

    // WARN: this makes lobby.state get moved out, and it needs to be placed back to be updated
    // done on purpose to easily change it to LobbyState::Finished
    let LobbyState::Ongoing(mut ongoing) = lobby.state else {
        return Err(GameProgramError::LobbyNotOngoing)?;
    };

    let current_slot = Clock::get()?.slot;

    let slot_delta = match ongoing.slot {
        Some(slot) => current_slot - slot,
        None => 1,
    };
    ongoing.slot = Some(current_slot);

    // TODO: in the future, might want to run N times for all the slots we have missed
    if slot_delta > 0 {
        if ctx.remaining_accounts.len() != ongoing.tick_info.inputs.len() {
            Err(ProgramError::NotEnoughAccountKeys)?;
        }

        let current_tick = ongoing.tick;
        for ((player, current_inputs), inputs_account) in ongoing
            .tick_info
            .inputs
            .iter_mut()
            .zip(ctx.remaining_accounts.iter())
        {
            let mut inputs_info =
                deser_and_check_inputs(inputs_account, *player, id, *ctx.program_id)?;

            if let Some(new_inputs) = inputs_info.inputs.get(&current_tick) {
                *current_inputs = new_inputs.clone();
            }

            // cleanup all inputs
            inputs_info.inputs.retain(|tick, _| *tick > current_tick);

            // reserialize inputs account. account rent should be the same
            {
                let mut data = inputs_account.data.borrow_mut();
                DeformAccount::Inputs(inputs_info)
                    .write_into(&mut data)
                    .map_err(|_| error!(GameProgramError::SerializeInputsAccount))?;
            }
        }

        let user_logic = &mut ongoing.user_logic;
        // ongoing.tick_info.inputs has the latest inputs already, this is confusing but faster
        let new_game_state = user_logic
            .advance_frame(&ongoing.tick_info.game_state, &ongoing.tick_info.inputs)
            .map_err(|e| {
                msg!("Error advancing frame: {}", e.to_string());
                GameProgramError::AdvanceFrame
            })?;

        ongoing.tick_info.game_state = new_game_state;

        // Advance the tick only after the frame has been simulated. This mirrors the
        // off-chain simulations (deform_offline / deform_quic server match loop),
        // which read inputs at `current_tick`, advance the frame, then increment.
        ongoing.tick += 1;

        lobby.state = if ongoing.tick_info.game_state.has_ended() {
            LobbyState::Finished(LobbyFinished(ongoing))
        } else {
            LobbyState::Ongoing(ongoing)
        };

        {
            let mut data = ctx.accounts.lobby.data.borrow_mut();
            DeformAccount::Lobby(lobby)
                .write_into(&mut data)
                .map_err(|_| error!(GameProgramError::SerializeLobby))?;
        }
    }

    Ok(())
}
