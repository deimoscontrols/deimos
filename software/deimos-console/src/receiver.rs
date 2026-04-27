//! Background UDP multicast receiver thread.
//!
//! Joins the configured multicast group, reads incoming datagrams in a blocking
//! `recv_from` loop (with a 100 ms read timeout so the loop can observe a shutdown signal),
//! deserializes each datagram as a [`ReportingMessage`], and forwards it to the UI thread via a
//! bounded [`crossbeam_channel`].
//!
//! # Drop semantics on a full channel
//!
//! The viewer prefers freshness over completeness: when the bounded channel is at capacity
//! the receiver evicts the *oldest* queued message and enqueues the new one. The eviction is
//! counted in [`OVERWRITTEN_FRAMES`]. With a single writer the eviction-then-retry path
//! cannot fail; [`DROPPED_FRAMES`] is retained as a defensive attribution counter for any
//! future multi-writer extension and stays at zero in normal operation.

use std::io;
use std::net::{Ipv4Addr, SocketAddrV4, UdpSocket};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::Duration;

use crossbeam_channel::{Receiver, Sender, TrySendError};
use socket2::{Domain, Protocol, Socket, Type};

use deimos::dispatcher::ReportingMessage;

use crate::config::DeimosConsoleConfig;

/// Defensive counter for messages that the drop-oldest eviction-then-retry could not enqueue.
///
/// The current single-writer design makes the retry infallible (after evicting one message
/// or finding the channel empty, the channel has at least one free slot). This counter
/// therefore stays at zero in normal operation. It is retained as the right place to
/// attribute lost frames if a future change introduces a second writer to the channel.
/// Exposed as a `static` so the UI thread can read it directly for the per-tick telemetry
/// line.
pub static DROPPED_FRAMES: AtomicU64 = AtomicU64::new(0);

/// Count of messages that displaced an older message from the front of the bounded channel.
///
/// Incremented every time the receiver thread evicts the oldest queued message to make room
/// for an incoming one. This is the operator-visible signal that the UI is falling behind:
/// total throughput is preserved, but the queued data has shifted toward the freshest rows.
pub static OVERWRITTEN_FRAMES: AtomicU64 = AtomicU64::new(0);

/// Return the current value of the receiver-thread drop counter.
///
/// See [`DROPPED_FRAMES`] for the attribution rules. Stays at zero in normal operation;
/// any non-zero value indicates a structural change to the writer count.
pub fn receiver_dropped_frames() -> u64 {
    DROPPED_FRAMES.load(Ordering::Relaxed)
}

/// Return the current value of the receiver-thread overwrite counter.
///
/// "Overwritten" means "evicted oldest to make room for a newer message". This grows whenever
/// the UI cannot keep up with the inbound row rate; the queued data shifts toward the freshest
/// rows, preserving the freshness invariant under backpressure.
pub fn receiver_overwritten_frames() -> u64 {
    OVERWRITTEN_FRAMES.load(Ordering::Relaxed)
}

/// Capacity of the bounded channel from the receiver thread to the UI thread.
///
/// At 100 Hz × 30 s = 3 000 messages. This gives roughly 30 s of ring-buffer headroom before
/// drops begin if the UI stalls, which is consistent with the 30 s scrolling window target.
const CHANNEL_CAPACITY: usize = 3_000;

/// Build the bounded channel and spawn the receiver background thread.
///
/// Returns `(rx, join_handle)`. The caller passes `rx` to `DeimosConsoleApp::new` and
/// holds `join_handle` for clean shutdown (the thread exits when it encounters a non-transient
/// socket error or when the process terminates).
///
/// # Errors
///
/// Returns `Err` only if the underlying socket cannot be created, configured, or bound — i.e.
/// before the thread starts. Runtime errors inside the thread are logged to stderr and cause
/// the thread to exit quietly.
pub fn spawn(
    config: &DeimosConsoleConfig,
) -> io::Result<(Receiver<ReportingMessage>, thread::JoinHandle<()>)> {
    let socket = build_socket(config)?;

    let (tx, rx) = crossbeam_channel::bounded(CHANNEL_CAPACITY);

    // Hand the recv thread a *clone* of the receiver so it can pop the oldest queued message
    // when the channel is full (drop-oldest policy). The UI also drains `rx`; at capacity the
    // two consumers race, which is intentional — see `recv_loop` for the rationale.
    let rx_for_drainer = rx.clone();

    let handle = thread::Builder::new()
        .name("deimos-receiver".to_string())
        .spawn(move || recv_loop(socket, tx, rx_for_drainer))?;

    Ok((rx, handle))
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Create, configure, and return the receiving UDP socket.
fn build_socket(config: &DeimosConsoleConfig) -> io::Result<UdpSocket> {
    let raw = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;

    raw.set_reuse_address(true)?;

    // SO_REUSEPORT allows multiple processes on the same machine to receive the same
    // multicast stream (e.g. two console windows during development). It is a no-op on
    // platforms that don't support it; ignore the error on those.
    #[cfg(not(windows))]
    let _ = raw.set_reuse_port(true);

    // Bind to INADDR_ANY so the socket receives multicast traffic on all interfaces.
    let bind_addr = SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, config.port);
    raw.bind(&bind_addr.into())?;

    // Join the multicast group on the configured interface (or the default interface).
    let interface = config.interface.unwrap_or(Ipv4Addr::UNSPECIFIED);
    raw.join_multicast_v4(&config.multicast_group, &interface)?;

    // Convert to std::net::UdpSocket before setting the read timeout (socket2's timeout
    // API is equivalent, but we want a std socket for the recv_from call in the loop).
    let std_sock: UdpSocket = raw.into();

    // 100 ms timeout — short enough to check for shutdown without burning CPU, long enough
    // not to add noticeable latency.
    std_sock.set_read_timeout(Some(Duration::from_millis(100)))?;

    Ok(std_sock)
}

/// Main receive loop running on the background thread.
///
/// Exits when a non-transient socket error occurs (anything other than `WouldBlock` or
/// `TimedOut`). The caller's `JoinHandle` can detect the exit.
///
/// # Drop-oldest on a full channel
///
/// When `tx.try_send` returns `Full`, the loop calls `try_recv` on its cloned `Receiver` to
/// evict the front of the queue, increments [`OVERWRITTEN_FRAMES`], and retries the send
/// once. The UI thread also drains the channel; if the UI happens to pop the slot first
/// between our `try_recv` and our retry, the retry can still see `Full` and we count one
/// "could not enqueue" event in [`DROPPED_FRAMES`]. Both outcomes are acceptable: at
/// capacity the UI is already behind, so losing a single in-flight message in the race is no
/// worse than the eviction we were already going to perform.
fn recv_loop(
    socket: UdpSocket,
    tx: Sender<ReportingMessage>,
    rx_for_drainer: Receiver<ReportingMessage>,
) {
    let mut buf = [0u8; 65535];

    loop {
        match socket.recv_from(&mut buf) {
            Ok((n, _addr)) => match ReportingMessage::decode(&buf[..n]) {
                Ok(msg) => enqueue_with_drop_oldest(&tx, &rx_for_drainer, msg),
                Err(e) => {
                    // Malformed packet — log and continue.
                    eprintln!("deimos-receiver: postcard decode error (skipping): {e}");
                }
            },

            Err(e)
                if e.kind() == io::ErrorKind::WouldBlock
                    || e.kind() == io::ErrorKind::TimedOut
                    || e.kind() == io::ErrorKind::Interrupted =>
            {
                // Normal: the 100 ms timeout fired with no data, or a signal interrupted
                // the recv (EINTR). Loop so the thread can check for shutdown.
                continue;
            }

            Err(e) => {
                // Non-transient socket error — log and exit the thread.
                eprintln!("deimos-receiver: socket error, receiver thread exiting: {e}");
                break;
            }
        }
    }
}

/// Enqueue `msg` on `tx`, evicting the oldest queued message if the channel is full.
///
/// On `TrySendError::Full`, attempts to pop one message from `rx_for_drainer` to make room
/// and retries the send. [`OVERWRITTEN_FRAMES`] is incremented only when the pop actually
/// displaced a queued message; if the UI raced ahead and emptied the channel between the
/// `Full` and the pop, the retry still succeeds but no displacement happened, so the
/// counter is left alone. On `TrySendError::Disconnected` the function returns silently —
/// the caller's next `recv_from` will surface the disconnect via its own loop exit.
fn enqueue_with_drop_oldest(
    tx: &Sender<ReportingMessage>,
    rx_for_drainer: &Receiver<ReportingMessage>,
    msg: ReportingMessage,
) {
    let returned = match tx.try_send(msg) {
        Ok(()) => return,
        Err(TrySendError::Full(returned)) => returned,
        Err(TrySendError::Disconnected(_)) => return,
    };

    // Evict the front (if anything is there) and retry. The single-writer invariant
    // guarantees the retry cannot fail: either we just freed a slot, or the UI drained
    // the whole channel between the `Full` and our `try_recv` and the channel is now
    // empty. Either way `try_send` succeeds. Attribute the count based on whether the
    // pop actually displaced a queued message:
    //
    // - `Ok(_)` — we evicted a real message, so `OVERWRITTEN_FRAMES` reflects a true
    //   drop-oldest event.
    // - `Err(Empty)` — the UI emptied the channel for us; nothing was displaced, so the
    //   retry is just a normal enqueue and no counter moves.
    let displaced = rx_for_drainer.try_recv().is_ok();

    match tx.try_send(returned) {
        Ok(()) => {
            if displaced {
                OVERWRITTEN_FRAMES.fetch_add(1, Ordering::Relaxed);
            }
        }
        Err(_) => {
            // Unreachable under the single-writer invariant. Kept as a defensive
            // attribution path in case a future change introduces a second writer.
            DROPPED_FRAMES.fetch_add(1, Ordering::Relaxed);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::SocketAddr;

    /// Build a small ReportingMessage::Row carrying a single sequence number.
    ///
    /// The test only cares about the `seq` field for ordering checks; the other fields are
    /// filled with cheap placeholder values.
    fn row(seq: u64) -> ReportingMessage {
        ReportingMessage::Row {
            seq,
            timestamp: seq as f64,
            system_time: String::new(),
            values: vec![],
        }
    }

    /// Bind a UDP socket on loopback with an ephemeral port; return `(socket, port)`.
    ///
    /// The socket has a short read timeout so the recv loop can observe channel
    /// disconnect / thread join in bounded time during teardown.
    fn bind_loopback_recv() -> (UdpSocket, u16) {
        let sock = UdpSocket::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
            .expect("bind loopback recv socket");
        sock.set_read_timeout(Some(Duration::from_millis(50)))
            .expect("set recv timeout");
        let port = sock.local_addr().expect("local_addr").port();
        (sock, port)
    }

    /// Drop-oldest semantics under sustained backpressure: when the bounded channel is full
    /// and N additional rows arrive, the channel must end up holding the N *most recent*
    /// rows and `OVERWRITTEN_FRAMES` must have grown by exactly N.
    ///
    /// The test exercises the real `recv_loop` against a loopback UDP socket: a sender on a
    /// second loopback socket transmits encoded `ReportingMessage::Row` packets to the recv
    /// thread, and a UI-side receiver clone is left untouched so the channel saturates.
    #[test]
    fn drop_oldest_keeps_most_recent_rows_and_counts_overwrites() {
        const CAP: usize = 4;
        const EXTRA: u64 = 6;

        let (recv_sock, recv_port) = bind_loopback_recv();

        let (tx, rx) = crossbeam_channel::bounded::<ReportingMessage>(CAP);
        let rx_for_drainer = rx.clone();

        // Snapshot the global counter so the assertion is robust to other tests that may
        // have run in this process and incremented it.
        let overwritten_before = OVERWRITTEN_FRAMES.load(Ordering::SeqCst);

        let recv_handle = thread::Builder::new()
            .name("test-recv".to_string())
            .spawn(move || recv_loop(recv_sock, tx, rx_for_drainer))
            .expect("spawn recv thread");

        // Sender: bind an ephemeral port on loopback and send enough packets to overflow the
        // channel by EXTRA. Because the UI-side `rx` is held by this test thread but never
        // drained, every packet past the first CAP forces a drop-oldest eviction.
        let sender = UdpSocket::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
            .expect("bind sender socket");
        let dest = SocketAddr::from((Ipv4Addr::LOCALHOST, recv_port));

        let total = (CAP as u64) + EXTRA;
        let mut buf = Vec::with_capacity(64);
        for seq in 0..total {
            buf.clear();
            row(seq).encode_into(&mut buf).expect("encode row");
            sender.send_to(&buf, dest).expect("send to recv");
            // Brief yield to let the recv thread interleave with sends; without this on
            // fast hosts the kernel may coalesce all packets and the recv thread's first
            // wake-up reads the whole batch in a single tight burst — which still produces
            // the correct end state but is harder to read in test traces.
            thread::sleep(Duration::from_millis(2));
        }

        // Wait until the channel is at full capacity and the overwrite counter has caught up.
        // Poll briefly rather than guessing a fixed sleep.
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        loop {
            let overwrites_now = OVERWRITTEN_FRAMES.load(Ordering::SeqCst) - overwritten_before;
            if rx.len() == CAP && overwrites_now >= EXTRA {
                break;
            }
            if std::time::Instant::now() > deadline {
                panic!(
                    "timed out waiting for recv thread: rx.len()={}, overwrites={}",
                    rx.len(),
                    overwrites_now
                );
            }
            thread::sleep(Duration::from_millis(10));
        }

        // Drain the channel and assert it holds the N most recent rows.
        let drained: Vec<u64> = rx
            .try_iter()
            .map(|m| match m {
                ReportingMessage::Row { seq, .. } => seq,
                _ => panic!("unexpected non-Row message in channel"),
            })
            .collect();

        let expected: Vec<u64> = ((total - CAP as u64)..total).collect();
        assert_eq!(
            drained, expected,
            "channel must hold the {CAP} most recent rows in FIFO order"
        );

        let overwrites_total = OVERWRITTEN_FRAMES.load(Ordering::SeqCst) - overwritten_before;
        assert_eq!(
            overwrites_total, EXTRA,
            "OVERWRITTEN_FRAMES must have grown by exactly the number of rows past capacity"
        );

        // Tear down: the recv thread owns `tx` and `rx_for_drainer`, so the channel never
        // disconnects from the test's perspective and we can't ask the thread to exit
        // cleanly. Drop the test's `rx` (no functional effect — just releases our clone)
        // and drop the JoinHandle to detach; the recv thread keeps blocking on its 50 ms
        // socket timeout until the test process exits and reaps it.
        drop(rx);
        drop(recv_handle);
    }

    /// `enqueue_with_drop_oldest` must enqueue cleanly when the channel has free capacity,
    /// without touching either counter.
    #[test]
    fn enqueue_with_drop_oldest_no_eviction_on_free_channel() {
        let (tx, rx) = crossbeam_channel::bounded::<ReportingMessage>(2);
        let rx_drain = rx.clone();

        let dropped_before = DROPPED_FRAMES.load(Ordering::SeqCst);
        let overwritten_before = OVERWRITTEN_FRAMES.load(Ordering::SeqCst);

        enqueue_with_drop_oldest(&tx, &rx_drain, row(0));

        assert_eq!(rx.len(), 1, "row must be enqueued");
        assert_eq!(
            DROPPED_FRAMES.load(Ordering::SeqCst),
            dropped_before,
            "no drop on a non-full channel"
        );
        assert_eq!(
            OVERWRITTEN_FRAMES.load(Ordering::SeqCst),
            overwritten_before,
            "no overwrite on a non-full channel"
        );
    }
}
