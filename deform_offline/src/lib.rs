use deform_core::Pubkey;
use deform_core::{DeformClient, DeformResult, DeformUserLogic};

mod client;

pub fn new_offline_client<T: DeformUserLogic>(player: Pubkey) -> DeformResult<DeformClient<T>> {
    client::OfflineBackend::<T>::init(player)
}
