use std::{
    collections::{HashMap, HashSet},
    sync::{atomic::AtomicBool, Arc, Mutex, MutexGuard},
};

use tokio::sync::{mpsc, Notify};

use crate::{
    error::UserFacingResult, lobby::LobbyStatus, DeformError, DeformGameState, DeformResult,
    DeformUserLogic, Pubkey, TickInfo,
};

/// A [`DeformClient`] acts as the frontend interface where the game interacts with the library, abstracting the underlying backend implementation.
/// Currently, the client is completely agnostic to the backend.
pub struct DeformClient<T: DeformUserLogic> {
    /// Used to tell the backend to terminate
    pub terminate: Arc<Notify>,
    /// Channel used to set inputs
    // FIX: in the future, this should be changed to no longer be a channel; instead client can access the inputs directly, like I do for reading state. When that happens I thing Inputs no longer needs to be Send
    pub set_inputs_sender: mpsc::UnboundedSender<T::Inputs>,
    /// Game state to be read by the client
    pub sdk_game_state: Arc<Mutex<DeformReadState<T>>>,
    /// Set to true by the backend thread when it exits (cleanly or due to error).
    pub backend_dead: Arc<AtomicBool>,
}

/// The state that is returned by the SDK to your application.
#[derive(serde::Serialize)]
pub struct DeformReadState<T: DeformUserLogic> {
    pub tick_info: TickInfo<T>,
    /// The last known status the server has sent us
    pub remote_status: LobbyStatus,
    pub stats: Stats,
    /// Your own data, so you can read it back when reading the rest of the state.
    ///
    /// NOTE: I had a lot of trouble deciding how to do this. The backends need mutable access, so I always have to use a mutex of some sort.
    /// However, running the callbacks inside the lock is bad as I don't know how long the operations being done by the user are taking.
    /// So, the approach I have chosen is to keep an owned T in the backend. After operations are done and the [`DeformReadState`] needs to be updated, it is cloned into here.
    pub user_logic: T,
    pub internal_error: UserFacingResult<T, ()>,
}

impl<T: DeformUserLogic> DeformReadState<T> {
    /// Create a new state when players are known
    pub fn new(players: &HashSet<Pubkey>) -> Self {
        let game_state = T::GameState::new(players);
        let mut inputs = HashMap::new();
        for player in players.iter() {
            inputs.insert(player.clone(), T::Inputs::default());
        }
        let tick_info = TickInfo { game_state, inputs };

        Self {
            tick_info,
            remote_status: Default::default(),
            stats: Default::default(),
            user_logic: Default::default(),
            internal_error: Ok(()),
        }
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
    pub fn read_state(&self) -> DeformResult<MutexGuard<'_, DeformReadState<T>>> {
        let state = self
            .sdk_game_state
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
