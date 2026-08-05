use std::collections::HashMap;

use anchor_lang::prelude::*;
use deform_core::{
    accounts::{
        lobby::{LobbyState, Network},
        DeformAccount,
    },
    DeformUserLogic,
};

use crate::{
    error::GameProgramError,
    state::UserLogic,
    util::{deser_and_check_inputs, deser_and_check_lobby},
    Inputs,
};

#[derive(Accounts)]
pub struct SetInputsAccounts<'info> {
    #[account(mut)]
    pub user: Signer<'info>,
    /// CHECK: PDA derived and verified manually because LobbyAccount uses wincode, not borsh.
    pub lobby: UncheckedAccount<'info>,
    /// CHECK: PDA derived and verified manually because InputsAccount uses wincode, not borsh.
    #[account(mut)]
    pub inputs: UncheckedAccount<'info>,
}

pub fn handler<'info>(
    ctx: Context<'info, SetInputsAccounts<'info>>,
    id: u64,
    batch_inputs_bytes: Vec<u8>,
) -> Result<()> {
    let lobby = deser_and_check_lobby(&ctx.accounts.lobby, id, *ctx.program_id)?;

    let inputs: HashMap<u64, Inputs> =
        wincode::deserialize(&batch_inputs_bytes).map_err(|_| ProgramError::InvalidArgument)?;

    let mut inputs_account = deser_and_check_inputs(
        &ctx.accounts.inputs,
        ctx.accounts.user.key(),
        id,
        *ctx.program_id,
    )?;

    if !matches!(lobby.metadata.network, Network::FullyOnChain(_)) {
        Err(GameProgramError::NotFullyOnChain)?;
    }

    let LobbyState::Ongoing(ongoing) = &lobby.state else {
        return Err(GameProgramError::LobbyNotOngoing)?;
    };

    if !ongoing
        .tick_info
        .inputs
        .contains_key(&ctx.accounts.user.key())
    {
        Err(GameProgramError::PlayerNotInLobby)?;
    }

    for (tick, inputs) in inputs.iter() {
        // if there are too many inputs, just silently exit
        // they will be cleaned up by the next tick invocation
        // TODO: instead remove older values?
        if inputs_account.inputs.len() > UserLogic::MAX_INPUTS as usize {
            break;
        }

        // if value is in the past it gets rejected
        // TODO: <=???
        if *tick < ongoing.tick {
            continue;
        }

        inputs_account.inputs.insert(*tick, inputs.clone());
    }

    // serialize. account rent should be the same
    {
        let mut data = ctx.accounts.inputs.data.borrow_mut();
        DeformAccount::Inputs(inputs_account)
            .write_into(&mut data)
            .map_err(|_| error!(GameProgramError::SerializeInputsAccount))?;
    }

    Ok(())
}
