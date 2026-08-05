use std::collections::BTreeMap;
#[cfg(feature = "client")]
use std::fmt::{Debug, Display};

pub use deform_derive::Smooth;
use wincode::{config::DefaultConfig, SchemaRead, SchemaWrite};

pub mod accounts;
#[cfg(feature = "client")]
pub mod client;
pub mod error;
pub mod game_program_client;
pub mod smooth;

#[cfg(feature = "client")]
pub use client::{DeformClient, DeformSharedBackendState, Stats};
pub use error::{DeformError, DeformResult};
pub use smooth::{NoopSmoother, Smooth, SmoothParams, Smoothable, SmoothableField};

use crate::accounts::lobby::{not_started::LobbyNotStarted, LobbyMetadata, ValidatorNetwork};

/// I like calling it a pubkey
pub type Pubkey = solana_address::Address;

/// Trait that defines what data types the game uses, as well as the logic functions/callbacks.
///
/// To use this crate, you should start by implementing this type, and then making a [`DeformClient`].
/// Note that the callbacks have mutable access to `self`, meaning you can provide and mutate your own data,
/// using the struct that implements this trait.
///
/// Types that implement this trait may also contain state that is separate from the game state. The main difference is that, every tick, a new game state will be created, and others may be deleted or overwritten, but [`DeformUserLogic`] objects will be reused as long as the match lives.
///
/// NOTE: this should be as quick to serialize as possible, as serialization/deserialization happens many times per second.
// TODO: make the callbacks receive the entire lobby state instead of just the game state??
pub trait DeformUserLogic:
    Debug
    + Clone
    + Send
    + Sync
    + 'static
    + for<'de> SchemaRead<'de, DefaultConfig, Dst = Self>
    + SchemaWrite<DefaultConfig, Src = Self>
    // NOTE: intentionally only `Serialize`, not `Deserialize`. These types are only ever
    // serde-*serialized* (e.g. `serde_json::to_value` for display); deserialization goes
    // through wincode (`SchemaRead`). Requiring `DeserializeOwned` here would duplicate the
    // `T: Deserialize<'de>` bound that `#[derive(Deserialize)]` already generates on every
    // generic type, causing an E0283 ambiguity (HRTB supertrait bound vs. serde's own bound).
    // NOTE: TLDR: everything blows up if you make this be Deserialize. It doesn't make a lot of sense for this
    + MaybeSerdeSerialize
{
    // user must define inputs and game state
    type Inputs: DeformInputs;

    /// NOTE: this should be as quick to serialize as possible, as serialization/deserialization happens many times per second.
    type GameState: DeformGameState;
    type Smoother: Smooth<Self::GameState>;
    type Error: std::error::Error
        + Send
        + Sync
        + 'static
        + serde::Serialize
        + for<'de> SchemaRead<'de, DefaultConfig, Dst = Self::Error>
        + SchemaWrite<DefaultConfig, Src = Self::Error>
        + Clone
        + Display;

    /// Microseconds per simulation tick. For 60 fps, use `16667`.
    const TICK_RATE_MICROS: u64 = 16667;

    // TODO: this is really ugly
    /// Ephemeral rollups are very finicky when it comes to resizing accounts, and the docs are not clear.
    /// Thus, I want to have accounts at their max size before delegating them.
    /// This means I need to know the max possible size of the game state and the inputs.
    ///
    /// A possible solution would be to have the user provide an instance of the object that contains the max possible size,
    /// but this has other limitations, such as
    /// - needing data to construct the instance
    /// - not able to be generous, and having to exactly provide the biggest possible data instance is very limiting
    ///
    /// As such, I have the user specify in number of serialized bytes. I also cannot have the user specify max bytes for game state/inputs only,
    /// as this would easily break for types that are dynamic, have enums, etc, and even if that were not the case I would have to guess how wincode is serializing things
    const MAX_INPUTS_ACCOUNT_BYTES: u64 = 1024;
    const MAX_INPUTS: u64 = 32;
    const MAX_LOBBY_ACCOUNT_BYTES: u64 = 1024;

    fn new_from_lobby(
        lobby_metadata: &LobbyMetadata,
        not_started: &LobbyNotStarted,
    ) -> Result<Self, Self::Error>;
    fn new_game_from_lobby(
        lobby_metadata: &LobbyMetadata,
        not_started: &LobbyNotStarted,
    ) -> Result<Self::GameState, Self::Error>;

    /// User-provided callback to advance the game state. From a certain state and inputs, it must compute the next state.
    ///
    /// When an error is returned, it is broadcasted to the clients and the match ends.
    // TODO: use TickInfo instead?
    // TODO: return an enum instead of result, to specify if match should end for example, or error transmited but match keeps going
    fn advance_frame(
        &mut self,
        state: &Self::GameState,
        inputs: &BTreeMap<Pubkey, Self::Inputs>,
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

    // There is no mechanism to know, on-chain, how long each slot lasts
    // I also can't ask the player, as he could use that to do bad things
    // At the same time I want the user to be able to override this at any time
    // So, I provide a default here, but the user can do whatever he wants
    fn get_micros_per_slot(network: &ValidatorNetwork) -> u64 {
        match network {
            ValidatorNetwork::Localhost(_) => 50000, // 20hz // 16667, // 60hz
            ValidatorNetwork::Devnet(_) => 50000,    // 20hz
            ValidatorNetwork::Mainnet(_) => 50000,   // 20hz
        }
    }
}

/// Information on a certain tick of a game
#[derive(Debug, Clone, SchemaRead, SchemaWrite)]
#[cfg_attr(not(target_arch = "bpf"), derive(serde::Serialize))]
pub struct TickInfo<T: DeformUserLogic> {
    /// The current game state at this tick
    pub game_state: T::GameState,
    // TODO: this is probably really bad.
    // I had to do this to prevent the user from having to store inputs inside of the GameState which would be even worse
    // but hashing pubkeys like this is going to use a lot of memory and be very slow. It would prob be better to use player IDs
    // FIX: need to serialize these pubkeys as B64
    /// The inputs that, combined with the previous state, have led to this new state.
    // TODO: in sbf, this should use pinocchio pubkey
    pub inputs: BTreeMap<Pubkey, T::Inputs>,
}

/// Trait that does nothing except require anchor ser and deser when the feature is active
pub trait MaybeAnchor {}

#[cfg(feature = "anchor")]
impl<T> MaybeAnchor for T where T: anchor_lang::AnchorSerialize + anchor_lang::AnchorDeserialize {}

#[cfg(not(feature = "anchor"))]
impl<T> MaybeAnchor for T {}

/// Trait that does nothing except require serde `Serialize` when not building for bpf.
#[cfg(not(target_arch = "bpf"))]
pub trait MaybeSerdeSerialize: serde::Serialize {}
#[cfg(not(target_arch = "bpf"))]
impl<T> MaybeSerdeSerialize for T where T: serde::Serialize {}

#[cfg(target_arch = "bpf")]
pub trait MaybeSerdeSerialize {}
#[cfg(target_arch = "bpf")]
impl<T> MaybeSerdeSerialize for T {}

/// Trait that does nothing except require serde `DeserializeOwned` when not building for bpf.
#[cfg(not(target_arch = "bpf"))]
pub trait MaybeSerdeDeserialize: serde::de::DeserializeOwned {}
#[cfg(not(target_arch = "bpf"))]
impl<T> MaybeSerdeDeserialize for T where T: serde::de::DeserializeOwned {}

#[cfg(target_arch = "bpf")]
pub trait MaybeSerdeDeserialize {}
#[cfg(target_arch = "bpf")]
impl<T> MaybeSerdeDeserialize for T {}

/// Inputs used by a game.
///
/// NOTE: `Eq` may not work well with floats; you may wish to manually override it.
/// For example, you may want inputs with a difference of <0.000001 to not be considered different inputs.
/// TODO: define a different trait method for this??
pub trait DeformInputs:
    Default
    + Debug
    + Eq
    + Clone
    + Send
    + Sync
    + 'static
    + serde::Serialize
    + for<'de> SchemaRead<'de, DefaultConfig, Dst = Self>
    + SchemaWrite<DefaultConfig, Src = Self>
    + MaybeAnchor
    + MaybeSerdeSerialize
    + MaybeSerdeDeserialize
{
    /// When inputs are predicted, some actions may not make sense to be repeated, such as one-off toggles. Using this, you can decide for yourself to just implement a simple .clone() or, instead, reset some attributes before returning the inputs.
    ///
    /// By default, all inputs just get cloned.
    fn predict(&self) -> Self {
        self.clone()
    }

    /// Combines a later sample from the same tick into this one. The game engine usually
    /// runs faster than the simulation, so several inputs can be provided within one tick,
    /// and only one of them is ever applied.
    ///
    /// By default the newest sample wins. Override this to OR button presses together, so
    /// a button pressed and released inside a single tick is not lost.
    fn merge(&mut self, newer: &Self) {
        *self = newer.clone();
    }
}

pub trait DeformGameState:
    Clone
    + Debug
    + Send
    + Sync
    + serde::Serialize
    + for<'de> SchemaRead<'de, DefaultConfig, Dst = Self>
    + SchemaWrite<DefaultConfig, Src = Self>
    + MaybeSerdeSerialize
{
    fn has_ended(&self) -> bool;
}
