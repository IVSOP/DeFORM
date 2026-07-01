use deform_core::Pubkey;
use solana_address::address;

pub mod generated;
pub mod pong_logic;
pub mod solana;

pub use pong_logic::{PongGame, PongGameState, PongInputs};

#[cfg(feature = "client")]
pub use pong_logic::{NoAuth, PongQuicLogic};

pub const ANCHOR_PROGRAM_ID: Pubkey = address!("5Ku1phD9gZ6PQYv8YVBpK6WnzXQFBZ5un9u59RL7G82r");
