use anchor_lang::prelude::*;

use crate::{constants::ADMIN, error::GameProgramError, instructions::close_account};

/// WARN: DO NOT DEPLOY THIS INSTRUCTION. Closes any account, with no checks at all.
#[derive(Accounts)]
pub struct ForceCloseAccounts<'info> {
    #[account(mut, address = ADMIN @ GameProgramError::Unauthorized)]
    pub admin: Signer<'info>,
    /// CHECK: deliberately unchecked.
    #[account(mut)]
    pub account: UncheckedAccount<'info>,
}

pub fn handler(ctx: Context<ForceCloseAccounts>) -> Result<()> {
    close_account(
        &ctx.accounts.account.to_account_info(),
        &ctx.accounts.admin.to_account_info(),
    )
}
