use deform_core::Pubkey;
use deform_core::{DeformClient, DeformResult, DeformUserLogic};
use std::collections::HashSet;

mod client;

pub fn new_offline_client<T: DeformUserLogic>(
    player: Pubkey,
    players: HashSet<Pubkey>,
) -> DeformResult<DeformClient<T>> {
    client::OfflineBackend::<T>::init(player, players)
}
