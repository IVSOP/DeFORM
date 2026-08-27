use std::{fmt::Debug, sync::Arc};

use deform_core::{
    DeformClient, DeformUserLogic, accounts::lobby::Lobby, error::UserFacingResult,
    game_program_client::GameProgramClient,
};
use solana_sdk::signature::Keypair;
use tokio_util::sync::CancellationToken;

mod client;
mod ws;

use client::FocBackend;

/// Ties a game's [`DeformUserLogic`] to the [`GameProgramClient`] that builds its
/// on-chain instructions. The FoC analogue of `DeformQuicLogic`, minus the Web2
/// server concepts (auth, custom reliable messages).
pub trait DeformFocLogic: Clone + Sized + Debug + Send + Sync + 'static {
    type UserLogic: DeformUserLogic;
    type ProgramClient: GameProgramClient<Self::UserLogic>;

    /// Maximum time-dilation rate, as a fraction of the base tick rate.
    /// 0.10 means the client runs at most 10% faster to refill the server's input
    /// buffer. Slowing down is capped at half of this since being early only costs lag and not a missprediction
    const TIME_DILATION: f32 = 0.10;

    /// Extra inputs to keep queued on the server beyond the one it consumes each tick as margin for jitter
    const JITTER_SLACK: f32 = 2.0;
}

pub fn new_foc_client<F: DeformFocLogic>(
    rpc_url: String,
    ws_url: String,
    keypair: Arc<Keypair>,
    program_client: F::ProgramClient,
    lobby: Lobby<F::UserLogic>,
    visual_tick_micros: u64,
    slot_time_micros: u64,
    cancellation_token: CancellationToken,
) -> UserFacingResult<F::UserLogic, DeformClient<F::UserLogic>> {
    FocBackend::<F>::init(
        rpc_url,
        ws_url,
        keypair,
        program_client,
        lobby,
        visual_tick_micros,
        slot_time_micros,
        cancellation_token,
    )
}
