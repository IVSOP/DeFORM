use std::sync::{Arc, Mutex};

use tokio::sync::{Notify, mpsc};

use crate::lobby::LobbyStatus;

pub mod lobby;

/// Trait that defines what data types the game uses, as well as the logic functions/callbacks.
///
/// To use this crate, you should start by implementing this type, and then making a [`Client`].
/// Note that the callbacks have mutable access to `self`, meaning you can provide and mutate your own data,
/// using the struct that implements this trait.
pub trait SdkLogic {
    // user must define inputs and game state
    type Inputs: Eq + Clone + serde::Serialize;
    type GameState: Clone + serde::Serialize;

    // user must provide certain callbacks
    /// User-provided callback to advance the game state. From a certain state and inputs, it must compute the next state.
    fn advance_frame(&mut self, state: &Self::GameState, inputs: &Self::Inputs) -> Self::GameState;

    /// User-provided callback called when a callback is triggered.
    /// 
    /// This could be used, for example, to manually emit events, or log information.
    fn on_rollback(&mut self) {}
}

struct QuicBackend<T: SdkLogic> {
    /// The user-provided logic struct. Used to execute the callbacks.
    /// Since we store the type, it is also possible for the user
    /// to pass in some arbitrary data, as well as mutate it inside of the callbacks.
    user_logic: T,



    // internal data this needs to have:
    // per-frame inputs
    // latest state
}

pub struct Client<T: SdkLogic> {
    // send a msg to terminate
    pub terminate: Arc<Notify>,

    // send a msg to set inputs
    // FIX: in the future, this should be changed to no longer be a channel; instead client can access the inputs directly, like I do for reading state
    pub set_inputs_sender: mpsc::UnboundedSender<T::Inputs>,
    // game state to be read by the client
    pub game_state: Arc<Mutex<SdkGameState<T>>>,
}

/// Game state returned by the SDK to your application
#[derive(serde::Serialize)]
pub struct SdkGameState<T: SdkLogic> {
    /// The current state of the simulation, which may be ahead of the server
    pub state: T::GameState,
    /// The previous inputs, which lead to this state
    pub inputs: T::Inputs,
    /// The last known status the server has sent us
    pub remote_status: LobbyStatus,
}

impl<T: SdkLogic> Client<T> {
    /// Initializes an offline backend
    pub fn new_offline() -> Self {
        todo!()
    }

    /// Initializes a QUIC backend
    pub fn new_quic() -> Self {
        todo!()
    }

    /// Returns the latest state along with other useful information
    pub fn read_state(&self) -> SdkGameState<T> {
        todo!()
    }

    pub fn set_inputs(&self, inputs: T::Inputs) {
        todo!()
    }
}
