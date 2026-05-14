//! Background UDP multicast receiver thread.
//!
//! Joins the configured multicast group, reads incoming datagrams in a blocking `recv_from` loop
//! (with a 100 ms timeout for shutdown checks), decodes each as a [`ReportingMessage`], and
//! forwards it to the UI thread via a bounded [`crossbeam_channel`]. Drop-oldest eviction
//! preserves freshness when the UI lags; the counter exposed via [`ReceiverCounters`] tracks
//! how often it fires.

use std::io;
use std::net::{Ipv4Addr, SocketAddrV4, UdpSocket};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender, TrySendError};
use socket2::{Domain, Protocol, Socket, Type};

use deimos::dispatcher::ReportingMessage;

use crate::config::DeimosConsoleConfig;

/// Atomic counters exposed by [`spawn`]. Clone freely — backed by `Arc`s shared with the
/// receiver thread.
///
/// - `overwritten` increments on every drop-oldest eviction (steady-state signal that the UI
///   is falling behind).
/// - `dropped` increments only if a retry after eviction also fails. Under the current single-
///   writer design this stays at zero; it is the right place to attribute losses if a future
///   change introduces a second writer to the channel.
#[derive(Clone, Default)]
pub struct ReceiverCounters {
    pub dropped: Arc<AtomicU64>,
    pub overwritten: Arc<AtomicU64>,
}

/// 100 Hz × 30 s ≈ 30 s of ring-buffer headroom before drops begin if the UI stalls, matching
/// the 30 s scrolling window default.
const CHANNEL_CAPACITY: usize = 3_000;

/// One log line per second under sustained decode errors. Matches the `ReportingDispatcher`
/// send-error log cadence so both sides of the wire are equally noisy under corruption.
const ERROR_LOG_RATE_LIMIT: Duration = Duration::from_secs(1);

/// Throttle for repetitive error logs: at most one line per `window`, summing suppressed events
/// into the next emitted line so the operator sees the true rate.
struct LogRateLimiter {
    window: Duration,
    last_logged: Option<Instant>,
    suppressed: u64,
}

impl LogRateLimiter {
    fn new(window: Duration) -> Self {
        Self {
            window,
            last_logged: None,
            suppressed: 0,
        }
    }

    /// Returns `Some(suppressed_since_last_log)` when the caller should log, `None` otherwise.
    fn check(&mut self, now: Instant) -> Option<u64> {
        let should_log = self
            .last_logged
            .is_none_or(|last| now.duration_since(last) >= self.window);
        if should_log {
            let drained = self.suppressed;
            self.suppressed = 0;
            self.last_logged = Some(now);
            Some(drained)
        } else {
            self.suppressed += 1;
            None
        }
    }
}

/// Build the bounded channel and spawn the receiver background thread.
///
/// Returns `(rx, join_handle, counters)`. The caller passes `rx` to `DeimosConsoleApp::new`,
/// holds `join_handle` for shutdown, and reads `counters` for the per-tick telemetry line.
///
/// Returns `Err` only if the socket cannot be created, configured, or bound. Runtime errors
/// inside the thread are logged to stderr and cause the thread to exit quietly.
pub fn spawn(
    config: &DeimosConsoleConfig,
) -> io::Result<(
    Receiver<ReportingMessage>,
    thread::JoinHandle<()>,
    ReceiverCounters,
)> {
    let socket = build_socket(config)?;
    let (tx, rx) = crossbeam_channel::bounded(CHANNEL_CAPACITY);

    // The recv thread holds a clone of the receiver so it can pop the oldest queued message
    // when the channel is full. The UI also drains `rx`; at capacity the two consumers race —
    // see `enqueue_with_drop_oldest` for the rationale.
    let rx_for_drainer = rx.clone();

    let counters = ReceiverCounters::default();
    let counters_for_thread = counters.clone();

    let handle = thread::Builder::new()
        .name("deimos-receiver".to_string())
        .spawn(move || recv_loop(socket, tx, rx_for_drainer, counters_for_thread))?;

    Ok((rx, handle, counters))
}

fn build_socket(config: &DeimosConsoleConfig) -> io::Result<UdpSocket> {
    let raw = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    raw.set_reuse_address(true)?;

    // SO_REUSEPORT lets two console windows receive the same multicast stream during
    // development. No-op on platforms that don't support it.
    #[cfg(not(windows))]
    let _ = raw.set_reuse_port(true);

    raw.bind(&SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, config.port).into())?;

    let interface = config.interface.unwrap_or(Ipv4Addr::UNSPECIFIED);
    raw.join_multicast_v4(&config.multicast_group, &interface)?;

    let std_sock: UdpSocket = raw.into();
    // 100 ms read timeout — short enough to observe shutdown, long enough to not burn CPU.
    std_sock.set_read_timeout(Some(Duration::from_millis(100)))?;
    Ok(std_sock)
}

/// Main receive loop. Exits on any non-transient socket error.
///
/// On a full channel `enqueue_with_drop_oldest` evicts the front and retries. If the UI raced
/// the eviction and the retry still sees `Full`, that one message is counted in
/// `counters.dropped`. Both outcomes are acceptable: at capacity the UI is already behind.
fn recv_loop(
    socket: UdpSocket,
    tx: Sender<ReportingMessage>,
    rx_for_drainer: Receiver<ReportingMessage>,
    counters: ReceiverCounters,
) {
    let mut buf = [0u8; 65535];
    let mut decode_error_limiter = LogRateLimiter::new(ERROR_LOG_RATE_LIMIT);

    loop {
        match socket.recv_from(&mut buf) {
            Ok((n, _addr)) => match ReportingMessage::decode(&buf[..n]) {
                Ok(msg) => enqueue_with_drop_oldest(&tx, &rx_for_drainer, msg, &counters),
                Err(e) => {
                    if let Some(suppressed) = decode_error_limiter.check(Instant::now()) {
                        eprintln!(
                            "deimos-receiver: postcard decode error (skipping): {e} \
                             ({suppressed} similar suppressed since last log)"
                        );
                    }
                }
            },

            Err(e)
                if e.kind() == io::ErrorKind::WouldBlock
                    || e.kind() == io::ErrorKind::TimedOut
                    || e.kind() == io::ErrorKind::Interrupted =>
            {
                continue;
            }

            Err(e) => {
                eprintln!("deimos-receiver: socket error, receiver thread exiting: {e}");
                break;
            }
        }
    }
}

/// Enqueue `msg`, evicting the oldest queued message if the channel is full. Counters move
/// only when the pop actually displaced a queued message; an empty channel after the `Full`
/// (UI drained between our checks) is a normal enqueue.
fn enqueue_with_drop_oldest(
    tx: &Sender<ReportingMessage>,
    rx_for_drainer: &Receiver<ReportingMessage>,
    msg: ReportingMessage,
    counters: &ReceiverCounters,
) {
    let returned = match tx.try_send(msg) {
        Ok(()) => return,
        Err(TrySendError::Full(returned)) => returned,
        Err(TrySendError::Disconnected(_)) => return,
    };

    let displaced = rx_for_drainer.try_recv().is_ok();

    match tx.try_send(returned) {
        Ok(()) => {
            if displaced {
                counters.overwritten.fetch_add(1, Ordering::Relaxed);
            }
        }
        Err(_) => {
            // Unreachable under the single-writer invariant; kept as a defensive attribution
            // path in case a future change introduces a second writer.
            counters.dropped.fetch_add(1, Ordering::Relaxed);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::SocketAddr;

    #[test]
    fn log_rate_limiter_logs_first_then_suppresses_then_drains() {
        let window = Duration::from_secs(1);
        let mut limiter = LogRateLimiter::new(window);
        let t0 = Instant::now();

        assert_eq!(limiter.check(t0), Some(0), "first event must log");

        for i in 1..=5 {
            assert_eq!(
                limiter.check(t0 + Duration::from_millis(100 * i)),
                None,
                "events inside the window must suppress",
            );
        }

        assert_eq!(
            limiter.check(t0 + window + Duration::from_millis(1)),
            Some(5),
            "the next post-window event must log and report the 5 suppressed",
        );

        assert_eq!(
            limiter.check(t0 + window + Duration::from_millis(2)),
            None,
            "suppressed count resets after a successful log",
        );
    }

    fn row(seq: u64) -> ReportingMessage {
        ReportingMessage::Row {
            seq,
            timestamp: seq as f64,
            system_time: String::new(),
            values: vec![],
        }
    }

    fn bind_loopback_recv() -> (UdpSocket, u16) {
        let sock = UdpSocket::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
            .expect("bind loopback recv socket");
        sock.set_read_timeout(Some(Duration::from_millis(50)))
            .expect("set recv timeout");
        let port = sock.local_addr().expect("local_addr").port();
        (sock, port)
    }

    /// Drop-oldest under sustained backpressure: N excess rows must leave the channel holding
    /// the N most recent rows and `overwritten` increased by exactly N.
    #[test]
    fn drop_oldest_keeps_most_recent_rows_and_counts_overwrites() {
        const CAP: usize = 4;
        const EXTRA: u64 = 6;

        let (recv_sock, recv_port) = bind_loopback_recv();

        let (tx, rx) = crossbeam_channel::bounded::<ReportingMessage>(CAP);
        let rx_for_drainer = rx.clone();
        let counters = ReceiverCounters::default();
        let counters_for_thread = counters.clone();

        let recv_handle = thread::Builder::new()
            .name("test-recv".to_string())
            .spawn(move || recv_loop(recv_sock, tx, rx_for_drainer, counters_for_thread))
            .expect("spawn recv thread");

        let sender = UdpSocket::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
            .expect("bind sender socket");
        let dest = SocketAddr::from((Ipv4Addr::LOCALHOST, recv_port));

        let total = (CAP as u64) + EXTRA;
        let mut buf = Vec::with_capacity(64);
        for seq in 0..total {
            buf.clear();
            row(seq).encode_into(&mut buf).expect("encode row");
            sender.send_to(&buf, dest).expect("send to recv");
            // Brief yield so the recv thread interleaves with sends; without it, fast hosts
            // may have the recv thread read the whole batch in a single wake-up.
            thread::sleep(Duration::from_millis(2));
        }

        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        loop {
            let overwrites_now = counters.overwritten.load(Ordering::SeqCst);
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

        assert_eq!(
            counters.overwritten.load(Ordering::SeqCst),
            EXTRA,
            "overwritten must have grown by exactly the number of rows past capacity"
        );

        // The recv thread owns `tx` and `rx_for_drainer`, so the channel never disconnects
        // from the test's perspective. Drop our `rx` and detach the JoinHandle; the recv
        // thread keeps blocking on its 50 ms socket timeout until the process exits.
        drop(rx);
        drop(recv_handle);
    }

    #[test]
    fn enqueue_with_drop_oldest_no_eviction_on_free_channel() {
        let (tx, rx) = crossbeam_channel::bounded::<ReportingMessage>(2);
        let rx_drain = rx.clone();
        let counters = ReceiverCounters::default();

        enqueue_with_drop_oldest(&tx, &rx_drain, row(0), &counters);

        assert_eq!(rx.len(), 1, "row must be enqueued");
        assert_eq!(
            counters.dropped.load(Ordering::SeqCst),
            0,
            "no drop on a non-full channel"
        );
        assert_eq!(
            counters.overwritten.load(Ordering::SeqCst),
            0,
            "no overwrite on a non-full channel"
        );
    }
}
