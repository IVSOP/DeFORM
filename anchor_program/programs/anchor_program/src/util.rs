use anchor_lang::{prelude::*, system_program};

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
    space: usize,
    signer_seeds: &[&[u8]],
) -> Result<()> {
    let rent = Rent::get()?;
    let required_lamports = rent.minimum_balance(space);
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
            space as u64,
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
            space as u64,
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
