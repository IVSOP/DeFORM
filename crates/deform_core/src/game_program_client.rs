#[cfg(feature = "client")]
use std::collections::HashMap;

#[cfg(feature = "client")]
use solana_instruction::Instruction;

use crate::accounts::lobby::Network;
#[cfg(feature = "client")]
use crate::{
    accounts::lobby::{not_started::LobbyNotStarted, Lobby, LobbyMetadata},
    DeformUserLogic, Pubkey,
};

// TODO: cleanup to not repeat things?
pub enum ReadyArgs {
    Web2 {
        user: Pubkey,
        lobby: Pubkey,
        id: u64,
    },
    FullyOnchain {
        user: Pubkey,
        lobby: Pubkey,
        id: u64,
        inputs: Pubkey,
    },
}

// TODO: rename, not really a client, more of an instruction builder
/// Since the on-chain aspects are fully customizable and are just a template for the user, the user must also specify how the instructions are created.
#[cfg(feature = "client")]
pub trait GameProgramClient<T: DeformUserLogic>: Clone + Send + Sync {
    fn game_program(&self) -> Pubkey;

    fn create_lobby_ix(
        &self,
        user: Pubkey,
        lobby: Pubkey,
        lobby_id: u64,
        network: Network,
    ) -> Result<Instruction, T::Error>;

    fn join_lobby_ix(
        &self,
        user: Pubkey,
        lobby: Pubkey,
        lobby_id: u64,
    ) -> Result<Instruction, T::Error>;

    fn ready_ix(&self, args: ReadyArgs) -> Result<Instruction, T::Error>;

    fn write_and_close_ix(
        &self,
        admin: Pubkey,
        lobby_pubkey: Pubkey,
        creator: Pubkey,
        lobby: &Lobby<T>,
    ) -> Result<Instruction, T::Error>;

    /// Closes `account` and refunds its rent to `admin`, with no checks whatsoever.
    /// Meant for wiping accounts left behind by an older layout, which no longer
    /// deserialize and so can't go through [`Self::write_and_close_ix`].
    fn force_close_ix(&self, admin: Pubkey, account: Pubkey) -> Result<Instruction, T::Error>;

    fn start_ix(
        &self,
        user: Pubkey,
        lobby_pubkey: Pubkey,
        lobby_metadata: &LobbyMetadata,
        not_started: &LobbyNotStarted,
        game: Pubkey,
    ) -> Result<Instruction, T::Error>;

    fn set_inputs_ix(
        &self,
        user: Pubkey,
        // these two accounts are already passed in as this instruction will run multiple times per frame
        inputs_account: Pubkey,
        lobby_account: Pubkey,
        lobby_id: u64,
        inputs: &HashMap<u64, T::Inputs>,
    ) -> Result<Instruction, T::Error>;

    /// Builds the instruction the crank runs each interval to advance the game
    /// on the ephemeral rollup. It is signerless — the magic-program crank
    /// executor drives it unattended. `inputs_accounts` are the (already-delegated)
    /// per-player inputs accounts the tick reads, in lobby order. Exposed as its
    /// own method so [`Self::init_crank_ix`] can embed its `Instruction`, and so
    /// an off-chain driver could also send it directly.
    fn tick_ix(
        &self,
        lobby_account: Pubkey,
        lobby_id: u64,
        inputs_accounts: &[Pubkey],
    ) -> Result<Instruction, T::Error>;

    /// Builds an instruction that schedules a recurring crank/task on the
    /// ephemeral rollup, which runs [`Self::tick_ix`] every
    /// `execution_interval_millis` for `iterations` times. Assumes the lobby and
    /// inputs accounts are already delegated to the rollup. Must be sent to the
    /// ER (not the base layer).
    fn init_crank_ix(
        &self,
        payer: Pubkey,
        lobby_account: Pubkey,
        lobby_id: u64,
        inputs_accounts: &[Pubkey],
        execution_interval_millis: i64,
        iterations: i64,
    ) -> Result<Instruction, T::Error>;
}
