use deform_core::Pubkey;
use deform_core::accounts::lobby::Lobby;
use deform_core::error::UserFacingResult;
use deform_core::{DeformClient, DeformUserLogic};

mod client;

pub fn new_offline_client<T: DeformUserLogic>(
    player: Pubkey,
    lobby: Lobby<T>,
    bot_fn: fn(&T::GameState, &Pubkey, &T::Inputs) -> T::Inputs,
    visual_tick_micros: u64,
) -> UserFacingResult<T, DeformClient<T>> {
    client::OfflineBackend::<T>::init(player, lobby, bot_fn, visual_tick_micros)
}
