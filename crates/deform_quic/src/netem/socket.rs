use std::{
    io,
    net::{IpAddr, SocketAddr},
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
    time::{Duration, Instant},
};

use quinn::{
    AsyncUdpSocket, UdpPoller,
    udp::{EcnCodepoint, RecvMeta, Transmit},
};
use str0m_netem::{Input, Netem, NetemConfig, Output, WithLen};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

/// How often the drain task reports what it has been doing.
const STATS_INTERVAL: Duration = Duration::from_secs(5);

/// Upper bound on how long the drain task will park when its queue is empty.
/// [`Netem::poll_timeout`] returns a decade-out sentinel in that case, which is well
/// past what tokio's timer wheel handles gracefully.
const MAX_IDLE: Duration = Duration::from_secs(3600);

/// An owned copy of a [`Transmit`], since `Transmit::contents` only borrows and we
/// need to hold the bytes across the emulated delay.
struct QueuedTx {
    destination: SocketAddr,
    src_ip: Option<IpAddr>,
    ecn: Option<EcnCodepoint>,
    contents: Vec<u8>,
}

impl WithLen for QueuedTx {
    fn len(&self) -> usize {
        self.contents.len()
    }
}

// `Netem<T>` requires `T: Clone` because duplication hands out a second copy.
impl Clone for QueuedTx {
    fn clone(&self) -> Self {
        Self {
            destination: self.destination,
            src_ip: self.src_ip,
            ecn: self.ecn,
            contents: self.contents.clone(),
        }
    }
}

/// Wraps a real socket and pushes every outgoing datagram through a network emulator.
#[derive(Debug)]
pub struct FakeSocket<S: AsyncUdpSocket + ?Sized> {
    inner: Arc<S>,
    /// Where `try_send` puts a datagram instead of sending it.
    outgoing: mpsc::UnboundedSender<QueuedTx>,
}

impl<S: AsyncUdpSocket + ?Sized> FakeSocket<S> {
    /// Wrap `inner`, spawning the drain task that owns the emulator.
    ///
    /// Must be called from inside a tokio runtime: it panics otherwise, the same way
    /// [`tokio::spawn`] does. The drain task stops when `cancellation_token` fires or
    /// when the last clone of this socket is dropped, whichever comes first.
    pub fn wrap(
        inner: Arc<S>,
        config: NetemConfig,
        cancellation_token: CancellationToken,
    ) -> Arc<Self> {
        let (outgoing, incoming) = mpsc::unbounded_channel();

        info!(
            ?config,
            "netem active on this endpoint's egress; the other direction is only \
             degraded if the peer is wrapped too",
        );

        tokio::spawn(drain(
            inner.clone(),
            incoming,
            Netem::new(config),
            cancellation_token,
        ));

        Arc::new(Self { inner, outgoing })
    }
}

impl<S: AsyncUdpSocket + ?Sized> AsyncUdpSocket for FakeSocket<S> {
    fn try_send(&self, transmit: &Transmit) -> io::Result<()> {
        // A GSO transmit carries several datagrams back to back. Split them so loss
        // and reordering act per datagram rather than per batch. `max_transmit_segments`
        // below tells quinn not to build these in the first place; this is belt and
        // braces in case it ever does anyway.
        let segments = match transmit.segment_size {
            Some(size) if size > 0 => transmit.contents.chunks(size),
            _ => transmit.contents.chunks(transmit.contents.len().max(1)),
        };

        for segment in segments {
            // The channel only closes once the drain task has stopped, after which
            // nothing we hand over would ever reach the wire. Swallowing that would
            // leave quinn believing it is still sending, so report it: a non-WouldBlock
            // error is fatal to the connection, which is the truth of the situation.
            if self
                .outgoing
                .send(QueuedTx {
                    destination: transmit.destination,
                    src_ip: transmit.src_ip,
                    ecn: transmit.ecn,
                    contents: segment.to_vec(),
                })
                .is_err()
            {
                return Err(io::Error::other("netem drain task has stopped"));
            }
        }

        Ok(())
    }

    fn poll_recv(
        &self,
        cx: &mut Context,
        bufs: &mut [io::IoSliceMut<'_>],
        meta: &mut [RecvMeta],
    ) -> Poll<io::Result<usize>> {
        self.inner.poll_recv(cx, bufs, meta)
    }

    fn local_addr(&self) -> io::Result<SocketAddr> {
        self.inner.local_addr()
    }

    fn create_io_poller(self: Arc<Self>) -> Pin<Box<dyn UdpPoller>> {
        // `try_send` queues into an unbounded channel and never reports WouldBlock,
        // so a send is always allowed to proceed.
        Box::pin(AlwaysWritable)
    }

    /// Forcing 1 disables GSO, so each [`Transmit`] is exactly one datagram and the
    /// emulator's per-packet decisions mean what they say.
    fn max_transmit_segments(&self) -> usize {
        1
    }

    fn max_receive_segments(&self) -> usize {
        self.inner.max_receive_segments()
    }

    fn may_fragment(&self) -> bool {
        self.inner.may_fragment()
    }
}

#[derive(Debug)]
struct AlwaysWritable;

impl UdpPoller for AlwaysWritable {
    fn poll_writable(self: Pin<&mut Self>, _cx: &mut Context) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

/// Owns the emulator and forwards packets to the real socket once they come due.
///
/// This is the whole reason the emulator can be sans-IO: it hands us decisions
/// (`Output::Packet` now, `Output::Timeout` later) and we supply the clock and the I/O.
async fn drain<S: AsyncUdpSocket + ?Sized>(
    inner: Arc<S>,
    mut incoming: mpsc::UnboundedReceiver<QueuedTx>,
    mut netem: Netem<QueuedTx>,
    cancellation_token: CancellationToken,
) {
    let mut poller = inner.clone().create_io_poller();
    let mut stats = tokio::time::interval(STATS_INTERVAL);
    stats.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    let (mut queued, mut sent) = (0u64, 0u64);

    loop {
        let deadline = tokio::time::Instant::now()
            + netem
                .poll_timeout()
                .saturating_duration_since(Instant::now())
                .min(MAX_IDLE);

        tokio::select! {
            _ = cancellation_token.cancelled() => break,
            packet = incoming.recv() => match packet {
                Some(packet) => {
                    queued += 1;
                    netem.handle_input(Input::Packet(Instant::now(), packet));
                }
                // Every `FakeSocket` holding the sender is gone; so is the endpoint.
                None => break,
            },
            _ = tokio::time::sleep_until(deadline) => {
                netem.handle_input(Input::Timeout(Instant::now()));
            }
            _ = stats.tick() => {
                if queued > 0 {
                    debug!(
                        queued,
                        sent,
                        dropped = queued.saturating_sub(sent).saturating_sub(netem.queue_len() as u64),
                        in_flight = netem.queue_len(),
                        "netem",
                    );
                }
                continue;
            }
        }

        while let Some(output) = netem.poll_output() {
            // `Output::Timeout` is only a hint; `poll_timeout` above already gives us
            // the same deadline at the top of every iteration.
            let Output::Packet(packet) = output else {
                continue;
            };

            if send(&inner, &mut poller, &packet).await.is_err() {
                warn!("netem drain task stopping: socket send failed");
                return;
            }
            sent += 1;
        }
    }

    debug!(queued, sent, "netem drain task finished");
}

/// Send one datagram, waiting for writability if the kernel buffer is full.
async fn send<S: AsyncUdpSocket + ?Sized>(
    inner: &Arc<S>,
    poller: &mut Pin<Box<dyn UdpPoller>>,
    packet: &QueuedTx,
) -> io::Result<()> {
    loop {
        let transmit = Transmit {
            destination: packet.destination,
            ecn: packet.ecn,
            contents: &packet.contents,
            segment_size: None,
            src_ip: packet.src_ip,
        };

        match inner.try_send(&transmit) {
            Ok(()) => return Ok(()),
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                std::future::poll_fn(|cx| poller.as_mut().poll_writable(cx)).await?;
            }
            Err(e) => return Err(e),
        }
    }
}
