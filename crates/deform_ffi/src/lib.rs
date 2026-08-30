//! C ABI bindings for DeFORM's backends.
//!
//! A `#[no_mangle]` symbol cannot be generic, so nothing here is exported directly: this
//! crate holds the generic implementations, and each game crate stamps out its own
//! monomorphic symbols with the `export_*` macros.
//!
//! ```ignore
//! deform_ffi::export_common!(pong, PongGame);
//! deform_ffi::export_offline_client!(pong, PongGame, pong_bot);
//! deform_ffi::export_quic_client!(pong, PongQuicLogic);
//! deform_ffi::export_foc_client!(pong, PongFocLogic, PongAnchorClient);
//! ```
//!
//! Every fallible export returns a [`ByteBuffer`] holding one JSON object, either
//! `{"data": ...}` or `{"error": "..."}`, which the host frees with [`deform_free_bytes`].
//! The `new_*_client` exports put a leaked `DeformClient<T>` pointer in `data`, as an
//! integer; that handle is the first argument to every other export, and is released with
//! `<prefix>_free_client`.
//!
//! Pubkeys cross the boundary as base58 strings. Lobbies cross as the raw account data of
//! the lobby PDA, the same bytes `getAccountInfo` returns -- except offline, which has no
//! account and builds its lobby from a player list instead.
//!
//! The macros are a convenience, not the interface. The generic functions are public, so a
//! game that wants different symbol names, extra arguments, or a hand-rolled auth path can
//! write its own wrappers against them.

mod buffer;
mod client;
mod macros;

pub use buffer::{
    ByteBuffer, deform_free_bytes, json_data, json_error, json_result, string_to_buffer,
};
pub use client::{
    free_client, leak_client, lobby_from_account_bytes, pubkey_from_buffer, read_state, set_inputs,
    shutdown,
};
/// Re-exported so the `export_*` macros can concatenate identifiers without the game crate
/// needing its own `paste` dependency.
#[doc(hidden)]
pub use paste;

#[cfg(feature = "offline")]
mod offline;
#[cfg(feature = "offline")]
pub use offline::new_offline_client;

#[cfg(feature = "quic")]
mod quic;
#[cfg(feature = "quic")]
pub use quic::new_quic_client;

#[cfg(feature = "foc")]
mod foc;
#[cfg(feature = "foc")]
pub use foc::new_foc_client;
