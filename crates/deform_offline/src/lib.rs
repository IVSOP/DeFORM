use deform_core::{
    DeformClient, DeformUserLogic, Pubkey, accounts::lobby::Lobby, error::UserFacingResult,
};
use tokio_util::sync::CancellationToken;

mod client;

pub fn new_offline_client<T: DeformUserLogic>(
    player: Pubkey,
    lobby: Lobby<T>,
    bot_fn: fn(&T::GameState, &Pubkey, &T::Inputs) -> T::Inputs,
    visual_tick_micros: u64,
    cancellation_token: CancellationToken,
) -> UserFacingResult<T, DeformClient<T>> {
    client::OfflineBackend::<T>::init(
        player,
        lobby,
        bot_fn,
        visual_tick_micros,
        cancellation_token,
    )
}
