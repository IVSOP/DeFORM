#[cfg(feature = "client")]
use solana_instruction::Instruction;

use crate::accounts::lobby::Network;
#[cfg(feature = "client")]
use crate::{accounts::lobby::LobbyFinished, DeformUserLogic, Pubkey};

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
    ) -> Instruction;

    fn join_lobby_ix(&self, user: Pubkey, lobby: Pubkey, lobby_id: u64) -> Instruction;

    fn ready_ix(&self, args: ReadyArgs) -> Instruction;

    fn write_and_close_ix(
        &self,
        admin: Pubkey,
        lobby_pubkey: Pubkey,
        creator: Pubkey,
        lobby: &LobbyFinished<T>,
    ) -> Instruction;
}
