use deform_core::Pubkey;
use deform_core::{DeformClient, DeformResult, DeformUserLogic};
use std::collections::HashSet;

mod client;

pub fn new_offline_client<T: DeformUserLogic>(
    player: Pubkey,
    players: HashSet<Pubkey>,
    bot_fn: fn(&T::GameState, &Pubkey, &T::Inputs) -> T::Inputs,
    visual_tick_micros: u64,
) -> DeformResult<DeformClient<T>> {
    client::OfflineBackend::<T>::init(player, players, bot_fn, visual_tick_micros)
}
