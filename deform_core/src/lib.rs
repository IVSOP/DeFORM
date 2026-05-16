use pinocchio::pubkey::Pubkey;
use std::{
    collections::HashMap,
    sync::{Arc, Mutex, atomic::AtomicBool},
};
use wincode::{SchemaRead, SchemaWrite, config::DefaultConfig};

use tokio::sync::{Notify, mpsc};

use crate::lobby::LobbyStatus;

pub mod error;
pub mod lobby;

pub use error::{DeformError, DeformResult};

/// Trait that defines what data types the game uses, as well as the logic functions/callbacks.
///
/// To use this crate, you should start by implementing this type, and then making a [`Client`].
/// Note that the callbacks have mutable access to `self`, meaning you can provide and mutate your own data,
/// using the struct that implements this trait.
///
/// Note that while this trait defines [`UserLogic::Inputs`] and [`UserLogic::GameState`], the backend is the one responsible for holding stateful information. You should use the struct that implements this trait to store aditional data that you want to keep out of the [`UserLogic::GameState`].
pub trait DeformUserLogic: Clone + Default + Send + 'static {
    // user must define inputs and game state
    type Inputs: DeformInputs;
    type GameState: DeformGameState;
    type Error: std::error::Error + Send + Sync + 'static;

    // user must provide certain callbacks
    /// User-provided callback to advance the game state. From a certain state and inputs, it must compute the next state.
    // NOTE: I'm not using TickInfo here to not make it more confusing to the user
    fn advance_frame(
        &mut self,
        state: &Self::GameState,
        inputs: &HashMap<Pubkey, Self::Inputs>,
    ) -> Result<Self::GameState, Self::Error>;

    /// User-provided callback called when a callback is triggered.
    ///
    /// This could be used, for example, to manually emit events, or log information.
    fn before_rollback(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }
    fn after_rollback(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }

    /// User-provided callback called when a gap is detected.
    /// A gap happens when, for example, the states received from the server are:
    /// 0 1 2 3 _ 5 -> gap on `4`
    ///
    /// This could be used, for example, to manually emit events, or log information.
    fn before_gap(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }
    fn after_gap(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }
}

/// A [`Client`] acts as the frontend interface where the game interacts with the library, abstracting the underlying backend implementation.
/// Currently, the client is completely agnostic to the backend.
pub struct Client<T: DeformUserLogic> {
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

#[derive(serde::Serialize, Clone, Default)]
pub struct TickInfo<T: DeformUserLogic> {
    /// The current game state at this tick
    pub game_state: T::GameState,
    // TODO: this is probably really bad.
    // I had to do this to prevent the user from having to store inputs inside of the GameState which would be even worse
    // but hashing pubkeys like this is going to use a lot of memory and be very slow. It would prob be better to use player IDs
    // FIX: need to serialize these pubkeys as B64
    /// The inputs that, combined with the previous state, have led to this new state.
    // TODO: in sbf, this should use pinocchio pubkey
    pub inputs: HashMap<Pubkey, T::Inputs>,
}

/// The state that is returned by the SDK to your application
#[derive(serde::Serialize, Clone, Default)]
pub struct DeformReadState<T: DeformUserLogic> {
    pub tick_info: TickInfo<T>,
    /// The last known status the server has sent us
    pub remote_status: LobbyStatus,
    pub stats: Stats,
    /// Your own data, so you can read it back when reading the rest of the state
    pub user_logic: T,
}

#[derive(serde::Serialize, Default, Clone)]
pub struct Stats {
    pub ping_ms: f64,
}

impl<T: DeformUserLogic> Client<T> {
    /// Returns the latest state along with other useful information
    pub fn read_state(&self) -> DeformResult<DeformReadState<T>> {
        let state = self
            .sdk_game_state
            .lock()
            .map_err(|_| DeformError::LockPoisoned)?
            .clone();

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

pub trait DeformInputs:
    Default
    + Eq
    + Clone
    + Send
    + Sync
    + 'static
    + serde::Serialize
    + for<'de> SchemaRead<'de, DefaultConfig, Dst = Self>
    + SchemaWrite<DefaultConfig, Src = Self>
    + MaxLen
{
    /// When inputs are predicted, some actions may not make sense to be repeated, such as one-off toggles. Using this, you can decide for yourself to just implement a simple .clone() or, instead, reset some attributes before returning the inputs.
    fn predict(&self) -> Self;
}

pub trait DeformGameState:
    Default
    + Clone
    + Send
    + serde::Serialize
    + for<'de> SchemaRead<'de, DefaultConfig, Dst = Self>
    + SchemaWrite<DefaultConfig, Src = Self>
    + MaxLen
{
}

pub trait MaxLen {
    fn max_len() -> DeformResult<usize>;
}
