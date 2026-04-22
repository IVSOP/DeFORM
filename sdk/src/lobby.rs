use std::collections::HashMap;

use pinocchio::pubkey::Pubkey;

use crate::SdkLogic;

/// Information about a player.
/// You will probably have per-player information in the GameState, which makes this inneficient, idk how to solve it.
pub struct PlayerInfo<T: SdkLogic> {
    /// Last inputs, used to generate the current [`GameState`]
    pub inputs: T::Inputs,
    pub ephemeral_key: Pubkey,
    // main pubkey is implicit in the key of the hashmap
}

/// An on-chain lobby account
pub struct Lobby<T: SdkLogic> {
    /// Given a player's main pubkey, corresponds the information about this player
    pub players_info: HashMap<Pubkey, PlayerInfo<T>>,
    pub game_state: T::GameState,
    pub status: LobbyStatus,
}

#[cfg_attr(not(target_arch = "bpf"), derive(serde::Serialize))]
pub enum LobbyStatus {
    NotStarted = 0,
    Started = 1,
    Finished = 2,
}
