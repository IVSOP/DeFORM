use crate::{error::GameProgramError, state::UserLogic};
use anchor_lang::{prelude::*, system_program};
use deform_core::accounts::{lobby::Lobby, DeformAccount};

/// Robustly create a program-owned PDA for one of our wincode-serialized accounts.
///
/// This mirrors what Anchor's `#[account(init)]` does under the hood, but works for
/// accounts we (de)serialize manually with wincode instead of borsh.
///
/// A bare `system_program::create_account` fails if the target already holds any
/// lamports. That lets an attacker permanently block a PDA by pre-funding it with a
/// plain transfer before the legitimate creation runs. To close that griefing vector,
/// when the account is already funded we instead top it up to rent-exemption (if
/// needed), then `allocate` the space and `assign` ownership to `owner`.
///
/// The caller is responsible for verifying `target`'s address is the expected PDA and
/// that it is not already initialized (empty data + system-owned) before calling this.
pub fn create_pda_account<'info>(
    payer: &AccountInfo<'info>,
    target: &AccountInfo<'info>,
    system_program_id: Pubkey,
    owner: &Pubkey,
    space: u64,
    signer_seeds: &[&[u8]],
) -> Result<()> {
    let rent = Rent::get()?;
    let required_lamports = rent.minimum_balance(space as usize);
    let current_lamports = target.lamports();

    if current_lamports == 0 {
        // Fresh account: create it in a single instruction.
        system_program::create_account(
            CpiContext::new_with_signer(
                system_program_id,
                system_program::CreateAccount {
                    from: payer.clone(),
                    to: target.clone(),
                },
                &[signer_seeds],
            ),
            required_lamports,
            space,
            owner,
        )?;
    } else {
        // Account was pre-funded (possibly by an attacker). Top up to rent-exemption,
        // then allocate space and assign ownership to our program.
        let deficit = required_lamports.saturating_sub(current_lamports);
        if deficit > 0 {
            system_program::transfer(
                CpiContext::new(
                    system_program_id,
                    system_program::Transfer {
                        from: payer.clone(),
                        to: target.clone(),
                    },
                ),
                deficit,
            )?;
        }
        system_program::allocate(
            CpiContext::new_with_signer(
                system_program_id,
                system_program::Allocate {
                    account_to_allocate: target.clone(),
                },
                &[signer_seeds],
            ),
            space,
        )?;
        system_program::assign(
            CpiContext::new_with_signer(
                system_program_id,
                system_program::Assign {
                    account_to_assign: target.clone(),
                },
                &[signer_seeds],
            ),
            owner,
        )?;
    }

    Ok(())
}

pub fn deser_and_check_lobby(
    lobby_account: AccountInfo,
    lobby_id: u64,
    program: Pubkey,
) -> Result<Lobby<UserLogic>> {
    // account must have > 0 lamports
    require_gt!(**lobby_account.lamports.borrow(), 0);

    // owned by our program
    require_keys_eq!(*lobby_account.owner, program);

    // deserialize (will also check data len indirectly)
    let data = lobby_account.data.borrow();
    let lobby =
        DeformAccount::from_bytes(&data).map_err(|_| error!(GameProgramError::DeserializeLobby))?;

    // account type matches
    let DeformAccount::Lobby(lobby) = lobby else {
        return Err(GameProgramError::InvalidAccountType)?;
    };

    // pda matches
    let lobby_pda =
        Lobby::<UserLogic>::create_program_address(lobby_id, &program, lobby.metadata.bump)
            .map_err(|_| ProgramError::InvalidSeeds)?;
    require_keys_eq!(lobby_pda, *lobby_account.key, GameProgramError::InvalidPda);

    // id matches
    require_eq!(lobby_id, lobby.metadata.id);

    Ok(lobby)
}
