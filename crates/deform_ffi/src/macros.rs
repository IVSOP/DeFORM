/// Exports the backend-agnostic surface for one game: reading state, setting inputs,
/// shutting down and freeing the handle.
///
/// ```ignore
/// deform_ffi::export_common!(pong, pong::pong_logic::PongGame);
/// ```
///
/// emits `pong_read_state`, `pong_set_inputs`, `pong_shutdown` and `pong_free_client`.
/// Invoke it once per game, alongside at least one `export_*_client!`.
#[macro_export]
macro_rules! export_common {
    ($prefix:ident, $logic:ty) => {
        $crate::paste::paste! {
            /// Returns `{"data": {"lobby": ..., "stats": ...}}`, or `{"error": ...}` if the
            /// backend has failed. Free the result with `deform_free_bytes`.
            ///
            /// # Safety
            /// `client` must be a live handle from one of this game's `new_*_client` exports.
            #[unsafe(no_mangle)]
            pub extern "C" fn [<$prefix _read_state>](
                client: *mut ::core::ffi::c_void,
            ) -> $crate::ByteBuffer {
                unsafe { $crate::read_state::<$logic>(client) }
            }

            /// Queues JSON-encoded inputs on the backend. Returns `{"data": null}`.
            /// Free the result with `deform_free_bytes`.
            ///
            /// # Safety
            /// `client` must be a live handle, and `inputs_json` must point to
            /// `inputs_json.size` readable bytes.
            #[unsafe(no_mangle)]
            pub extern "C" fn [<$prefix _set_inputs>](
                client: *mut ::core::ffi::c_void,
                inputs_json: $crate::ByteBuffer,
            ) -> $crate::ByteBuffer {
                unsafe { $crate::set_inputs::<$logic>(client, inputs_json) }
            }

            /// Cancels the backend. The handle stays readable; free it separately.
            ///
            /// # Safety
            /// `client` must be a live handle.
            #[unsafe(no_mangle)]
            pub extern "C" fn [<$prefix _shutdown>](
                client: *mut ::core::ffi::c_void,
            ) -> $crate::ByteBuffer {
                unsafe { $crate::shutdown::<$logic>(client) }
            }

            /// Cancels the backend and drops the handle.
            ///
            /// # Safety
            /// `client` must be a live handle, and must not be used again afterwards.
            #[unsafe(no_mangle)]
            pub extern "C" fn [<$prefix _free_client>](client: *mut ::core::ffi::c_void) {
                unsafe { $crate::free_client::<$logic>(client) }
            }
        }
    };
}

/// Exports `<prefix>_new_offline_client` for one game, driving every non-local player with
/// `$bot_fn`.
///
/// ```ignore
/// deform_ffi::export_offline_client!(pong, pong::pong_logic::PongGame, pong::pong_logic::pong_bot);
/// ```
#[cfg(feature = "offline")]
#[macro_export]
macro_rules! export_offline_client {
    ($prefix:ident, $logic:ty, $bot_fn:expr) => {
        $crate::paste::paste! {
            /// Starts the offline backend. `players` is a JSON array of base58 pubkeys
            /// whose first entry is the creator; `player` is the one this host drives.
            /// Returns `{"data": <handle>}`.
            ///
            /// # Safety
            /// Each buffer must point to as many readable bytes as its `size` says.
            #[unsafe(no_mangle)]
            pub extern "C" fn [<$prefix _new_offline_client>](
                player: $crate::ByteBuffer,
                players: $crate::ByteBuffer,
                lobby_id: u64,
                visual_tick_micros: u64,
            ) -> $crate::ByteBuffer {
                unsafe {
                    $crate::new_offline_client::<$logic>(
                        player,
                        players,
                        lobby_id,
                        $bot_fn,
                        visual_tick_micros,
                    )
                }
            }
        }
    };
}

/// Exports `<prefix>_new_quic_client` for one game.
///
/// ```ignore
/// deform_ffi::export_quic_client!(pong, pong::pong_logic::PongQuicLogic);
/// ```
#[cfg(feature = "quic")]
#[macro_export]
macro_rules! export_quic_client {
    ($prefix:ident, $quic_logic:ty) => {
        $crate::paste::paste! {
            /// Connects the QUIC backend. `lobby_account` is the raw lobby PDA account
            /// data; empty `server_name`/`auth`/`fake_network` buffers take the defaults.
            /// Returns `{"data": <handle>}`.
            ///
            /// # Safety
            /// Each buffer must point to as many readable bytes as its `size` says.
            #[unsafe(no_mangle)]
            pub extern "C" fn [<$prefix _new_quic_client>](
                lobby_account: $crate::ByteBuffer,
                player: $crate::ByteBuffer,
                server_addr: $crate::ByteBuffer,
                server_name: $crate::ByteBuffer,
                skip_cert_verify: u8,
                visual_tick_micros: u64,
                auth: $crate::ByteBuffer,
                fake_network: $crate::ByteBuffer,
            ) -> $crate::ByteBuffer {
                unsafe {
                    $crate::new_quic_client::<$quic_logic>(
                        lobby_account,
                        player,
                        server_addr,
                        server_name,
                        skip_cert_verify,
                        visual_tick_micros,
                        auth,
                        fake_network,
                    )
                }
            }
        }
    };
}

/// Exports `<prefix>_new_foc_client` for one game. `$program_client` is an expression
/// evaluated on every call to build the instruction builder.
///
/// ```ignore
/// deform_ffi::export_foc_client!(pong, pong::pong_logic::PongFocLogic, PongAnchorClient);
/// ```
#[cfg(feature = "foc")]
#[macro_export]
macro_rules! export_foc_client {
    ($prefix:ident, $foc_logic:ty, $program_client:expr) => {
        $crate::paste::paste! {
            /// Starts the fully-on-chain backend. `lobby_account` is the raw lobby PDA
            /// account data and must be a `FullyOnChain` lobby; empty `rpc_url`/`ws_url`
            /// and a zero `slot_time_micros` are derived from its validator network.
            /// Returns `{"data": <handle>}`.
            ///
            /// # Safety
            /// Each buffer must point to as many readable bytes as its `size` says.
            #[unsafe(no_mangle)]
            pub extern "C" fn [<$prefix _new_foc_client>](
                lobby_account: $crate::ByteBuffer,
                keypair: $crate::ByteBuffer,
                rpc_url: $crate::ByteBuffer,
                ws_url: $crate::ByteBuffer,
                visual_tick_micros: u64,
                slot_time_micros: u64,
            ) -> $crate::ByteBuffer {
                unsafe {
                    $crate::new_foc_client::<$foc_logic>(
                        lobby_account,
                        keypair,
                        $program_client,
                        rpc_url,
                        ws_url,
                        visual_tick_micros,
                        slot_time_micros,
                    )
                }
            }
        }
    };
}
