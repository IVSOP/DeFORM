use std::{io, net::SocketAddr, time::Duration};

use tokio_util::sync::CancellationToken;

mod socket;

pub use socket::FakeSocket;
pub use str0m_netem::{
    Bitrate, DataSize, GilbertElliot, LossModel, NetemConfig, Probability, RandomLoss,
};

#[cfg_attr(
    feature = "egui-probe",
    derive(egui_probe::EguiProbe),
    egui_probe(tags combobox)
)]
#[derive(Debug, Clone, Copy, Default)]
pub enum FakeNetwork {
    /// Good signal: 5ms, ~1% bursty loss, 100 Mbps.
    #[default]
    Wifi,
    /// Weak signal: 15ms, ~5% bursty loss, 50 Mbps.
    WifiLossy,
    /// Shared with a video stream: 10ms, 20ms jitter, ~10% loss, 5 Mbps.
    WifiCongested,
    /// LTE/4G: 50ms, ~2% bursty loss from handoffs, 30 Mbps.
    Cellular,
    /// Geostationary satellite: 600ms, ~3% bursty loss, 15 Mbps.
    Satellite,
    /// Severely congested path: 80ms, 40ms jitter, ~10% loss, 10 Mbps.
    Congested,
    /// Clean 50ms RTT: no loss, no reordering, 2ms jitter, unmetered. Isolates
    /// distance from damage.
    Good50Ms,
    /// As bad as a link gets while still being playable: 60ms +/- 25ms correlated
    /// jitter (so never past a 100ms RTT), ~10% bursty loss, every 10th packet
    /// reordered, 1% duplicated. Unmetered on purpose --- a bottleneck's queueing
    /// delay would push the round trip past the 100ms this promises.
    StressTest,
}

impl From<FakeNetwork> for NetemConfig {
    fn from(network: FakeNetwork) -> Self {
        match network {
            FakeNetwork::Wifi => NetemConfig::wifi(),
            FakeNetwork::WifiLossy => NetemConfig::wifi_lossy(),
            FakeNetwork::WifiCongested => NetemConfig::wifi_congested(),
            FakeNetwork::Cellular => NetemConfig::cellular(),
            FakeNetwork::Satellite => NetemConfig::satellite(),
            FakeNetwork::Congested => NetemConfig::congested(),
            FakeNetwork::Good50Ms => NetemConfig::new()
                .latency(Duration::from_millis(50))
                .jitter(Duration::from_millis(2)),
            FakeNetwork::StressTest => NetemConfig::new()
                .latency(Duration::from_millis(60))
                .jitter(Duration::from_millis(25))
                .delay_correlation(Probability::new(0.5))
                .loss(GilbertElliot::congested())
                .duplicate(Probability::new(0.01))
                .reorder_gap(10),
        }
    }
}

pub fn fake_quic_endpoint(
    addr: SocketAddr,
    network: FakeNetwork,
    cancellation_token: CancellationToken,
) -> io::Result<quinn::Endpoint> {
    let runtime = quinn::default_runtime()
        .ok_or_else(|| io::Error::other("no async runtime found for the QUIC endpoint"))?;

    let socket = runtime.wrap_udp_socket(std::net::UdpSocket::bind(addr)?)?;
    let socket = FakeSocket::wrap(socket, network.into(), cancellation_token);

    quinn::Endpoint::new_with_abstract_socket(
        quinn::EndpointConfig::default(),
        None,
        socket,
        runtime,
    )
}
