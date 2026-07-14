pub mod constants;
pub mod error;
pub mod instructions;
pub mod state;
pub mod util;

use anchor_lang::prelude::*;
pub use constants::*;
use deform_core::accounts::lobby::Network;
pub use instructions::*;
use state::Inputs;

declare_id!("5Ku1phD9gZ6PQYv8YVBpK6WnzXQFBZ5un9u59RL7G82r");

#[program]
pub mod anchor_program {
    use super::*;

    pub fn create_lobby(
        ctx: Context<CreateLobbyAccounts>,
        id: u64,
        network: Network,
    ) -> Result<()> {
        create_lobby::handler(ctx, id, network)
    }

    pub fn join_lobby(ctx: Context<JoinLobbyAccounts>, id: u64) -> Result<()> {
        join_lobby::handler(ctx, id)
    }

    pub fn ready(ctx: Context<ReadyAccounts>, id: u64) -> Result<()> {
        ready::handler(ctx, id)
    }

    pub fn write_and_close(
        ctx: Context<WriteAndCloseAccounts>,
        id: u64,
        scores: Vec<PlayerScore>,
    ) -> Result<()> {
        write_and_close::handler(ctx, id, scores)
    }

    pub fn start<'info>(ctx: Context<'info, StartGameAccounts<'info>>, id: u64) -> Result<()> {
        start::handler(ctx, id)
    }

    // anchor devs believe you are stupid so they don't let you use BTreeMap here, so it has to be a Vec
    // also, codama will generate a new Inputs type instead of just reusing the current one, if you try to use a workaround struct
    // the easiest solution is really to just use wincode and only pass in the bytes
    pub fn set_inputs<'info>(
        ctx: Context<'info, SetInputsAccounts<'info>>,
        id: u64,
        // bytes of a HashMap<u64, Inputs>
        batch_inputs_bytes: Vec<u8>,
    ) -> Result<()> {
        set_inputs::handler(ctx, id, batch_inputs_bytes)
    }

    pub fn tick<'info>(ctx: Context<'info, TickAccounts<'info>>, id: u64) -> Result<()> {
        tick::handler(ctx, id)
    }

    pub fn undelegate<'info>(
        ctx: Context<'info, UndelegateAccounts<'info>>,
        id: u64,
    ) -> Result<()> {
        undelegate::handler(ctx, id)
    }

    /// Delegation-program callback. Do not call directly and do not rename — the
    /// name determines the discriminator the delegation program CPIs into.
    pub fn process_undelegation(
        ctx: Context<InitializeAfterUndelegation>,
        account_seeds: Vec<Vec<u8>>,
    ) -> Result<()> {
        undelegate::process_undelegation_handler(ctx, account_seeds)
    }
}
