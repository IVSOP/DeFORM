pub mod constants;
pub mod error;
pub mod instructions;
pub mod state;
pub mod util;

use anchor_lang::prelude::*;

pub use constants::*;
pub use instructions::*;

declare_id!("9qwW8eBuxLP6Gw1dGDFtTgXGHLnYm8usU1BW6R7BRknK");

#[program]
pub mod anchor_program {
    use super::*;

    pub fn create_lobby(ctx: Context<CreateLobbyAccounts>, id: u64) -> Result<()> {
        create_lobby::handler(ctx, id)
    }

    pub fn join_lobby(ctx: Context<JoinLobbyAccounts>, id: u64) -> Result<()> {
        join_lobby::handler(ctx, id)
    }

    pub fn ready(ctx: Context<ReadyAccounts>, id: u64, fully_onchain: bool) -> Result<()> {
        ready::handler(ctx, id, fully_onchain)
    }

    pub fn write_and_close(
        ctx: Context<WriteAndCloseAccounts>,
        id: u64,
        scores: Vec<PlayerScore>,
    ) -> Result<()> {
        write_and_close::handler(ctx, id, scores)
    }
}
