use anchor_lang::prelude::*;

use crate::constants::ADMIN;
use crate::error::GameError;
use crate::state::*;

#[derive(AnchorSerialize, AnchorDeserialize)]
pub struct PlayerScore {
    pub player: Pubkey,
    pub score: u32,
}

#[derive(Accounts)]
pub struct WriteAndCloseAccounts<'info> {
    #[account(mut, address = ADMIN @ GameError::Unauthorized)]
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

    let lobby_account = {
        let data = lobby_info.data.borrow();
        LobbyAccount::from_bytes(&data).map_err(|_| error!(GameError::DeserializeLobby))?
    };

    match lobby_account.account_type {
        AccountTypes::Lobby => {}
        _ => return Err(error!(GameError::InvalidAccountType)),
    }

    let pda = LobbyAccount::create_program_address(id, lobby_account.bump, ctx.program_id)?;
    require_keys_eq!(lobby_info.key(), pda, GameError::InvalidPda);

    require_keys_eq!(
        ctx.accounts.creator.key(),
        lobby_account.lobby.creator,
        GameError::CreatorMismatch
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
