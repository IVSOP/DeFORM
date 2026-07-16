use std::collections::BTreeMap;

use anchor_lang::prelude::*;
use deform_core::{
    accounts::{
        inputs::InputsAccount,
        lobby::{ongoing::LobbyOngoing, LobbyFinished, LobbyState, Network},
        DeformAccount,
    },
    DeformGameState, DeformUserLogic,
};

use crate::{
    error::GameProgramError,
    state::UserLogic,
    util::{deser_and_check_inputs, deser_and_check_lobby},
};

#[derive(Accounts)]
pub struct TickAccounts<'info> {
    // No signer: `tick` runs unattended as a scheduled crank/task on the ephemeral
    // rollup, where the magic-program crank executor — not a user — drives it. The
    // handler never needed a signer identity anyway; it only touches `lobby` and the
    // inputs accounts passed as remaining accounts, all already delegated to the ER.
    /// CHECK: PDA derived and verified manually because LobbyAccount uses wincode, not borsh.
    #[account(mut)]
    pub lobby: UncheckedAccount<'info>,
    // remaining accounts are all of the inputs in order of the users that are in the lobby
}

pub fn handler<'info>(ctx: Context<'info, TickAccounts<'info>>, id: u64) -> Result<()> {
    let mut lobby = deser_and_check_lobby(&ctx.accounts.lobby, id, *ctx.program_id)?;

    let Network::FullyOnChain(network) = &lobby.metadata.network else {
        return Err(GameProgramError::NotFullyOnChain)?;
    };

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

    if slot_delta > 0 {
        if ctx.remaining_accounts.len() != ongoing.tick_info.inputs.len() {
            Err(ProgramError::NotEnoughAccountKeys)?;
        }

        let mut inputs_infos: BTreeMap<Pubkey, InputsAccount<UserLogic>> = BTreeMap::new();
        for (player, inputs_account) in ongoing
            .tick_info
            .inputs
            .keys()
            .zip(ctx.remaining_accounts.iter())
        {
            let mut inputs_info =
                deser_and_check_inputs(inputs_account, *player, id, *ctx.program_id)?;

            // we can cleanup all old inputs now, we will only be using inputs that are more recent than this
            inputs_info.inputs.retain(|tick, _| *tick >= ongoing.tick);

            inputs_infos.insert(*player, inputs_info);
        }

        // Elapsed real time is `slot_delta * micros_per_slot`; run one game tick per
        // `TICK_RATE_MICROS` of it so the on-chain sim keeps pace with the slot clock
        // (e.g. a 50ms devnet slot = 3 ticks at 60Hz). Always at least one tick.
        let micros_per_slot = UserLogic::get_micros_per_slot(network);
        let num_ticks = (slot_delta * micros_per_slot
            / <UserLogic as DeformUserLogic>::TICK_RATE_MICROS)
            .max(1);

        // Run the simulation, threading the owned `LobbyOngoing` through each tick.
        // Once `advance_tick` returns `Finished`, the `let ... else` breaks so we never
        // tick a finished lobby again.
        let mut lobby_state = LobbyState::Ongoing(ongoing);
        for _ in 0..num_ticks {
            let LobbyState::Ongoing(ongoing) = lobby_state else {
                break;
            };
            lobby_state = advance_tick(ongoing, &mut inputs_infos)?;
        }
        lobby.state = lobby_state;

        // reserialize lobby
        {
            let mut data = ctx.accounts.lobby.data.borrow_mut();
            DeformAccount::Lobby(lobby)
                .write_into(&mut data)
                .map_err(|_| error!(GameProgramError::SerializeLobby))?;
        }

        // Order matches how `inputs_infos` was built: both `inputs_infos` (a BTreeMap
        // keyed by player) and `remaining_accounts` follow the sorted-pubkey order of
        // `tick_info.inputs`, so consuming the values realigns them with their accounts.
        for (inputs_info, inputs_account) in inputs_infos
            .into_values()
            .zip(ctx.remaining_accounts.iter())
        {
            {
                let mut data = inputs_account.data.borrow_mut();
                DeformAccount::Inputs(inputs_info)
                    .write_into(&mut data)
                    .map_err(|_| error!(GameProgramError::SerializeInputsAccount))?;
            }
        }
    }

    Ok(())
}

/// Advance the lobby by a single tick: apply this tick's inputs, run the frame, bump
/// the tick counter, and return the resulting state (`Ongoing` or `Finished`).
///
/// Named `advance_tick` rather than `tick` so it doesn't collide with the `tick`
/// instruction that `#[program]` re-exports at the crate root.
pub fn advance_tick(
    mut ongoing: LobbyOngoing<UserLogic>,
    inputs_infos: &mut BTreeMap<Pubkey, InputsAccount<UserLogic>>,
) -> Result<LobbyState<UserLogic>> {
    let current_tick = ongoing.tick;
    for (player, current_inputs) in ongoing.tick_info.inputs.iter_mut() {
        let inputs_info = inputs_infos.get_mut(player).unwrap();

        if let Some(new_inputs) = inputs_info.inputs.remove(&current_tick) {
            *current_inputs = new_inputs.clone();
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

    if ongoing.tick_info.game_state.has_ended() {
        Ok(LobbyState::Finished(LobbyFinished(ongoing)))
    } else {
        Ok(LobbyState::Ongoing(ongoing))
    }
}
