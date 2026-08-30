use std::sync::Arc;

use deform_core::{DeformUserLogic, accounts::lobby::Network};
use deform_foc::DeformFocLogic;
use solana_sdk::signature::Keypair;
use tokio_util::sync::CancellationToken;

use crate::{
    buffer::{ByteBuffer, json_error},
    client::{leak_client, lobby_from_account_bytes},
};

/// Accepts either a 32-byte ed25519 seed or the 64-byte `[secret || public]` layout that
/// Solana keypair JSON files use.
fn keypair_from_bytes(bytes: &[u8]) -> Result<Keypair, String> {
    match bytes.len() {
        32 => {
            let seed: [u8; 32] = bytes.try_into().expect("checked length");
            Ok(Keypair::new_from_array(seed))
        }
        64 => Keypair::try_from(bytes).map_err(|e| format!("invalid 64-byte keypair: {e}")),
        len => Err(format!("keypair must be 32 or 64 bytes, got {len}")),
    }
}

/// Starts the fully-on-chain backend against the ephemeral rollup and returns a leaked
/// client handle.
///
/// `lobby_account` is the raw lobby PDA account data. It must be a `FullyOnChain` lobby --
/// its `ValidatorNetwork` is what the RPC and WebSocket endpoints default to, and what
/// `get_micros_per_slot` derives the slot time from.
///
/// Empty or zero arguments mean "derive from the lobby": `rpc_url`/`ws_url` fall back to
/// that network's `er_endpoints()`, and `slot_time_micros = 0` to
/// `T::get_micros_per_slot`. Pass them explicitly to point at a different validator.
///
/// # Safety
/// Every [`ByteBuffer`] argument must point to initialized memory of its stated length for
/// the duration of the call.
pub unsafe fn new_foc_client<F: DeformFocLogic>(
    lobby_account: ByteBuffer,
    keypair: ByteBuffer,
    program_client: F::ProgramClient,
    rpc_url: ByteBuffer,
    ws_url: ByteBuffer,
    visual_tick_micros: u64,
    slot_time_micros: u64,
) -> ByteBuffer {
    let lobby = match unsafe { lobby_from_account_bytes::<F::UserLogic>(&lobby_account) } {
        Ok(lobby) => lobby,
        Err(e) => return json_error(e),
    };

    let Network::FullyOnChain(validator_network) = &lobby.metadata.network else {
        return json_error("the FoC backend requires a FullyOnChain lobby");
    };

    let endpoints = validator_network.er_endpoints();
    let slot_time_micros = if slot_time_micros == 0 {
        <F::UserLogic as DeformUserLogic>::get_micros_per_slot(validator_network)
    } else {
        slot_time_micros
    };

    let rpc_url = match unsafe { rpc_url.as_str() } {
        Ok(url) if !url.trim().is_empty() => url.trim().to_string(),
        Ok(_) => endpoints.rpc.to_string(),
        Err(e) => return json_error(format!("rpc_url is not utf-8: {e}")),
    };

    let ws_url = match unsafe { ws_url.as_str() } {
        Ok(url) if !url.trim().is_empty() => url.trim().to_string(),
        Ok(_) => endpoints.ws.to_string(),
        Err(e) => return json_error(format!("ws_url is not utf-8: {e}")),
    };

    let keypair = match keypair_from_bytes(unsafe { keypair.as_slice() }) {
        Ok(keypair) => keypair,
        Err(e) => return json_error(e),
    };

    match deform_foc::new_foc_client::<F>(
        rpc_url,
        ws_url,
        Arc::new(keypair),
        program_client,
        lobby,
        visual_tick_micros,
        slot_time_micros,
        CancellationToken::new(),
    ) {
        Ok(client) => leak_client(client),
        Err(e) => json_error(e),
    }
}
