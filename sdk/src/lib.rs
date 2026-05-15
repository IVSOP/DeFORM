use std::sync::{Arc, Mutex, atomic::AtomicBool};
use anyhow::Result;

use tokio::sync::{Notify, mpsc};

use crate::lobby::LobbyStatus;

pub mod lobby;
pub mod backend;

/// Trait that defines what data types the game uses, as well as the logic functions/callbacks.
///
/// To use this crate, you should start by implementing this type, and then making a [`Client`].
/// Note that the callbacks have mutable access to `self`, meaning you can provide and mutate your own data,
/// using the struct that implements this trait.
pub trait UserLogic {
    // user must define inputs and game state
    type Inputs: Eq + Clone + Send + Sync + 'static + serde::Serialize;
    type GameState: Clone + serde::Serialize;

    // user must provide certain callbacks
    /// User-provided callback to advance the game state. From a certain state and inputs, it must compute the next state.
    fn advance_frame(&mut self, state: &Self::GameState, inputs: &Self::Inputs) -> Self::GameState;

    /// User-provided callback called when a callback is triggered.
    /// 
    /// This could be used, for example, to manually emit events, or log information.
    fn before_rollback(&mut self) {}
    fn after_rollback(&mut self) {}

    /// User-provided callback called when a gap is detected.
    /// A gap happens when, for example, the states received from the server are:
    /// 0 1 2 3 _ 5 -> gap on `4`
    /// 
    /// This could be used, for example, to manually emit events, or log information.
    fn before_gap(&mut self) {}
    fn after_gap(&mut self) {}
}

/// A [`Client`] acts as the frontend interface where the game interacts with the library, abstracting the underlying backend implementation.
/// Currently, the client is completely agnostic to the backend.
pub struct Client<T: UserLogic> {
    /// Used to tell the backend to terminate
    pub terminate: Arc<Notify>,
    /// Channel used to set inputs
    // FIX: in the future, this should be changed to no longer be a channel; instead client can access the inputs directly, like I do for reading state. When that happens I thing Inputs no longer needs to be Send
    pub set_inputs_sender: mpsc::UnboundedSender<T::Inputs>,
    /// Game state to be read by the client
    pub game_state: Arc<Mutex<SdkGameState<T>>>,
    /// Set to true by the backend thread when it exits (cleanly or due to error).
    pub backend_dead: Arc<AtomicBool>,
}

/// The state that is returned by the SDK to your application
#[derive(serde::Serialize)]
pub struct SdkGameState<T: UserLogic> {
    /// The current state of the simulation, which may be ahead of the server
    pub state: T::GameState,
    /// The previous inputs, which lead to this state
    pub inputs: T::Inputs,
    /// The last known status the server has sent us
    pub remote_status: LobbyStatus,
}

impl<T: UserLogic> Clone for SdkGameState<T> {
    fn clone(&self) -> Self {
        SdkGameState {
            state: self.state.clone(),
            inputs: self.inputs.clone(),
            remote_status: self.remote_status.clone(),
        }
    }
}

impl<T: UserLogic> Client<T> {
    /// Returns the latest state along with other useful information
    pub fn read_state(&self) -> Result<SdkGameState<T>> {
        let state = match self.game_state.lock() {
            Ok(state) => {
                // TODO: in the future, when reading inputs, we need to clear the events
                // to do that there has to be an on_read() function or something
                // or let the user do it themselves?? idk how events are going to be handled yet
                state.clone()
            }
            // cursed since error has a lifetime for whatever reason and anyhow needs 'static
            Err(e) => return Err(anyhow::anyhow!("{e}")),
        };

        Ok(state)
    }

    pub fn set_inputs(&self, inputs: T::Inputs) -> Result<()> {
        Ok(self.set_inputs_sender.send(inputs)?)
    }

    pub fn shutdown(&self) {
        self.terminate.notify_one()
    }
}
