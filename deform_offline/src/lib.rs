use deform_core::Pubkey;
use deform_core::{DeformClient, DeformResult, DeformUserLogic};
use std::collections::HashSet;

mod client;

pub fn new_offline_client<T: DeformUserLogic>(
    player: Pubkey,
    players: HashSet<Pubkey>,
    bot_fn: impl Fn(&T::GameState, &Pubkey) -> T::Inputs + Send + Sync + 'static,
) -> DeformResult<DeformClient<T>> {
    client::OfflineBackend::<T>::init(player, players, bot_fn)
}
