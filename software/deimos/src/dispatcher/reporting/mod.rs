//! Push-based reporting dispatcher that multicasts dispatched rows to operator consoles.
//!
//! The wire format is defined in the [`wire`] submodule.
//!
//! `ReportingDispatcher` serializes each [`Row`] onto a one-way UDP multicast transport
//! suitable for multi-seat control rooms. Sends in the control loop are non-blocking and
//! infallible: if the socket buffer is full, the frame is dropped and `dropped_frames` is
//! incremented. The dispatcher never returns `Err` from `consume` after a successful `init`.
//!
//! Two message types are sent on the wire:
//! - [`ReportingMessage::Schema`] — emitted at the start of Operating and periodically
//!   thereafter; contains channel names, units, and a wall-clock anchor.
//! - [`ReportingMessage::Row`] — one per `consume` call; carries the sequence number,
//!   timestamps, and channel values.

pub mod wire;
pub use wire::ReportingMessage;

use std::io;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, UdpSocket};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use socket2::{Domain, Protocol, Socket, Type};
use tracing::warn;

use crate::controller::context::ControllerCtx;

use super::Dispatcher;

/// Standard Ethernet MTU in bytes.
const ETHERNET_MTU: usize = 1500;

/// Ethernet (14 B) + IP (20 B) + UDP (8 B) header overhead in bytes.
const IP_UDP_OVERHEAD: usize = 42;

/// Maximum UDP payload budget before IP fragmentation occurs.
const MTU_PAYLOAD_BUDGET: usize = ETHERNET_MTU - IP_UDP_OVERHEAD;

/// Per-channel payload size estimate (8 bytes per f64 value).
const BYTES_PER_CHANNEL: usize = 8;

/// Estimated fixed overhead per Row message (seq u64, timestamp f64, system_time ~30 bytes,
/// postcard framing ~10 bytes).
const ROW_OVERHEAD_BYTES: usize = 64;

/// Push-based reporting dispatcher.
///
/// Serializes each dispatched [`crate::dispatcher::Row`] into a compact binary wire format
/// and sends it to a UDP multicast group for consumption by one or more operator consoles.
/// Frames that cannot be sent without blocking are silently dropped; `dropped_frames` tracks
/// the count so operators can see data quality in the viewer's status bar.
///
/// The socket is bound at `init` time and closed at `terminate` time. Between runs the struct
/// holds only the configuration fields; runtime-only state is `#[serde(skip)]`.
#[derive(Serialize, Deserialize)]
pub struct ReportingDispatcher {
    /// UDP multicast group address.
    pub multicast_group: Ipv4Addr,

    /// UDP port.
    pub port: u16,

    /// Local interface to bind for outbound multicast traffic. `None` lets the OS choose.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outbound_interface: Option<Ipv4Addr>,

    /// How often to re-emit the Schema packet while Operating, so late-joining consoles
    /// can discover channel metadata without waiting for the next run.
    pub schema_period: Duration,

    /// Count of frames dropped because the non-blocking send would have blocked.
    ///
    /// Stored behind `Arc` so callers can clone a handle via [`dropped_frames_handle`] before
    /// registering the dispatcher with the controller, and read the counter after the run ends
    /// (including after `terminate` resets runtime state).
    ///
    /// `#[serde(skip)]` because this is runtime-only state, not configuration.
    #[serde(skip)]
    dropped_frames: Arc<AtomicU64>,

    /// Pre-allocated serialization scratch buffer. Re-used each `consume` to avoid
    /// per-frame heap allocation in the control loop.
    #[serde(skip)]
    scratch_buf: Vec<u8>,

    // --- Runtime-only fields (cleared/reset by init) ---
    /// Bound non-blocking UDP socket, present only while Operating.
    #[serde(skip)]
    socket: Option<UdpSocket>,

    /// Resolved multicast destination address (multicast_group + port).
    #[serde(skip)]
    multicast_addr: Option<SocketAddrV4>,

    /// Stored Schema message built at init time; re-emitted periodically and at terminate.
    #[serde(skip)]
    stored_schema: Option<ReportingMessage>,

    /// Monotonically increasing sequence counter; reset to 0 at each init.
    #[serde(skip)]
    seq: u64,

    /// Instant at which the last Schema packet was sent; used to throttle re-emission.
    #[serde(skip)]
    last_schema_sent: Option<Instant>,

    /// Instant at which the last non-WouldBlock send error was logged; used to rate-limit
    /// repeated error messages in the consume hot path.
    #[serde(skip)]
    last_error_log_at: Option<Instant>,
}

/// Rate limit for non-WouldBlock send error log messages.
const ERROR_LOG_RATE_LIMIT: Duration = Duration::from_secs(1);

impl ReportingDispatcher {
    /// Create a new `ReportingDispatcher` with the given configuration.
    ///
    /// # Arguments
    ///
    /// * `multicast_group` — UDP multicast destination address.
    /// * `port` — UDP destination port.
    /// * `outbound_interface` — Local interface for multicast traffic; `None` for OS default.
    /// * `schema_period` — Interval between periodic Schema re-emissions while Operating.
    pub fn new(
        multicast_group: Ipv4Addr,
        port: u16,
        outbound_interface: Option<Ipv4Addr>,
        schema_period: Duration,
    ) -> Box<Self> {
        Box::new(Self {
            multicast_group,
            port,
            outbound_interface,
            schema_period,
            dropped_frames: Arc::new(AtomicU64::new(0)),
            scratch_buf: Vec::new(),
            socket: None,
            multicast_addr: None,
            stored_schema: None,
            seq: 0,
            last_schema_sent: None,
            last_error_log_at: None,
        })
    }

    /// Return the current dropped-frames count.
    pub fn dropped_frames(&self) -> u64 {
        self.dropped_frames.load(Ordering::Relaxed)
    }

    /// Return a cloned `Arc` to the dropped-frames counter.
    ///
    /// Clone this handle **before** registering the dispatcher with the controller.
    /// The controller consumes the dispatcher, but the `Arc` keeps the counter alive so
    /// the caller can read it after `Controller::run` returns — even after `terminate`
    /// has reset other runtime state.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let reporting = ReportingDispatcher::new(...);
    /// let dropped = reporting.dropped_frames_handle();
    /// controller.add_dispatcher("reporting", reporting);
    /// controller.run(...);
    /// assert_eq!(dropped.load(Ordering::Relaxed), 0);
    /// ```
    pub fn dropped_frames_handle(&self) -> Arc<AtomicU64> {
        Arc::clone(&self.dropped_frames)
    }
}

#[typetag::serde]
impl Dispatcher for ReportingDispatcher {
    fn init(
        &mut self,
        ctx: &ControllerCtx,
        channel_names: &[String],
        _core_assignment: usize,
    ) -> Result<(), String> {
        // Reset all runtime state so re-init is clean.
        self.socket = None;
        self.stored_schema = None;
        self.seq = 0;
        self.last_schema_sent = None;
        self.last_error_log_at = None;
        self.dropped_frames.store(0, Ordering::Relaxed);
        self.scratch_buf.clear();

        // --- Bind socket with SO_REUSEADDR ---
        //
        // We use socket2 to set SO_REUSEADDR before bind, which is not possible with
        // std::net::UdpSocket directly (it binds immediately in `bind()`).
        let raw = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))
            .map_err(|e| format!("ReportingDispatcher: failed to create UDP socket: {e}"))?;

        raw.set_reuse_address(true)
            .map_err(|e| format!("ReportingDispatcher: SO_REUSEADDR failed: {e}"))?;

        // Bind to INADDR_ANY on the chosen port so the OS assigns a source port.
        let bind_addr: SocketAddr = SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 0).into();
        raw.bind(&bind_addr.into())
            .map_err(|e| format!("ReportingDispatcher: bind failed: {e}"))?;

        // --- Multicast socket options (set via socket2 before converting) ---
        raw.set_multicast_ttl_v4(1)
            .map_err(|e| format!("ReportingDispatcher: set_multicast_ttl_v4 failed: {e}"))?;

        if let Some(iface) = self.outbound_interface {
            raw.set_multicast_if_v4(&iface)
                .map_err(|e| format!("ReportingDispatcher: set_multicast_if_v4 failed: {e}"))?;
        }

        raw.set_nonblocking(true)
            .map_err(|e| format!("ReportingDispatcher: set_nonblocking failed: {e}"))?;

        // Convert to std UdpSocket.
        let std_sock: UdpSocket = raw.into();

        // --- MTU check ---
        let channel_count = channel_names.len();
        let projected_row_bytes = BYTES_PER_CHANNEL * channel_count + ROW_OVERHEAD_BYTES;
        if projected_row_bytes > MTU_PAYLOAD_BUDGET {
            warn!(
                channel_count,
                projected_row_bytes,
                mtu_payload_budget = MTU_PAYLOAD_BUDGET,
                "ReportingDispatcher: projected Row payload ({projected_row_bytes} B) exceeds \
                 MTU budget ({MTU_PAYLOAD_BUDGET} B); IP fragmentation will occur on some networks"
            );
        }

        // --- Compute monotonic epoch anchor ---
        //
        // We want the nanoseconds-since-Unix-epoch corresponding to controller timestamp 0.
        // The controller's timestamps are session-relative (starting from dt_ns). Since we are
        // at init time and the first cycle hasn't started, we use the current system time as a
        // reasonable anchor. The viewer can compute approximate wall time from this anchor plus
        // each Row's session-relative timestamp.
        let monotonic_epoch_ns = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);

        // --- Build and store Schema message ---
        let schema = ReportingMessage::Schema {
            channel_names: channel_names.to_vec(),
            channel_units: ctx.channel_units.clone(),
            monotonic_epoch_ns,
            is_session_end: false,
        };
        self.stored_schema = Some(schema);
        self.multicast_addr = Some(SocketAddrV4::new(self.multicast_group, self.port));
        self.socket = Some(std_sock);

        Ok(())
    }

    fn consume(
        &mut self,
        time: SystemTime,
        timestamp: i64,
        channel_values: Vec<f64>,
    ) -> Result<(), String> {
        let (Some(socket), Some(multicast_addr)) =
            (self.socket.as_ref(), self.multicast_addr.as_ref())
        else {
            // Not initialized; silently skip (should not happen after a successful init).
            return Ok(());
        };

        // --- Periodic Schema re-emission ---
        //
        // Emit a Schema packet on the first consume call (last_schema_sent is None). The first
        // consume corresponds to Operating-state entry — init runs during Configuring, so the
        // first data row arrives at the start of Operating. After that, re-emit every
        // `schema_period` so late-joining viewers can discover channel metadata mid-run.
        let now = Instant::now();
        let should_emit_schema = match self.last_schema_sent {
            None => true,
            Some(last) => now.duration_since(last) >= self.schema_period,
        };

        if should_emit_schema && let Some(schema) = &self.stored_schema {
            self.scratch_buf.clear();
            match schema.encode_into(&mut self.scratch_buf) {
                Err(e) => {
                    warn!("ReportingDispatcher: Schema encode error (skipping): {e}");
                }
                Ok(_) => {
                    // Advance last_schema_sent before the send attempt so that any
                    // outcome — success, WouldBlock, or persistent error — causes the
                    // re-emit to wait a full schema_period before the next attempt.
                    // Without this, a persistent non-WouldBlock error (e.g. interface
                    // down) would re-encode and re-send the Schema every cycle instead
                    // of once per schema_period, wasting CPU. WouldBlock still silently
                    // retries next period rather than next cycle, which is fine because
                    // the Schema will come around again quickly anyway.
                    // Schema drops are not counted into dropped_frames (that counter
                    // tracks data rows only). Non-WouldBlock errors are rate-limit-logged
                    // per the realtime-reporting spec (R8).
                    self.last_schema_sent = Some(now);
                    match socket.send_to(&self.scratch_buf, *multicast_addr) {
                        Ok(_) => {}
                        Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                            // Silent drop; retry next schema_period.
                        }
                        Err(e) => {
                            let should_log = self.last_error_log_at.is_none_or(|last| {
                                now.duration_since(last) >= ERROR_LOG_RATE_LIMIT
                            });
                            if should_log {
                                warn!("ReportingDispatcher: Schema send_to error (skipping): {e}");
                                self.last_error_log_at = Some(now);
                            }
                        }
                    }
                }
            }
        }

        // Build wire Row.
        let msg = ReportingMessage::Row {
            seq: self.seq,
            // Convert nanoseconds (i64) to seconds (f64) for the wire format.
            timestamp: timestamp as f64 * 1e-9,
            system_time: super::fmt_time(time),
            values: channel_values,
        };

        // Serialize into pre-allocated scratch buffer (clear first to avoid stale bytes).
        self.scratch_buf.clear();
        if let Err(e) = msg.encode_into(&mut self.scratch_buf) {
            warn!("ReportingDispatcher: postcard encode error (skipping frame): {e}");
            return Ok(());
        }

        // Advance seq before the send attempt so the wire sequence number is always
        // emitted (or accounted for) regardless of send outcome. This ensures the viewer's
        // NaN-gap detector sees a gap even when the frame is dropped due to a non-WouldBlock
        // error (e.g., ENETUNREACH).
        self.seq += 1;

        // Non-blocking send.
        match socket.send_to(&self.scratch_buf, *multicast_addr) {
            Ok(_) => {}
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                self.dropped_frames.fetch_add(1, Ordering::Relaxed);
            }
            Err(e) => {
                // Non-WouldBlock error: count the drop and log at most once per rate-limit
                // window. consume must never return Err so this dispatcher cannot abort the
                // control loop.
                self.dropped_frames.fetch_add(1, Ordering::Relaxed);
                let should_log = self
                    .last_error_log_at
                    .is_none_or(|last| now.duration_since(last) >= ERROR_LOG_RATE_LIMIT);
                if should_log {
                    warn!("ReportingDispatcher: send_to error (frame dropped): {e}");
                    self.last_error_log_at = Some(now);
                }
            }
        }

        Ok(())
    }

    fn terminate(&mut self) -> Result<(), String> {
        // Emit one final Schema with is_session_end=true so viewers can detect a clean
        // session end rather than inferring it from a stale connection. Errors are swallowed
        // — terminate must never return Err.
        if let (Some(socket), Some(multicast_addr), Some(stored_schema)) = (
            self.socket.as_ref(),
            self.multicast_addr.as_ref(),
            self.stored_schema.take(),
        ) {
            // Build the session-end Schema from the stored one, flipping the flag.
            let session_end_schema = match stored_schema {
                ReportingMessage::Schema {
                    channel_names,
                    channel_units,
                    monotonic_epoch_ns,
                    ..
                } => ReportingMessage::Schema {
                    channel_names,
                    channel_units,
                    monotonic_epoch_ns,
                    is_session_end: true,
                },
                other => other,
            };

            self.scratch_buf.clear();
            match session_end_schema.encode_into(&mut self.scratch_buf) {
                Err(e) => {
                    warn!("ReportingDispatcher: session-end Schema encode error (ignored): {e}");
                }
                Ok(_) => {
                    // Switch to blocking mode for the final send so a momentarily-full
                    // kernel buffer does not silently swallow the session-end marker.
                    // Errors are rate-limit-warned and ignored — terminate must not return Err.
                    if let Err(e) = socket.set_nonblocking(false) {
                        warn!(
                            "ReportingDispatcher: could not set blocking mode for session-end \
                             Schema send (ignored): {e}"
                        );
                    }
                    if let Err(e) = socket.send_to(&self.scratch_buf, *multicast_addr) {
                        warn!(
                            "ReportingDispatcher: session-end Schema send_to error (ignored): {e}"
                        );
                    }
                }
            }
        }

        // Drop the socket and reset all runtime state.
        // Note: dropped_frames is NOT reset here so that callers holding a handle from
        // dropped_frames_handle() can read the final count after the run ends.
        // It will be reset to 0 at the next init() call.
        self.socket = None;
        self.multicast_addr = None;
        self.stored_schema = None;
        self.seq = 0;
        self.scratch_buf.clear();
        self.last_schema_sent = None;
        self.last_error_log_at = None;

        Ok(())
    }
}
