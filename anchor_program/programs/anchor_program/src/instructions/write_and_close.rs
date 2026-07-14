use anchor_lang::prelude::*;
use deform_core::accounts::lobby::Network;

use crate::{constants::ADMIN, error::GameProgramError, util::deser_and_check_lobby};

#[derive(AnchorSerialize, AnchorDeserialize)]
pub struct PlayerScore {
    pub player: Pubkey,
    pub score: u32,
}

#[derive(Accounts)]
pub struct WriteAndCloseAccounts<'info> {
    #[account(mut, address = ADMIN @ GameProgramError::Unauthorized)]
    pub admin: Signer<'info>,
    /// CHECK: PDA derived and verified manually because LobbyAccount uses wincode, not borsh.
    #[account(mut)]
    pub lobby: UncheckedAccount<'info>,
    /// CHECK: Must match the creator stored in the lobby.
    #[account(mut)]
    pub creator: UncheckedAccount<'info>,
}

pub fn handler(
    ctx: Context<WriteAndCloseAccounts>,
    id: u64,
    _scores: Vec<PlayerScore>,
) -> Result<()> {
    let lobby_info = ctx.accounts.lobby.to_account_info();

    let lobby_account = deser_and_check_lobby(&lobby_info, id, *ctx.program_id)?;

    if !matches!(lobby_account.metadata.network, Network::Web2) {
        Err(GameProgramError::NotWeb2)?;
    }

    require_keys_eq!(
        ctx.accounts.creator.key(),
        lobby_account.metadata.creator,
        GameProgramError::CreatorMismatch
    );

    let creator_info = ctx.accounts.creator.to_account_info();
    let lamports = lobby_info.lamports();

    **lobby_info.try_borrow_mut_lamports()? = 0;
    **creator_info.try_borrow_mut_lamports()? = creator_info
        .lamports()
        .checked_add(lamports)
        .ok_or(ProgramError::ArithmeticOverflow)?;

    lobby_info.assign(&System::id());
    lobby_info.resize(0)?;

    Ok(())
}
