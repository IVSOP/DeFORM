use std::sync::{Arc, Mutex, MutexGuard};

use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::{
    accounts::lobby::Lobby, error::UserFacingResult, DeformError, DeformResult, DeformUserLogic,
};

/// A [`DeformClient`] acts as the frontend interface where the game interacts with the library, abstracting the underlying backend implementation.
/// Currently, the client is completely agnostic to the backend.
#[derive(Clone)]
pub struct DeformClient<T: DeformUserLogic> {
    /// Channel used to set inputs
    // FIX: in the future, this should be changed to no longer be a channel; instead client can access the inputs directly, like I do for reading state. When that happens I thing Inputs no longer needs to be Send
    pub set_inputs_sender: mpsc::UnboundedSender<T::Inputs>,
    /// Game state to be read by the client, along with other useful info from the backend
    pub backend_state: Arc<Mutex<DeformSharedBackendState<T>>>,
    /// This has three uses:
    /// - Check if the backend has already exited
    /// - Order the backend to stop what it is doing and exit cleanly
    /// - Once the game ends, the backend will not imediately exit, to prevent issues: since we don't have a global mutex on this, it is possible to be sending inputs and just then the backend finishes, closing the channe. So, the user is responsible for manually calling this to shutdown the backend every time
    pub cancellation_token: CancellationToken,
}

/// The state that is returned by the SDK to your application.
pub struct DeformSharedBackendState<T: DeformUserLogic> {
    pub lobby: Lobby<T>,
    // TODO: how to make this customizable by each backend?
    pub stats: Stats,
    pub internal_error: UserFacingResult<T, ()>,
}

impl<T: DeformUserLogic> DeformSharedBackendState<T> {
    pub fn new_from_lobby(lobby: Lobby<T>) -> UserFacingResult<T, Self> {
        Ok(Self {
            lobby,
            stats: Default::default(),
            internal_error: Ok(()),
        })
    }
}

#[derive(serde::Serialize, Default, Clone)]
pub struct Stats {
    pub ping_ms: f64,
}

impl<T: DeformUserLogic> DeformClient<T> {
    /// Returns the latest state along with other useful information.
    /// To prevent unecessary cloning (and having to derive Clone), this is just a very thin
    /// wrapper of locking the mutex. You must drop it as soon as possible to avoid contention.
    pub fn read_state(&self) -> DeformResult<MutexGuard<'_, DeformSharedBackendState<T>>> {
        let state = self
            .backend_state
            .lock()
            .map_err(|_| DeformError::LockPoisoned)?;

        Ok(state)
    }

    pub fn set_inputs(&self, inputs: T::Inputs) -> DeformResult {
        self.set_inputs_sender
            .send(inputs)
            .map_err(|_| DeformError::ChannelClosed)
    }

    pub fn shutdown(&self) {
        self.cancellation_token.cancel()
    }
}
