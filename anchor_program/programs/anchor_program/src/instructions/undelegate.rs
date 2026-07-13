use anchor_lang::prelude::*;
use deform_core::accounts::lobby::{LobbyFinished, LobbyState};
use ephemeral_rollups_sdk::{
    consts::{MAGIC_CONTEXT_ID, MAGIC_PROGRAM_ID},
    cpi::undelegate_account,
    ephem::{FoldableIntentBuilder, MagicIntentBundleBuilder},
};

use crate::{
    error::GameProgramError,
    util::{deser_and_check_inputs, deser_and_check_lobby},
};

/// ER-side trigger. Runs on the ephemeral rollup: commits the final state of the
/// lobby and every inputs account back to the base layer and undelegates them.
///
/// This schedules the commit+undelegate intent with the magic program; the actual
/// undelegation is finalized on base layer by the delegation program, which calls
/// back into [`process_undelegation`] (once per account).
#[derive(Accounts)]
pub struct UndelegateAccounts<'info> {
    #[account(mut)]
    pub payer: Signer<'info>,
    /// CHECK: delegated PDA; ownership/seeds are enforced by the delegation program.
    #[account(mut)]
    pub lobby: UncheckedAccount<'info>,
    /// CHECK: magic program context, written by the magic program.
    #[account(mut, address = MAGIC_CONTEXT_ID)]
    pub magic_context: UncheckedAccount<'info>,
    /// CHECK: the magic program.
    #[account(address = MAGIC_PROGRAM_ID)]
    pub magic_program: UncheckedAccount<'info>,
    // remaining accounts are the delegated inputs accounts to undelegate
}

pub fn handler<'info>(ctx: Context<'info, UndelegateAccounts<'info>>, id: u64) -> Result<()> {
    let lobby_info = deser_and_check_lobby(&ctx.accounts.lobby, id, *ctx.program_id)?;

    let players: Vec<Pubkey> = match lobby_info.state {
        LobbyState::NotStarted(not_started) => not_started.player_status.keys().copied().collect(),
        LobbyState::Ongoing(ongoing) => ongoing.tick_info.inputs.keys().copied().collect(),
        LobbyState::Finished(LobbyFinished(finished)) => {
            finished.tick_info.inputs.keys().copied().collect()
        }
    };
    for (player, inputs_account) in players.iter().zip(ctx.remaining_accounts.iter()) {
        deser_and_check_inputs(inputs_account, *player, id, *ctx.program_id)?;
    }

    // lobby first, then every inputs account passed in remaining_accounts
    let mut committed = Vec::with_capacity(1 + ctx.remaining_accounts.len());
    committed.push(ctx.accounts.lobby.to_account_info());
    committed.extend(ctx.remaining_accounts.iter().cloned());

    MagicIntentBundleBuilder::new(
        ctx.accounts.payer.to_account_info(),
        ctx.accounts.magic_context.to_account_info(),
        ctx.accounts.magic_program.to_account_info(),
    )
    .commit_and_undelegate(&committed)
    .build_and_invoke()
    .map_err(|e| {
        msg!("Error committing/undelegating accounts: {:?}", e);
        GameProgramError::Undelegate
    })?;

    Ok(())
}

/// Base-layer callback. The delegation program CPIs into this instruction once per
/// undelegated account to re-create the PDA (owned by us again) with its committed
/// data.
///
/// The name MUST stay `process_undelegation`: Anchor derives its 8-byte discriminator
/// as `sha256("global:process_undelegation")[..8]`, which equals the delegation
/// program's `EXTERNAL_UNDELEGATE_DISCRIMINATOR`. Renaming it breaks the callback.
///
/// `account_seeds` are the original PDA seeds (without bump) that the delegation
/// program stored at delegation time and replays here, so this single handler works
/// for the lobby and every inputs account.
pub fn process_undelegation_handler(
    ctx: Context<InitializeAfterUndelegation>,
    account_seeds: Vec<Vec<u8>>,
) -> Result<()> {
    undelegate_account(
        &ctx.accounts.base_account.to_account_info(),
        ctx.program_id,
        &ctx.accounts.buffer.to_account_info(),
        &ctx.accounts.payer.to_account_info(),
        &ctx.accounts.system_program.to_account_info(),
        account_seeds,
    )
    .map_err(|e| {
        msg!("Error re-creating undelegated account: {:?}", e);
        GameProgramError::Undelegate
    })?;

    Ok(())
}

/// Accounts for the `process_undelegation` callback. Order is fixed by the
/// delegation program's CPI: `[base_account, buffer, payer, system_program]`.
#[derive(Accounts)]
pub struct InitializeAfterUndelegation<'info> {
    /// CHECK: PDA being re-created; validated via `account_seeds` inside `undelegate_account`.
    #[account(mut)]
    pub base_account: UncheckedAccount<'info>,
    /// CHECK: delegation-program-owned buffer holding the committed data (signer set by DLP).
    pub buffer: UncheckedAccount<'info>,
    /// CHECK: payer funding the re-created account.
    #[account(mut)]
    pub payer: UncheckedAccount<'info>,
    /// CHECK: system program.
    pub system_program: UncheckedAccount<'info>,
}
