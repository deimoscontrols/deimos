//! Push-based reporting dispatcher that multicasts dispatched rows to operator consoles.
//!
//! The wire format is defined in the [`wire`] submodule.
//!
//! `ReportingDispatcher` serializes each [`Row`] onto a one-way UDP multicast transport
//! suitable for multi-seat control rooms. Sends in the control loop are non-blocking and
//! infallible: any `send_to` error — `WouldBlock` from a full socket buffer, ENETUNREACH,
//! or any other transient failure — increments `dropped_frames` and is swallowed so the
//! dispatcher never returns `Err` from `consume` after a successful `init`.
//!
//! Two message types are sent on the wire:
//! - [`ReportingMessage::Schema`] — emitted at the start of Operating, periodically
//!   re-emitted while Operating, and once more from `terminate` with `is_session_end=true`;
//!   contains channel names, units, and a wall-clock anchor.
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

/// UDP payload budget before IP fragmentation: 1500 B MTU minus 14+20+8 B of Ethernet/IP/UDP
/// headers. Used at init to warn if the projected `Row` payload would fragment.
const MTU_PAYLOAD_BUDGET: usize = 1500 - 42;
const BYTES_PER_CHANNEL: usize = 8;
/// seq u64 + timestamp f64 + system_time (~30 B) + postcard framing (~10 B).
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

    /// Frames dropped because a non-blocking send would have blocked. Behind `Arc` so callers
    /// can clone a handle via [`dropped_frames_handle`] and read the count after the run ends.
    #[serde(skip)]
    dropped_frames: Arc<AtomicU64>,

    /// Re-used each `consume` to avoid per-frame heap allocation in the control loop.
    #[serde(skip)]
    scratch_buf: Vec<u8>,

    #[serde(skip)]
    socket: Option<UdpSocket>,

    #[serde(skip)]
    multicast_addr: Option<SocketAddrV4>,

    /// Built at `init`; re-emitted periodically and at `terminate`.
    #[serde(skip)]
    stored_schema: Option<ReportingMessage>,

    /// Sequence counter; reset at each `init`.
    #[serde(skip)]
    seq: u64,

    #[serde(skip)]
    last_schema_sent: Option<Instant>,

    /// Throttle for repeated non-WouldBlock send errors in the consume hot path.
    #[serde(skip)]
    last_error_log_at: Option<Instant>,
}

/// Rate limit for non-WouldBlock send error log messages.
const ERROR_LOG_RATE_LIMIT: Duration = Duration::from_secs(1);

impl ReportingDispatcher {
    /// `outbound_interface = None` lets the OS pick the NIC. `schema_period` controls re-emission
    /// cadence while Operating so late-joining viewers can recover channel metadata.
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

    /// Cloned `Arc` to the dropped-frames counter. Clone before registering the dispatcher
    /// with the controller; the controller consumes the dispatcher, but the `Arc` keeps the
    /// counter readable after `Controller::run` returns and after `terminate` resets other state.
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
        // Reset runtime state so re-init is clean.
        self.socket = None;
        self.stored_schema = None;
        self.seq = 0;
        self.last_schema_sent = None;
        self.last_error_log_at = None;
        self.dropped_frames.store(0, Ordering::Relaxed);
        self.scratch_buf.clear();

        // socket2 lets us set SO_REUSEADDR before bind; std::net::UdpSocket binds eagerly.
        let raw = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))
            .map_err(|e| format!("ReportingDispatcher: failed to create UDP socket: {e}"))?;

        raw.set_reuse_address(true)
            .map_err(|e| format!("ReportingDispatcher: SO_REUSEADDR failed: {e}"))?;

        let bind_addr: SocketAddr = SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 0).into();
        raw.bind(&bind_addr.into())
            .map_err(|e| format!("ReportingDispatcher: bind failed: {e}"))?;

        raw.set_multicast_ttl_v4(1)
            .map_err(|e| format!("ReportingDispatcher: set_multicast_ttl_v4 failed: {e}"))?;

        if let Some(iface) = self.outbound_interface {
            raw.set_multicast_if_v4(&iface)
                .map_err(|e| format!("ReportingDispatcher: set_multicast_if_v4 failed: {e}"))?;
        }

        raw.set_nonblocking(true)
            .map_err(|e| format!("ReportingDispatcher: set_nonblocking failed: {e}"))?;

        let std_sock: UdpSocket = raw.into();

        let projected_row_bytes = BYTES_PER_CHANNEL * channel_names.len() + ROW_OVERHEAD_BYTES;
        if projected_row_bytes > MTU_PAYLOAD_BUDGET {
            warn!(
                "ReportingDispatcher: projected Row payload {projected_row_bytes} B exceeds \
                 MTU budget {MTU_PAYLOAD_BUDGET} B; IP fragmentation will occur on some networks"
            );
        }

        // Wall-clock anchor for controller timestamp 0; viewers add the session-relative
        // Row timestamp to recover approximate wall time.
        let monotonic_epoch_ns = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);

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

        // Emit Schema on first consume (Operating-state entry) and every `schema_period` after,
        // so late-joining viewers can discover channel metadata mid-run.
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
                    // Advance `last_schema_sent` before the send so any outcome (success,
                    // WouldBlock, persistent error) waits a full `schema_period` before retry.
                    // Schema send failures are not counted into `dropped_frames` (data rows only).
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

        let msg = ReportingMessage::Row {
            seq: self.seq,
            timestamp: timestamp as f64 * 1e-9,
            system_time: super::fmt_time(time),
            values: channel_values,
        };

        self.scratch_buf.clear();
        if let Err(e) = msg.encode_into(&mut self.scratch_buf) {
            warn!("ReportingDispatcher: postcard encode error (skipping frame): {e}");
            return Ok(());
        }

        // Advance seq before send so the wire sequence is accounted for on any outcome — the
        // viewer's NaN-gap detector then sees a gap even when the send errored.
        self.seq += 1;

        match socket.send_to(&self.scratch_buf, *multicast_addr) {
            Ok(_) => {}
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                self.dropped_frames.fetch_add(1, Ordering::Relaxed);
            }
            Err(e) => {
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
        // Final Schema with `is_session_end=true` lets viewers distinguish a clean shutdown from
        // a stale connection. Errors are swallowed — terminate must never return Err.
        if let (Some(socket), Some(multicast_addr), Some(stored_schema)) = (
            self.socket.as_ref(),
            self.multicast_addr.as_ref(),
            self.stored_schema.take(),
        ) {
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
                    // Block on the final send so a momentarily-full kernel buffer cannot swallow
                    // the session-end marker. The socket drops a few lines down, so no undo.
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

        // Drop the socket and reset runtime state. `dropped_frames` survives — handle holders
        // read the final count after the run ends; the next `init` resets it.
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
