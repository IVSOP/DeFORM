//! Pong as a C library: the concrete instantiation of [`deform_ffi`]'s generic bindings.
//!
//! Each macro stamps out `#[unsafe(no_mangle)]` symbols prefixed with `pong_`, which is all
//! this crate is -- the implementations live in `deform_ffi`. A game that needs a surface
//! these macros do not cover writes its own wrappers over the same generic functions.

use pong::pong_logic::{PongGame, pong_bot};

deform_ffi::export_common!(pong, PongGame);

#[cfg(feature = "offline")]
deform_ffi::export_offline_client!(pong, PongGame, pong_bot);

#[cfg(feature = "quic")]
deform_ffi::export_quic_client!(pong, pong::pong_logic::PongQuicLogic);

#[cfg(feature = "foc")]
deform_ffi::export_foc_client!(
    pong,
    pong::pong_logic::PongFocLogic,
    pong::solana::anchor_client::PongAnchorClient
);
