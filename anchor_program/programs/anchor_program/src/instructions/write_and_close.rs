use anchor_lang::prelude::*;
use deform_core::accounts::lobby::Lobby;

use crate::constants::ADMIN;
use crate::error::GameProgramError;
use crate::state::*;
use crate::util::deser_and_check_lobby;

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

    let lobby_account =
        deser_and_check_lobby(ctx.accounts.lobby.to_account_info(), id, *ctx.program_id)?;

    let pda = Lobby::<UserLogic>::create_program_address(id, &ctx.program_id, lobby_account.bump)
        .map_err(|_| ProgramError::InvalidSeeds)?;
    require_keys_eq!(lobby_info.key(), pda, GameProgramError::InvalidPda);

    require_keys_eq!(
        ctx.accounts.creator.key(),
        lobby_account.creator,
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
