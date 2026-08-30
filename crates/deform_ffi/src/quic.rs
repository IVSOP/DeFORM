use deform_quic::{DeformQuicLogic, netem::FakeNetwork};
use tokio_util::sync::CancellationToken;

use crate::{
    buffer::{ByteBuffer, json_error},
    client::{leak_client, lobby_from_account_bytes, pubkey_from_buffer},
};

/// Maps a `FakeNetwork` variant name (case-insensitive, `_` and `-` ignored) to the variant.
/// Only reachable through the debug-only `fake_network` argument.
fn parse_fake_network(name: &str) -> Result<FakeNetwork, String> {
    let normalized: String = name
        .chars()
        .filter(|c| *c != '_' && *c != '-')
        .flat_map(char::to_lowercase)
        .collect();

    match normalized.as_str() {
        "wifi" => Ok(FakeNetwork::Wifi),
        "wifilossy" => Ok(FakeNetwork::WifiLossy),
        "wificongested" => Ok(FakeNetwork::WifiCongested),
        "cellular" => Ok(FakeNetwork::Cellular),
        "satellite" => Ok(FakeNetwork::Satellite),
        "congested" => Ok(FakeNetwork::Congested),
        "good50ms" => Ok(FakeNetwork::Good50Ms),
        "stresstest" => Ok(FakeNetwork::StressTest),
        other => Err(format!("unknown fake network {other:?}")),
    }
}

/// Connects the QUIC backend to `server_addr` and returns a leaked client handle.
///
/// `lobby_account` is the raw lobby PDA account data, i.e. what `getAccountInfo` returns.
///
/// Empty buffers mean "default": `server_name` falls back to the host part of
/// `server_addr` (what the certificate is normally issued for), `auth` to a zero-byte
/// `Q::Auth`, and `fake_network` to the real network. `auth` is wincode, not JSON, because
/// `Q::Auth` is only required to implement wincode's schema traits.
///
/// # Safety
/// Every [`ByteBuffer`] argument must point to initialized memory of its stated length for
/// the duration of the call.
#[allow(clippy::too_many_arguments)]
pub unsafe fn new_quic_client<Q: DeformQuicLogic>(
    lobby_account: ByteBuffer,
    player: ByteBuffer,
    server_addr: ByteBuffer,
    server_name: ByteBuffer,
    skip_cert_verify: u8,
    visual_tick_micros: u64,
    auth: ByteBuffer,
    fake_network: ByteBuffer,
) -> ByteBuffer {
    let lobby = match unsafe { lobby_from_account_bytes::<Q::UserLogic>(&lobby_account) } {
        Ok(lobby) => lobby,
        Err(e) => return json_error(e),
    };

    let player = match unsafe { pubkey_from_buffer(&player, "player") } {
        Ok(player) => player,
        Err(e) => return json_error(e),
    };

    let server_addr = match unsafe { server_addr.as_str() } {
        Ok(addr) if !addr.trim().is_empty() => addr.trim().to_string(),
        Ok(_) => return json_error("server_addr is empty"),
        Err(e) => return json_error(format!("server_addr is not utf-8: {e}")),
    };

    let server_name = match unsafe { server_name.as_str() } {
        Ok(name) if !name.trim().is_empty() => name.trim().to_string(),
        Ok(_) => server_addr
            .split(':')
            .next()
            .unwrap_or(&server_addr)
            .to_string(),
        Err(e) => return json_error(format!("server_name is not utf-8: {e}")),
    };

    let auth: Q::Auth = match wincode::deserialize(unsafe { auth.as_slice() }) {
        Ok(auth) => auth,
        Err(e) => return json_error(format!("deserialize auth: {e:?}")),
    };

    let fake_network = match unsafe { fake_network.as_str() } {
        Ok(name) if !name.trim().is_empty() => match parse_fake_network(name.trim()) {
            Ok(network) => Some(network),
            Err(e) => return json_error(e),
        },
        Ok(_) => None,
        Err(e) => return json_error(format!("fake_network is not utf-8: {e}")),
    };

    match deform_quic::new_quic_client::<Q>(
        server_addr,
        server_name,
        lobby,
        player,
        skip_cert_verify != 0,
        visual_tick_micros,
        auth,
        CancellationToken::new(),
        fake_network,
    ) {
        Ok(client) => leak_client(client),
        Err(e) => json_error(e),
    }
}
