use std::collections::HashMap;

use wincode::{SchemaRead, SchemaWrite};

use crate::{
    accounts::lobby::{Network, PlayerStatus},
    Pubkey,
};

// FIX: let the user pass in additional data as an arbitrary &U
/// An on-chain lobby account, where the game has not been started.
/// Serialized with wincode (not borsh), so it does not use `#[account]` in Anchor.
#[cfg_attr(not(target_arch = "bpf"), derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Debug, SchemaRead, SchemaWrite)]
pub struct LobbyNotStarted {
    pub id: u64,
    pub creator: Pubkey,
    pub network: Network,
    pub player_status: HashMap<Pubkey, PlayerStatus>, // ready vs not ready
    pub bump: u8,
}

impl LobbyNotStarted {
    pub fn new(
        id: u64,
        creator: Pubkey,
        network: Network,
        player_status: HashMap<Pubkey, PlayerStatus>,
        bump: u8,
    ) -> Self {
        Self {
            id,
            creator,
            network,
            player_status,
            bump,
        }
    }
}
