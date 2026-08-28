use std::{io, net::SocketAddr};

use tokio_util::sync::CancellationToken;

mod socket;

pub use socket::FakeSocket;
pub use str0m_netem::{
    Bitrate, DataSize, GilbertElliot, LossModel, NetemConfig, Probability, RandomLoss,
};

pub fn fake_quic_endpoint(
    addr: SocketAddr,
    config: NetemConfig,
    cancellation_token: CancellationToken,
) -> io::Result<quinn::Endpoint> {
    let runtime = quinn::default_runtime()
        .ok_or_else(|| io::Error::other("no async runtime found for the QUIC endpoint"))?;

    let socket = runtime.wrap_udp_socket(std::net::UdpSocket::bind(addr)?)?;
    let socket = FakeSocket::wrap(socket, config, cancellation_token);

    quinn::Endpoint::new_with_abstract_socket(
        quinn::EndpointConfig::default(),
        None,
        socket,
        runtime,
    )
}

/// Reads `DEFORM_NETEM` and maps it to one of `NetemConfig`'s presets, or None
pub fn fake_network_from_env() -> Option<NetemConfig> {
    let raw = std::env::var("DEFORM_NETEM").ok()?.to_lowercase();
    if raw.is_empty() {
        return None;
    }

    Some(match raw.as_str() {
        "wifi" => NetemConfig::wifi(),
        "wifi_lossy" => NetemConfig::wifi_lossy(),
        "wifi_congested" => NetemConfig::wifi_congested(),
        "cellular" => NetemConfig::cellular(),
        "satellite" => NetemConfig::satellite(),
        "congested" => NetemConfig::congested(),
        // TODO: error
        _other => return None,
    })
}
