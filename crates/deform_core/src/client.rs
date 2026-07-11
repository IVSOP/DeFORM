use std::sync::{atomic::AtomicBool, Arc, Mutex, MutexGuard};

use tokio::sync::{mpsc, Notify};

use crate::{
    accounts::lobby::Lobby, error::UserFacingResult, DeformError, DeformResult, DeformUserLogic,
};

/// A [`DeformClient`] acts as the frontend interface where the game interacts with the library, abstracting the underlying backend implementation.
/// Currently, the client is completely agnostic to the backend.
pub struct DeformClient<T: DeformUserLogic> {
    /// Used to tell the backend to terminate
    pub terminate: Arc<Notify>,
    /// Channel used to set inputs
    // FIX: in the future, this should be changed to no longer be a channel; instead client can access the inputs directly, like I do for reading state. When that happens I thing Inputs no longer needs to be Send
    pub set_inputs_sender: mpsc::UnboundedSender<T::Inputs>,
    /// Game state to be read by the client, along with other useful info from the backend
    pub backend_state: Arc<Mutex<DeformSharedBackendState<T>>>,
    /// Set to true by the backend thread when it exits (cleanly or due to error).
    pub backend_dead: Arc<AtomicBool>,
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
        self.terminate.notify_one()
    }
}
