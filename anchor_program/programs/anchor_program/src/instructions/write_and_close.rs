use anchor_lang::prelude::*;
use deform_core::accounts::lobby::{LobbyFinished, LobbyState, Network};

use crate::{
    constants::ADMIN,
    error::GameProgramError,
    util::{deser_and_check_inputs, deser_and_check_lobby},
};

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
    // in FullyOnChain mode, remaining accounts are grouped per player, in the same order
    // players appear in the lobby: [inputs, player]
}

pub fn handler<'info>(
    ctx: Context<'info, WriteAndCloseAccounts<'info>>,
    id: u64,
    _scores: Vec<PlayerScore>,
) -> Result<()> {
    let lobby_info = ctx.accounts.lobby.to_account_info();

    let lobby_account = deser_and_check_lobby(&lobby_info, id, *ctx.program_id)?;

    require_keys_eq!(
        ctx.accounts.creator.key(),
        lobby_account.metadata.creator,
        GameProgramError::CreatorMismatch
    );

    if matches!(lobby_account.metadata.network, Network::FullyOnChain(_)) {
        let players: Vec<Pubkey> = match lobby_account.state {
            LobbyState::NotStarted(not_started) => {
                not_started.player_status.keys().copied().collect()
            }
            LobbyState::Ongoing(ongoing) => ongoing.tick_info.inputs.keys().copied().collect(),
            LobbyState::Finished(LobbyFinished(finished)) => {
                finished.tick_info.inputs.keys().copied().collect()
            }
        };

        // one inputs account plus the player it belongs to (needed to refund its rent)
        const ACCOUNTS_PER_PLAYER: usize = 2;
        if ctx.remaining_accounts.len() != players.len() * ACCOUNTS_PER_PLAYER {
            Err(GameProgramError::MissingInputsAccount)?;
        }

        for (player, accounts) in players
            .iter()
            .zip(ctx.remaining_accounts.chunks_exact(ACCOUNTS_PER_PLAYER))
        {
            let [inputs_account, player_account] = accounts else {
                return Err(GameProgramError::MissingInputsAccount.into());
            };

            require_keys_eq!(
                player_account.key(),
                *player,
                GameProgramError::PlayerNotInLobby
            );

            deser_and_check_inputs(inputs_account, *player, id, *ctx.program_id)?;

            // the player paid for this account's rent in `ready`, so refund them
            close_account(inputs_account, player_account)?;
        }
    }

    close_account(&lobby_info, &ctx.accounts.creator.to_account_info())?;

    Ok(())
}

/// Drain `account` into `refund`, hand it back to the system program and zero its data.
pub(crate) fn close_account(account: &AccountInfo, refund: &AccountInfo) -> Result<()> {
    let lamports = account.lamports();

    **account.try_borrow_mut_lamports()? = 0;
    **refund.try_borrow_mut_lamports()? = refund
        .lamports()
        .checked_add(lamports)
        .ok_or(ProgramError::ArithmeticOverflow)?;

    account.assign(&System::id());
    account.resize(0)?;

    Ok(())
}
