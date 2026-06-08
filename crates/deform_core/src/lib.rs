use std::collections::{HashMap, HashSet};
use wincode::{config::DefaultConfig, SchemaRead, SchemaWrite};

pub use deform_derive::Smooth;

#[cfg(feature = "client")]
pub mod client;
pub mod error;
pub mod lobby;
pub mod smooth;

#[cfg(feature = "client")]
pub use client::{DeformClient, DeformReadState, Stats};

pub use error::{DeformError, DeformResult};
pub use smooth::{NoopSmoother, Smooth, SmoothParams, Smoothable, SmoothableField};

/// I like calling it a pubkey
pub type Pubkey = solana_address::Address;

/// Trait that defines what data types the game uses, as well as the logic functions/callbacks.
///
/// To use this crate, you should start by implementing this type, and then making a [`DeformClient`].
/// Note that the callbacks have mutable access to `self`, meaning you can provide and mutate your own data,
/// using the struct that implements this trait.
///
/// Note that while this trait defines [`DeformUserLogic::Inputs`] and [`DeformUserLogic::GameState`], the backend is the one responsible for holding stateful information. You should use the struct that implements this trait to store aditional data that you want to keep out of the [`DeformUserLogic::GameState`].
pub trait DeformUserLogic: Clone + Default + Send + 'static {
    // user must define inputs and game state
    type Inputs: DeformInputs;
    type GameState: DeformGameState;
    type Smoother: Smooth<Self::GameState>;
    type Error: std::error::Error + Send + Sync + 'static;

    /// Microseconds per simulation tick. For 60 fps, use `16667`.
    const TICK_RATE_MICROS: u64;

    // user must provide certain callbacks
    /// User-provided callback to advance the game state. From a certain state and inputs, it must compute the next state.
    // NOTE: I'm not using TickInfo here to not make it more confusing to the user
    fn advance_frame(
        &mut self,
        state: &Self::GameState,
        inputs: &HashMap<Pubkey, Self::Inputs>,
    ) -> Result<Self::GameState, Self::Error>;

    /// User-provided callback called when a callback is triggered. This happens when a previously computed state (in this case, prediction of inputs) does not match the state received from the server.
    ///
    /// This could be used, for example, to manually emit events, or log information.
    ///
    /// - *old_info* represents the current state (on the most recent tick) before the rollback happened
    /// - *new_info* represents the new, conflicting state that was received
    fn on_rollback(
        &mut self,
        // owned since it has been completely deleted
        _old_info: TickInfo<Self>,
        _new_info: &TickInfo<Self>,
    ) -> Result<(), Self::Error> {
        Ok(())
    }

    /// User-provided callback called when a gap is detected.
    /// A gap happens when, for example, the states received from the server are:
    /// 0 1 2 3 _ 5 -> gap on `4`
    ///
    /// NOTE: A rollback is also triggered, as it is assumed that there could be state divergences as the inputs cannot be compared, and the missing states are not recomputed, as the new state is now the source of truth.
    ///
    /// This could be used, for example, to manually emit events, or log information.
    /// If you are certain this is a non issue or can never happen (using websockets, for example), it is safe to ignore it, as a rollback will always be emitted either way.
    ///
    /// - *old_info* is the previous state that has been confirmed by the server
    /// - *new_info* represents the new state that was received
    fn on_gap(
        &mut self,
        _old_info: &TickInfo<Self>,
        _new_info: &TickInfo<Self>,
    ) -> Result<(), Self::Error> {
        Ok(())
    }

    /// User-provided callback when the state is fast-forwarded.
    /// This happens when the state received from the server is ahead of our own local state. The simulation will not recompute the missing states, and will instead assume the received state as the new source of truth.
    ///
    /// - *old_info* represents the latest state of the simulation before the server state was received
    /// - *new_info* represents the new state that was received
    fn on_fast_forward(
        &mut self,
        _old_info: &TickInfo<Self>,
        _new_info: &TickInfo<Self>,
    ) -> Result<(), Self::Error> {
        Ok(())
    }
}

#[derive(serde::Serialize, Clone)]
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

/// Trait that does nothing except require anchor ser and deser when the feature is active
pub trait Anchor {}

#[cfg(feature = "anchor")]
impl<T> Anchor for T
where
    T: anchor_lang::AnchorSerialize + anchor_lang::AnchorDeserialize,
{}

#[cfg(not(feature = "anchor"))]
impl<T> Anchor for T {}

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
    + Anchor
{
    /// When inputs are predicted, some actions may not make sense to be repeated, such as one-off toggles. Using this, you can decide for yourself to just implement a simple .clone() or, instead, reset some attributes before returning the inputs.
    ///
    /// By default, all inputs just get cloned.
    fn predict(&self) -> Self {
        self.clone()
    }
}

pub trait DeformGameState:
    Clone
    + Send
    + serde::Serialize
    + for<'de> SchemaRead<'de, DefaultConfig, Dst = Self>
    + SchemaWrite<DefaultConfig, Src = Self>
    + MaxLen
{
    fn new(players: &HashSet<Pubkey>) -> Self;
}

pub trait MaxLen {
    fn max_len() -> DeformResult<usize>;
}
