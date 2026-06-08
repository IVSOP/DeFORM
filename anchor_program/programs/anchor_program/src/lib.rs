pub mod constants;
pub mod deform;
pub mod error;
pub mod instructions;
pub mod state;

use anchor_lang::prelude::*;

pub use constants::*;
pub use instructions::*;
pub use state::*;

declare_id!("D3m2Wjs5kCgXWaoJxAuZzyTZpvwjEqrhckxJBSs3THfV");

#[program]
pub mod anchor_program {
    use super::*;

    pub fn create_lobby(ctx: Context<CreateLobbyAccounts>, id: u64) -> Result<()> {
        create_lobby::handler(ctx, id)
    }

    pub fn join_lobby(ctx: Context<JoinLobbyAccounts>, id: u64) -> Result<()> {
        join_lobby::handler(ctx, id)
    }
}
