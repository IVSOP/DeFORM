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
    /// Good signal: 5ms RTT, ~1% bursty loss, 100 Mbps.
    #[default]
    Wifi,
    /// Weak signal: 15ms RTT, ~5% bursty loss, 50 Mbps.
    WifiLossy,
    /// Shared with a video stream: 10ms RTT, 20ms jitter, ~10% loss, 5 Mbps.
    WifiCongested,
    /// LTE/4G: 50ms RTT, ~2% bursty loss from handoffs, 30 Mbps.
    Cellular,
    /// Geostationary satellite: 600ms RTT, ~3% bursty loss, 15 Mbps.
    Satellite,
    /// Severely congested path: 80ms RTT, 40ms jitter, ~10% loss, 10 Mbps.
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

const fn one_way(rtt_micros: u64) -> Duration {
    Duration::from_micros(rtt_micros / 2)
}

// [`FakeSocket`] emulates both directions, so a packet pays the latency twice. Loss and
// link rate are not halved: those are per-traversal, same as on a real link.
impl From<FakeNetwork> for NetemConfig {
    fn from(network: FakeNetwork) -> Self {
        let base = NetemConfig::new();
        match network {
            FakeNetwork::Wifi => base
                .latency(one_way(5_000))
                .jitter(one_way(2_000))
                .loss(GilbertElliot::wifi())
                .link(Bitrate::mbps(100), DataSize::kbytes(200)),
            FakeNetwork::WifiLossy => base
                .latency(one_way(15_000))
                .jitter(one_way(10_000))
                .loss(GilbertElliot::wifi_lossy())
                .link(Bitrate::mbps(50), DataSize::kbytes(100)),
            FakeNetwork::WifiCongested => base
                .latency(one_way(10_000))
                .jitter(one_way(20_000))
                .loss(GilbertElliot::congested())
                .link(Bitrate::mbps(5), DataSize::kbytes(30)),
            FakeNetwork::Cellular => base
                .latency(one_way(50_000))
                .jitter(one_way(15_000))
                .loss(GilbertElliot::cellular())
                .link(Bitrate::mbps(30), DataSize::kbytes(100)),
            FakeNetwork::Satellite => base
                .latency(one_way(600_000))
                .jitter(one_way(30_000))
                .loss(GilbertElliot::satellite())
                .link(Bitrate::mbps(15), DataSize::kbytes(500)),
            FakeNetwork::Congested => base
                .latency(one_way(80_000))
                .jitter(one_way(40_000))
                .loss(GilbertElliot::congested())
                .link(Bitrate::mbps(10), DataSize::kbytes(50)),
            FakeNetwork::Good50Ms => base.latency(one_way(50_000)).jitter(one_way(2_000)),
            FakeNetwork::StressTest => base
                .latency(one_way(60_000))
                .jitter(one_way(25_000))
                .delay_correlation(Probability::new(0.5))
                .loss(GilbertElliot::congested())
                .duplicate(Probability::new(0.01))
                .reorder_gap(10),
        }
    }
}

/// A raw [`NetemConfig`] here is applied per direction; a [`FakeNetwork`] has already
/// halved itself.
pub fn fake_quic_endpoint(
    addr: SocketAddr,
    network: impl Into<NetemConfig>,
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
