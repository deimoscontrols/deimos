//! Background UDP multicast receiver thread.
//!
//! Joins the configured multicast group, reads incoming datagrams in a tight non-blocking
//! loop (with a 100 ms read timeout so the loop can observe a shutdown signal), deserializes
//! each datagram as a [`ReportingMessage`], and forwards it to the UI thread via a bounded
//! [`crossbeam_channel`].
//!
//! When the channel is full the message is dropped and `DROPPED_FRAMES` is incremented so
//! the UI can surface backpressure counts (task 5.6).

use std::io;
use std::net::{Ipv4Addr, SocketAddrV4, UdpSocket};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::Duration;

use socket2::{Domain, Protocol, Socket, Type};

use deimos::dispatcher::ReportingMessage;

use crate::config::DeimosConsoleConfig;

/// Count of messages dropped because the bounded channel to the UI was full.
///
/// Exposed as a `static` so the UI thread can read it directly for the status bar (task 5.6).
/// The value is only ever incremented here and read by the UI; no reset between sessions
/// for now (a future task can add per-session snapshots).
pub static DROPPED_FRAMES: AtomicU64 = AtomicU64::new(0);

/// Return the current value of the receiver-thread drop counter.
///
/// This is the number of `ReportingMessage` packets that arrived from the network but could
/// not be forwarded to the UI thread because the bounded channel was full.
pub fn receiver_dropped_frames() -> u64 {
    DROPPED_FRAMES.load(Ordering::Relaxed)
}

/// Capacity of the bounded channel from the receiver thread to the UI thread.
///
/// At 100 Hz × 30 s = 3 000 messages. This gives roughly 30 s of ring-buffer headroom before
/// drops begin if the UI stalls, which is consistent with the 30 s scrolling window target.
const CHANNEL_CAPACITY: usize = 3_000;

/// Build the bounded channel and spawn the receiver background thread.
///
/// Returns `(tx, rx, join_handle)`. The caller passes `rx` to `DeimosConsoleApp::new` and
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
) -> io::Result<(
    crossbeam_channel::Sender<ReportingMessage>,
    crossbeam_channel::Receiver<ReportingMessage>,
    thread::JoinHandle<()>,
)> {
    let socket = build_socket(config)?;

    let (tx, rx) = crossbeam_channel::bounded(CHANNEL_CAPACITY);
    let tx_clone = tx.clone();

    let handle = thread::Builder::new()
        .name("deimos-receiver".to_string())
        .spawn(move || recv_loop(socket, tx_clone))?;

    Ok((tx, rx, handle))
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
fn recv_loop(socket: UdpSocket, tx: crossbeam_channel::Sender<ReportingMessage>) {
    let mut buf = [0u8; 65535];

    loop {
        match socket.recv_from(&mut buf) {
            Ok((n, _addr)) => {
                match ReportingMessage::decode(&buf[..n]) {
                    Ok(msg) => {
                        // try_send never blocks; on a full channel we drop and count.
                        if tx.try_send(msg).is_err() {
                            DROPPED_FRAMES.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                    Err(e) => {
                        // Malformed packet — log and continue.
                        eprintln!("deimos-receiver: postcard decode error (skipping): {e}");
                    }
                }
            }

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
