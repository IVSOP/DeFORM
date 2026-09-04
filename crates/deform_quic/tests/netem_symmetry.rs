//! Only the client endpoint is wrapped, so if ingress were a passthrough the measured
//! round trip would cost one traversal instead of two. Each test stands up a real QUIC
//! pair over loopback and times an echo.

use std::{
    net::{Ipv4Addr, SocketAddr},
    sync::Arc,
    time::{Duration, Instant},
};

use deform_quic::{
    ALPN_PROTOCOL,
    netem::{NetemConfig, fake_quic_endpoint},
};
use tokio_util::sync::CancellationToken;

/// Large enough that scheduling noise cannot be mistaken for it.
const ONE_WAY: Duration = Duration::from_millis(60);

fn spawn_echo_server() -> (SocketAddr, rustls::pki_types::CertificateDer<'static>) {
    let cert = rcgen::generate_simple_self_signed(vec!["localhost".into()]).expect("cert");
    let cert_der = rustls::pki_types::CertificateDer::from(cert.cert);
    let key_der =
        rustls::pki_types::PrivateKeyDer::try_from(cert.signing_key.serialize_der()).expect("key");

    let mut tls = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(vec![cert_der.clone()], key_der)
        .expect("server tls");
    tls.alpn_protocols = vec![ALPN_PROTOCOL.to_vec()];

    let quic_tls = quinn::crypto::rustls::QuicServerConfig::try_from(tls).expect("quic tls");
    let endpoint = quinn::Endpoint::server(
        quinn::ServerConfig::with_crypto(Arc::new(quic_tls)),
        SocketAddr::from((Ipv4Addr::LOCALHOST, 0)),
    )
    .expect("server endpoint");
    let addr = endpoint.local_addr().expect("server addr");

    tokio::spawn(async move {
        while let Some(incoming) = endpoint.accept().await {
            tokio::spawn(async move {
                let conn = incoming.await.expect("accept");
                while let Ok((mut send, mut recv)) = conn.accept_bi().await {
                    let msg = recv.read_to_end(64).await.expect("read");
                    send.write_all(&msg).await.expect("write");
                    send.finish().expect("finish");
                }
            });
        }
    });

    (addr, cert_der)
}

async fn measure_rtt(config: Option<NetemConfig>) -> Duration {
    let (addr, cert_der) = spawn_echo_server();

    let mut roots = rustls::RootCertStore::empty();
    roots.add(cert_der).expect("add root");
    let mut tls = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    tls.alpn_protocols = vec![ALPN_PROTOCOL.to_vec()];

    let token = CancellationToken::new();
    let bind = SocketAddr::from((Ipv4Addr::LOCALHOST, 0));
    let mut endpoint = match config {
        Some(config) => fake_quic_endpoint(bind, config, token.clone()).expect("fake endpoint"),
        None => quinn::Endpoint::client(bind).expect("plain endpoint"),
    };
    endpoint.set_default_client_config(quinn::ClientConfig::new(Arc::new(
        quinn::crypto::rustls::QuicClientConfig::try_from(tls).expect("quic client tls"),
    )));

    let conn = endpoint
        .connect(addr, "localhost")
        .expect("connect")
        .await
        .expect("handshake");

    // So the timed exchange does not pay for handshake leftovers.
    for _ in 0..2 {
        let (mut send, mut recv) = conn.open_bi().await.expect("open");
        send.write_all(b"ping").await.expect("send");
        send.finish().expect("finish");
        recv.read_to_end(64).await.expect("recv");
    }

    let started = Instant::now();
    let (mut send, mut recv) = conn.open_bi().await.expect("open");
    send.write_all(b"ping").await.expect("send");
    send.finish().expect("finish");
    let echoed = recv.read_to_end(64).await.expect("recv");
    let elapsed = started.elapsed();

    assert_eq!(echoed, b"ping", "echo came back wrong");
    token.cancel();
    elapsed
}

/// The floor the emulated numbers below are measured against.
#[tokio::test]
async fn unwrapped_loopback_is_fast() {
    let rtt = measure_rtt(None).await;
    assert!(
        rtt < Duration::from_millis(30),
        "bare loopback should be near-instant, took {rtt:?}"
    );
}

/// A passthrough `poll_recv` lands near `ONE_WAY` instead of twice it.
#[tokio::test]
async fn latency_applies_to_both_directions() {
    let rtt = measure_rtt(Some(NetemConfig::new().latency(ONE_WAY))).await;

    let expected = ONE_WAY * 2;
    assert!(
        rtt >= expected,
        "round trip {rtt:?} is below the {expected:?} the two traversals cost; \
         is poll_recv bypassing the emulator?"
    );
    assert!(
        rtt < expected + Duration::from_millis(60),
        "round trip {rtt:?} is far past the expected {expected:?}"
    );
}

/// A held-back packet has to wake the reader on its own; nothing else will poll the
/// socket while the emulator sits on the only datagram in flight.
#[tokio::test]
async fn delivery_survives_loss_and_jitter() {
    let config = NetemConfig::new()
        .latency(Duration::from_millis(20))
        .jitter(Duration::from_millis(10))
        .loss(deform_quic::netem::RandomLoss::new(
            deform_quic::netem::Probability::new(0.10),
        ));

    // Would hang rather than fail if the ingress timer never fired.
    let rtt = tokio::time::timeout(Duration::from_secs(10), measure_rtt(Some(config)))
        .await
        .expect("echo never came back: ingress packets are not being released");

    assert!(
        rtt >= Duration::from_millis(40),
        "round trip {rtt:?} too fast"
    );
}
