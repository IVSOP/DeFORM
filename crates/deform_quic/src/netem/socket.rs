use std::{
    collections::VecDeque,
    future::Future,
    io,
    net::{IpAddr, SocketAddr},
    pin::Pin,
    sync::{Arc, Mutex},
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

/// Scratch space for one read off the real socket.
const MAX_DATAGRAM: usize = 64 * 1024;

/// Cap on one `poll_recv`'s ingest, so a fast sender cannot hold the loop forever.
const RX_INGEST_BURST: usize = 64;

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

/// The same, for a datagram held back on its way in.
#[derive(Clone)]
struct QueuedRx {
    addr: SocketAddr,
    dst_ip: Option<IpAddr>,
    ecn: Option<EcnCodepoint>,
    contents: Vec<u8>,
}

impl WithLen for QueuedRx {
    fn len(&self) -> usize {
        self.contents.len()
    }
}

/// Egress gets a background task; ingress cannot, since quinn owns the only read on the
/// inner socket and a second reader would race it for datagrams. So [`poll_recv`] drives
/// the emulator itself, with `timer` standing in for the drain loop's sleep.
///
/// [`poll_recv`]: FakeSocket::poll_recv
struct RxState {
    netem: Netem<QueuedRx>,
    ready: VecDeque<QueuedRx>,
    timer: Pin<Box<tokio::time::Sleep>>,
    scratch: Vec<u8>,
}

/// Wraps a real socket and pushes every datagram, both directions, through a network
/// emulator.
pub struct FakeSocket<S: AsyncUdpSocket + ?Sized> {
    inner: Arc<S>,
    /// Where `try_send` puts a datagram instead of sending it.
    outgoing: mpsc::UnboundedSender<QueuedTx>,
    /// Guarded because `poll_recv` takes `&self`.
    rx: Mutex<RxState>,
}

// Hand-written so `RxState` does not have to be `Debug` too.
impl<S: AsyncUdpSocket + ?Sized> std::fmt::Debug for FakeSocket<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FakeSocket").finish_non_exhaustive()
    }
}

impl<S: AsyncUdpSocket + ?Sized> FakeSocket<S> {
    /// Wrap `inner`, spawning the drain task that owns the egress emulator. `config`
    /// applies to each direction separately.
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
            "netem active on both directions of this endpoint; the peer's own link is \
             only degraded if it is wrapped too",
        );

        tokio::spawn(drain(
            inner.clone(),
            incoming,
            Netem::new(config),
            cancellation_token,
        ));

        // A different seed than egress, or both directions would drop the same packet
        // indices.
        let rx = RxState {
            netem: Netem::new(config.seed(RX_SEED_OFFSET)),
            ready: VecDeque::new(),
            timer: Box::pin(tokio::time::sleep(MAX_IDLE)),
            scratch: vec![0; MAX_DATAGRAM],
        };

        Arc::new(Self {
            inner,
            outgoing,
            rx: Mutex::new(rx),
        })
    }
}

/// Arbitrary; only has to differ from the egress seed, which [`NetemConfig`] defaults.
const RX_SEED_OFFSET: u64 = 0x5eed_d01f;

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

    /// Reads off the real socket into the emulator, then hands quinn whatever it has
    /// released.
    fn poll_recv(
        &self,
        cx: &mut Context,
        bufs: &mut [io::IoSliceMut<'_>],
        meta: &mut [RecvMeta],
    ) -> Poll<io::Result<usize>> {
        let mut guard = self.rx.lock().unwrap_or_else(|e| e.into_inner());
        let rx = &mut *guard;

        let mut ingested = 0;
        let mut saturated = true;
        while ingested < RX_INGEST_BURST {
            let mut one_meta = [RecvMeta::default()];
            let polled = {
                let mut one_buf = [io::IoSliceMut::new(&mut rx.scratch)];
                self.inner.poll_recv(cx, &mut one_buf, &mut one_meta)
            };

            let count = match polled {
                // Registers our waker against the socket becoming readable.
                Poll::Pending => {
                    saturated = false;
                    break;
                }
                Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
                Poll::Ready(Ok(count)) => count,
            };

            if count == 0 {
                saturated = false;
                break;
            }

            let now = Instant::now();
            for m in &one_meta[..count] {
                // GRO coalesces datagrams into one buffer at `stride` boundaries. Split
                // them so loss acts per datagram, as on the send side.
                let stride = if m.stride == 0 { m.len } else { m.stride };
                for chunk in rx.scratch[..m.len].chunks(stride.max(1)) {
                    rx.netem.handle_input(Input::Packet(
                        now,
                        QueuedRx {
                            addr: m.addr,
                            dst_ip: m.dst_ip,
                            ecn: m.ecn,
                            contents: chunk.to_vec(),
                        },
                    ));
                }
                ingested += 1;
            }
        }

        rx.netem.handle_input(Input::Timeout(Instant::now()));
        while let Some(output) = rx.netem.poll_output() {
            // `Output::Timeout` is only a hint; `poll_timeout` below says the same.
            if let Output::Packet(packet) = output {
                rx.ready.push_back(packet);
            }
        }

        let capacity = bufs.len().min(meta.len());
        let mut filled = 0;
        while filled < capacity {
            let Some(next) = rx.ready.front() else { break };

            // Dropping beats truncating: a partial datagram reads as corruption.
            if next.contents.len() > bufs[filled].len() {
                warn!(
                    len = next.contents.len(),
                    "netem dropping oversized datagram"
                );
                rx.ready.pop_front();
                continue;
            }

            let packet = rx.ready.pop_front().expect("front just peeked");
            let len = packet.contents.len();
            bufs[filled][..len].copy_from_slice(&packet.contents);
            meta[filled] = RecvMeta {
                addr: packet.addr,
                len,
                stride: len,
                ecn: packet.ecn,
                dst_ip: packet.dst_ip,
            };
            filled += 1;
        }

        if filled > 0 {
            return Poll::Ready(Ok(filled));
        }

        // Nothing to hand over, and `Pending` owes the caller a wakeup. Above we only
        // got one for free if the socket ran dry.
        if saturated || !rx.ready.is_empty() {
            cx.waker().wake_by_ref();
            return Poll::Pending;
        }

        let due = rx
            .netem
            .poll_timeout()
            .saturating_duration_since(Instant::now())
            .min(MAX_IDLE);
        rx.timer.as_mut().reset(tokio::time::Instant::now() + due);
        // Polling is what registers the waker; whether it is already due does not matter.
        let _ = rx.timer.as_mut().poll(cx);

        Poll::Pending
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

    /// Likewise 1 inbound: `poll_recv` hands over one datagram per slot.
    fn max_receive_segments(&self) -> usize {
        1
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

/// Owns the egress emulator and forwards packets to the real socket once they come due.
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
