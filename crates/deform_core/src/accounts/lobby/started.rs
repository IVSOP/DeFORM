use wincode::{SchemaRead, SchemaWrite};

use crate::{accounts::lobby::Network, DeformUserLogic, Pubkey, TickInfo};

// FIX: let the user pass in additional data as an arbitrary &U
/// An on-chain lobby account, where the game has not been started.
/// Serialized with wincode (not borsh), so it does not use `#[account]` in Anchor.
// #[cfg_attr(not(target_arch = "bpf"), derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Debug, SchemaRead, SchemaWrite)]
pub struct LobbyOngoing<T: DeformUserLogic> {
    pub id: u64,
    pub creator: Pubkey,
    pub network: Network,
    pub tick: u64,
    // contains both inputs and the game state
    pub tick_info: TickInfo<T>,
    pub user_logic: T,
    pub bump: u8,
}
