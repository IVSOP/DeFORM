use wincode::{SchemaRead, SchemaWrite};

use crate::{DeformUserLogic, TickInfo};

// FIX: let the user pass in additional data as an arbitrary &U
/// An on-chain lobby account, where the game has not been started.
/// Serialized with wincode (not borsh), so it does not use `#[account]` in Anchor.
// #[cfg_attr(not(target_arch = "bpf"), derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Debug, SchemaRead, SchemaWrite)]
#[cfg_attr(not(target_arch = "bpf"), derive(serde::Serialize, serde::Deserialize))]
pub struct LobbyOngoing<T: DeformUserLogic> {
    pub tick: u64,
    // contains both inputs and the game state
    pub tick_info: TickInfo<T>,
    pub user_logic: T,
}
