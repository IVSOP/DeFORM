use std::{
    ffi::c_void,
    panic::{AssertUnwindSafe, catch_unwind},
    str::FromStr,
};

use deform_core::{DeformClient, DeformUserLogic, Pubkey, accounts::lobby::Lobby};

use crate::buffer::{ByteBuffer, json_data, json_error};

/// Boxes `client`, leaks the box, and returns its address in a success envelope:
/// `{"data": 94438402816}`. That integer is the handle every other export takes, and stays
/// valid until it is passed to [`free_client`].
pub fn leak_client<T: DeformUserLogic>(client: DeformClient<T>) -> ByteBuffer {
    json_data(&(Box::into_raw(Box::new(client)) as usize))
}

/// # Safety
/// `client` must be null, or a handle from a `new_*_client` export for this exact `T` that
/// has not been freed.
unsafe fn client_ref<'a, T: DeformUserLogic>(client: *mut c_void) -> Option<&'a DeformClient<T>> {
    unsafe { (client as *mut DeformClient<T>).as_ref() }
}

/// Runs `f`, converting a panic into an error envelope. Unwinding past an `extern "C"`
/// frame is undefined behaviour, so every export funnels through here.
fn guard(f: impl FnOnce() -> ByteBuffer) -> ByteBuffer {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(buffer) => buffer,
        Err(payload) => {
            let msg = payload
                .downcast_ref::<&str>()
                .map(|s| (*s).to_string())
                .or_else(|| payload.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "panic".to_string());

            json_error(format!("panic in deform ffi: {msg}"))
        }
    }
}

/// The whole backend state as JSON: `{"data": {"lobby": ..., "stats": {"ping_ms": ...}}}`.
///
/// A backend that has already failed reports that failure as the error envelope instead --
/// the lobby it left behind is stale by definition.
///
/// # Safety
/// See [`client_ref`].
pub unsafe fn read_state<T: DeformUserLogic>(client: *mut c_void) -> ByteBuffer {
    guard(|| {
        let Some(client) = (unsafe { client_ref::<T>(client) }) else {
            return json_error("null client handle");
        };

        let state = match client.read_state() {
            Ok(state) => state,
            Err(e) => return json_error(e),
        };

        if let Err(e) = &state.internal_error {
            return json_error(e);
        }

        json_data(&serde_json::json!({
            "lobby": &state.lobby,
            "stats": &state.stats,
        }))
    })
}

/// Deserializes `T::Inputs` from `inputs_json` and queues it on the backend.
/// Returns `{"data": null}` on success.
///
/// # Safety
/// See [`client_ref`] and [`ByteBuffer::as_slice`].
pub unsafe fn set_inputs<T: DeformUserLogic>(
    client: *mut c_void,
    inputs_json: ByteBuffer,
) -> ByteBuffer {
    guard(|| {
        let Some(client) = (unsafe { client_ref::<T>(client) }) else {
            return json_error("null client handle");
        };

        let json = match unsafe { inputs_json.as_str() } {
            Ok(json) => json,
            Err(e) => return json_error(format!("inputs are not utf-8: {e}")),
        };

        let inputs: T::Inputs = match serde_json::from_str(json) {
            Ok(inputs) => inputs,
            Err(e) => return json_error(format!("deserialize inputs: {e}")),
        };

        match client.set_inputs(inputs) {
            Ok(()) => json_data(&()),
            Err(e) => json_error(e),
        }
    })
}

/// Cancels the backend. The handle stays valid (and readable) afterwards; free it with
/// [`free_client`] once the host is done with it.
///
/// # Safety
/// See [`client_ref`].
pub unsafe fn shutdown<T: DeformUserLogic>(client: *mut c_void) -> ByteBuffer {
    guard(|| {
        let Some(client) = (unsafe { client_ref::<T>(client) }) else {
            return json_error("null client handle");
        };

        client.shutdown();
        json_data(&())
    })
}

/// Drops the leaked client. Cancels the backend first, so a host that forgot to call
/// [`shutdown`] does not leave the backend threads running.
///
/// # Safety
/// See [`client_ref`]. The handle must not be used again afterwards.
pub unsafe fn free_client<T: DeformUserLogic>(client: *mut c_void) {
    if client.is_null() {
        return;
    }

    let _ = catch_unwind(AssertUnwindSafe(|| {
        let client = unsafe { Box::from_raw(client as *mut DeformClient<T>) };
        client.shutdown();
        drop(client);
    }));
}

/// Parses a base58 pubkey out of a caller-owned buffer.
///
/// # Safety
/// See [`ByteBuffer::as_slice`].
pub unsafe fn pubkey_from_buffer(buffer: &ByteBuffer, what: &str) -> Result<Pubkey, String> {
    let s = unsafe { buffer.as_str() }.map_err(|e| format!("{what} is not utf-8: {e}"))?;

    Pubkey::from_str(s.trim()).map_err(|e| format!("{what} is not a base58 pubkey: {e}"))
}

/// Deserializes the `Lobby<T>` out of a raw lobby account, i.e. exactly the bytes
/// `getAccountInfo` returns for the lobby PDA.
///
/// # Safety
/// See [`ByteBuffer::as_slice`].
pub unsafe fn lobby_from_account_bytes<T: DeformUserLogic>(
    account: &ByteBuffer,
) -> Result<Lobby<T>, String> {
    use deform_core::accounts::DeformAccount;

    let bytes = unsafe { account.as_slice() };
    if bytes.is_empty() {
        return Err("lobby account data is empty".to_string());
    }

    match DeformAccount::<T>::from_bytes(bytes).map_err(|e| e.to_string())? {
        DeformAccount::Lobby(lobby) => Ok(lobby),
        DeformAccount::Inputs(_) => Err("account is an inputs account, not a lobby".to_string()),
    }
}
