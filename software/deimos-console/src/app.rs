use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use eframe::egui;
use egui_plot::{Line, Plot, PlotPoints};

use deimos::dispatcher::ReportingMessage;

use crate::config::DeimosConsoleConfig;
use crate::forensic::ForensicLog;
use crate::receiver::{receiver_dropped_frames, receiver_overwritten_frames};

// Muted status-indicator palette.
//
// Egui's built-in `Color32::{YELLOW, GREEN, RED}` are saturated screen primaries that are hard
// to read on both the default dark theme and any lighter theme. These shades sit in a mid-
// saturation range that reads cleanly against either background, and they are reserved for
// *dot glyphs* — the text next to them is always rendered with the theme's default foreground.
const MINT_GREEN: egui::Color32 = egui::Color32::from_rgb(110, 200, 140);
const AMBER: egui::Color32 = egui::Color32::from_rgb(230, 180, 60);
const CORAL_RED: egui::Color32 = egui::Color32::from_rgb(220, 100, 100);
const SKY_BLUE: egui::Color32 = egui::Color32::from_rgb(120, 180, 240);
const ORCHID: egui::Color32 = egui::Color32::from_rgb(190, 120, 220);

/// Six-state connection health indicator for the operator console.
///
/// Computed each UI frame from the current schema state, the wall-clock time of
/// the last received `Row`, the wall-clock time of the most recent stall, and
/// whether the receiver thread is still alive.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ConnectionHealth {
    /// No `Schema` has been received yet; cannot assess liveness.
    NoSchemaYet,
    /// A `Row` was received within the staleness threshold.
    Fresh,
    /// A stall was detected within the last `recovery_settle_secs` and rows are once
    /// again flowing fresh. The indicator stays in this state for the configured
    /// settle window so the operator notices the discontinuity even if they glance
    /// away. Distinct from [`ConnectionHealth::Fresh`] (data flowing, no recent
    /// stall) and from [`ConnectionHealth::Stale`] (no data flowing at all).
    Recovering,
    /// A `Schema` exists but no `Row` has arrived, or the last `Row` arrived more
    /// than `staleness_threshold` ago.
    Stale,
    /// The controller emitted a session-end `Schema`, signalling a clean shutdown.
    /// Operationally distinct from `Stale` (which means the stream went silent for
    /// an unknown reason): here the controller said goodbye.
    SessionEnded,
    /// The receiver thread has exited due to a non-transient socket error. No further
    /// packets will arrive in this session without restarting the console.
    ReceiverDead,
}

/// Maximum `Row` messages buffered before a `Schema` is received.
/// When full, the oldest entry is dropped so the most recent N rows are available on schema arrival.
const MAX_PENDING_ROWS: usize = 1000;

/// Why a stall was flagged. Both signals lead to the same buffer-clear handling, but recording
/// which one fired is useful in the per-tick telemetry and post-mortem log analysis.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum StallTrigger {
    /// The wall-clock gap between the previous and current drain pass exceeded
    /// `staleness_threshold_secs` — the UI thread was paused (hidden window, SIGSTOP, etc.)
    /// even though the receiver kept enqueueing rows behind us.
    UiPause,
    /// At least one drained row carried a `viewer_received_at - controller_system_time` lag
    /// above `staleness_threshold_secs`. The UI thread was healthy but the receiver fed us
    /// rows that were already old by the time we processed them — typically the bounded
    /// channel was at capacity and the drop-oldest policy is now flushing the catch-up tail.
    DataStaleness,
}

/// Outcome of [`DeimosConsoleApp::detect_stall`]. Carries the trigger and the freshest
/// controller timestamp from the drained batch so the buffer-clear step has a stable
/// reference for "newest sample on the plot".
#[derive(Clone, Copy, Debug)]
pub struct StallEvent {
    /// Which of the two stall signals fired.
    pub trigger: StallTrigger,
    /// Largest controller `timestamp` (`Row::timestamp`, in seconds since session start) among
    /// the rows in the batch that triggered detection. Used as the cutoff for pre-stall buffer
    /// clearing and as the x-coordinate anchor for the discontinuity sentinel.
    pub freshest_ts: f64,
}

/// A stall whose buffer-mutation step (eviction + sentinel push) has been deferred until
/// after the catch-up batch's rows have been appended to the per-channel buffers.
///
/// Two reasons we defer the mutation:
///
/// 1. **Insertion order vs. wall-clock order.** `egui_plot` renders points in their buffer
///    insertion order, not in time order. If we pushed the sentinel before the catch-up
///    rows ran through `process_row`, any catch-up row whose `timestamp < sentinel_ts`
///    would be inserted *after* the sentinel and the rendered line would draw from the
///    sentinel back to the older timestamps — the stale data would still be visible.
/// 2. **Frozen freeze.** While the operator has the view frozen we must not mutate the
///    buffers (it would clobber the history they paused to inspect), but the spec still
///    requires a sentinel marking the stall once they unfreeze. Recording the pending
///    stall lets the next non-frozen drain insert the sentinel.
#[derive(Clone, Copy, Debug)]
struct PendingStall {
    /// `freshest_ts - 1e-9`; the x-coordinate at which the NaN sentinel will be pushed.
    sentinel_ts: f64,
    /// The original `freshest_ts` from the triggering batch. Drives the eviction cutoff
    /// `freshest_ts - tail_keep_secs` so we keep a small contextual tail of pre-stall
    /// samples and drop everything older.
    freshest_ts: f64,
}

/// Per-channel ring buffer cap while frozen, expressed as a multiple of `window_seconds`.
///
/// During freeze, the live-window eviction is too aggressive (it would discard the data the
/// operator paused to inspect), but unbounded growth lets a long freeze OOM the viewer. We cap
/// at `FREEZE_BUFFER_WINDOW_MULTIPLE * window_seconds` of history per channel: enough that
/// unfreezing reveals what happened during the pause for any reasonable freeze duration, and
/// proportional to the window size the operator already configured.
const FREEZE_BUFFER_WINDOW_MULTIPLE: f64 = 10.0;

/// Metadata describing the current session's channel layout.
///
/// Populated when the first `Schema` packet is received. Replaced when a new non-session-end
/// `Schema` arrives (e.g. a periodic re-emission from the controller).
pub struct SchemaState {
    /// Ordered channel names, parallel to `channel_units` and to `Row::values`.
    pub channel_names: Vec<String>,
    /// Per-channel unit labels. `None` means unknown or not applicable.
    /// Used by the renderer to label plot y-axes.
    pub channel_units: Vec<Option<String>>,
    /// Reverse-lookup: channel name → index into `channel_names` / `Row::values`.
    pub name_to_index: HashMap<String, usize>,
    /// Set to `true` when a `Schema { is_session_end: true }` packet has been received.
    /// Read by [`DeimosConsoleApp::connection_health`] to surface
    /// [`ConnectionHealth::SessionEnded`], which the status bar then renders.
    pub session_end_received: bool,
    /// Monotonic clock epoch anchor from the Schema, used to compute synthetic timestamps
    /// for NaN gap sentinels when the exact gap time is not known.
    pub monotonic_epoch_ns: u64,
}

impl SchemaState {
    fn new(
        channel_names: Vec<String>,
        channel_units: Vec<Option<String>>,
        session_end_received: bool,
        monotonic_epoch_ns: u64,
    ) -> Self {
        let name_to_index = channel_names
            .iter()
            .enumerate()
            .map(|(i, n)| (n.clone(), i))
            .collect();
        Self {
            channel_names,
            channel_units,
            name_to_index,
            session_end_received,
            monotonic_epoch_ns,
        }
    }
}

/// Main application state for the Deimos operator console.
pub struct DeimosConsoleApp {
    config: DeimosConsoleConfig,
    /// Incoming messages from the receiver thread. Drained each frame in `update`.
    rx: crossbeam_channel::Receiver<ReportingMessage>,
    /// Current session schema, if one has been received.
    schema: Option<SchemaState>,
    /// `Row` messages received before a `Schema` has arrived, bounded at [`MAX_PENDING_ROWS`].
    pending_rows: VecDeque<ReportingMessage>,
    /// Missing-channel warnings: `(panel_title, channel_name)` pairs discovered at schema reconciliation.
    missing_channels: Vec<(String, String)>,
    /// Per-channel scrolling ring buffer: `channel_name → [(timestamp, value)]` oldest-first.
    /// A `f64::NAN` value is a line-break sentinel so `egui_plot` renders visible gap discontinuities.
    channel_data: HashMap<String, VecDeque<(f64, f64)>>,
    /// Wall-clock viewer time at which the most recent `Row` was received.
    last_row_received_at: Option<Instant>,
    /// Sequence number of the last successfully processed `Row`; `None` before the first row or on new session.
    last_seen_seq: Option<u64>,
    /// Accumulated count of sequence gaps detected on the wire (each N-frame gap contributes N).
    dropped_frames_on_wire: u64,
    /// When `true`, x-axis scrolling is halted while the receiver continues feeding ring buffers.
    frozen: bool,
    /// Controller timestamp captured when freeze was engaged; pins the visible window at
    /// `[frozen_xmax - window_seconds, frozen_xmax]`.
    frozen_xmax: Option<f64>,
    /// Per-session forensic log; `None` if not configured or if the file could not be opened.
    forensic_log: Option<ForensicLog>,
    /// Set when the forensic log is permanently disabled due to an open or write error.
    /// Displayed prominently in the status bar so the operator knows forensics stopped.
    forensic_disabled_reason: Option<String>,
    /// Count of `Row` messages dropped because their channel count mismatched the schema.
    schema_drift_drops: u64,
    /// Count of `Row` messages dropped from the pre-schema pending queue due to overflow.
    pending_overflow_drops: u64,
    /// Panels for which a mixed-unit warning has already been emitted (by panel index).
    warned_panels: HashSet<usize>,
    /// Set to `true` when the receiver channel disconnects (receiver thread has exited).
    /// Once set, this flag is sticky for the lifetime of the app.
    receiver_dead: bool,
    /// Wall-clock instant of the last per-second telemetry tick emitted to stderr.
    last_tick_at: Option<Instant>,
    /// Count of `Row` messages processed since the last telemetry tick.
    rows_since_last_tick: u64,
    /// Previous reported connection-health state; used to log transitions.
    last_health_logged: Option<ConnectionHealth>,
    /// Consecutive ticks with zero rows; drives idle-tick suppression so long quiet
    /// periods don't fill the log with identical zero-rate lines.
    consecutive_idle_ticks: u64,
    /// Sum of per-row rx-lag samples (ms) accumulated since the last tick.
    lag_sum_ms: f64,
    /// Minimum rx-lag sample (ms) since the last tick; `f64::INFINITY` when no samples yet.
    lag_min_ms: f64,
    /// Maximum rx-lag sample (ms) since the last tick; `f64::NEG_INFINITY` when no samples yet.
    lag_max_ms: f64,
    /// Wall-clock instant at which the previous `drain_messages` pass finished, or `None` before
    /// the first drain. Compared against `Instant::now()` at the start of the next pass to detect
    /// UI-thread pauses longer than `staleness_threshold_secs`.
    last_drain_at: Option<Instant>,
    /// Count of stall events flagged by [`DeimosConsoleApp::detect_stall`] since process start.
    /// Surfaced in the per-tick telemetry; an operator-visible signal that the live plot has
    /// jumped past stale data.
    stalls_detected: u64,
    /// Count of pre-stall samples popped from per-channel ring buffers across all stall events.
    /// Distinct from `dropped_frames_on_wire` (which counts wire-side gaps): this is the visual
    /// "stale segment" that the operator did not see live but that the forensic log retains.
    stale_rows_evicted: u64,
    /// Wall-clock instant at which the most recent stall fired, or `None` if no stall has
    /// been seen yet (or never since process start). Drives the
    /// [`ConnectionHealth::Recovering`] state: the indicator stays in `Recovering` while
    /// `last_stall_at.elapsed() <= recovery_settle_secs` so the operator notices the
    /// discontinuity for at least the configured settle window.
    last_stall_at: Option<Instant>,
    /// Stall whose buffer-mutation step has been deferred until after the current drain's
    /// catch-up rows have been processed (or, if the view is frozen, until the operator
    /// unfreezes and the next non-empty drain runs). See [`PendingStall`] for the rationale.
    /// Cleared once the deferred eviction + sentinel push has been applied.
    pending_stall: Option<PendingStall>,
}

impl DeimosConsoleApp {
    pub fn new(
        config: DeimosConsoleConfig,
        rx: crossbeam_channel::Receiver<ReportingMessage>,
    ) -> Self {
        Self {
            config,
            rx,
            schema: None,
            pending_rows: VecDeque::new(),
            missing_channels: Vec::new(),
            channel_data: HashMap::new(),
            last_row_received_at: None,
            last_seen_seq: None,
            dropped_frames_on_wire: 0,
            frozen: false,
            frozen_xmax: None,
            forensic_log: None,
            forensic_disabled_reason: None,
            schema_drift_drops: 0,
            pending_overflow_drops: 0,
            warned_panels: HashSet::new(),
            receiver_dead: false,
            last_tick_at: None,
            rows_since_last_tick: 0,
            last_health_logged: None,
            consecutive_idle_ticks: 0,
            lag_sum_ms: 0.0,
            lag_min_ms: f64::INFINITY,
            lag_max_ms: f64::NEG_INFINITY,
            last_drain_at: None,
            stalls_detected: 0,
            stale_rows_evicted: 0,
            last_stall_at: None,
            pending_stall: None,
        }
    }

    /// Emit a per-second telemetry line to stderr with row rate, rx-lag stats, and
    /// drop / stall counters. Suppresses repeat emissions during long idle periods — after
    /// three consecutive zero-row ticks, only one heartbeat per minute is printed.
    ///
    /// Runs at most once per second; the first call after startup only arms the timer.
    /// Safe to call every UI frame.
    ///
    /// # Tick line keys
    ///
    /// All fields after the `tick` token are rendered as `key=value` so external tooling
    /// can parse the line by splitting on whitespace and `=`. Adding a new key is non-
    /// breaking.
    ///
    /// | Key | Meaning |
    /// |---|---|
    /// | `rows` | `Row` messages processed since the previous tick. |
    /// | `rate` | Rows per second over the elapsed tick interval (Hz). |
    /// | `last_seq` | Sequence number of the most recently processed `Row`, or `-` before the first row. |
    /// | `wire_drops` | Cumulative count of sequence-number gaps observed on the wire (controller-side or network packet loss). |
    /// | `recv_drops` | Cumulative count of messages the receiver thread could not enqueue at all — zero in steady state under drop-oldest semantics; non-zero indicates a benign race with the UI's own drain. |
    /// | `overwritten_frames` | Cumulative count of receiver-thread drop-oldest evictions: messages that displaced an older queued message because the bounded channel was full. Grows under sustained backpressure while preserving freshness. |
    /// | `stalls_detected` | Cumulative count of stall events flagged by [`DeimosConsoleApp::detect_stall`]. Each event triggers a `Recovering` health transition immediately; the matching buffer clear and discontinuity sentinel land on the same drain when not frozen, or are deferred to the first non-frozen drain otherwise. |
    /// | `stale_rows_evicted` | Cumulative count of pre-stall samples popped from per-channel ring buffers across all stall events. Distinct from `wire_drops`: this is the visual "stale segment" the operator did not see live but that the forensic log retains. |
    /// | `pending` | Current depth of the pre-schema pending-rows queue. |
    /// | `lag_ms` | Optional `min/mean/max` triple of per-row rx-lag (viewer receipt minus controller cycle-start wall clock), only present when at least one row was processed this tick. |
    /// | `(idle Ns)` | Optional suffix when the tick is part of a deep-idle heartbeat. |
    fn maybe_emit_tick(&mut self) {
        const IDLE_GRACE: u64 = 3;
        const IDLE_HEARTBEAT_EVERY: u64 = 60;

        let now = Instant::now();
        let elapsed = match self.last_tick_at {
            Some(t) => now.duration_since(t),
            None => {
                self.last_tick_at = Some(now);
                return;
            }
        };
        if elapsed < Duration::from_secs(1) {
            return;
        }

        let rows = self.rows_since_last_tick;
        let is_idle = rows == 0;
        self.consecutive_idle_ticks = if is_idle {
            self.consecutive_idle_ticks + 1
        } else {
            0
        };

        // Decide whether to actually emit this tick.
        //   Active / just-went-idle:  always emit.
        //   Deep idle (>= IDLE_GRACE consecutive): emit only every IDLE_HEARTBEAT_EVERY ticks.
        let emit = !is_idle
            || self.consecutive_idle_ticks <= IDLE_GRACE
            || self
                .consecutive_idle_ticks
                .is_multiple_of(IDLE_HEARTBEAT_EVERY);

        if emit {
            let idle_age = if is_idle {
                Some(self.consecutive_idle_ticks)
            } else {
                None
            };
            eprintln!(
                "{} {}",
                wall_clock_prefix(),
                self.format_tick_line(rows, elapsed, idle_age, IDLE_GRACE),
            );
        }

        self.rows_since_last_tick = 0;
        self.lag_sum_ms = 0.0;
        self.lag_min_ms = f64::INFINITY;
        self.lag_max_ms = f64::NEG_INFINITY;
        self.last_tick_at = Some(now);
    }

    /// Build the per-second telemetry tick line (without the wall-clock prefix).
    ///
    /// Factored out of [`Self::maybe_emit_tick`] so unit tests can assert on the exact key set
    /// without capturing stderr. `idle_age` is `Some(n)` when the tick is part of a deep-idle
    /// heartbeat (n consecutive idle ticks) and `None` otherwise; the `(idle Ns)` suffix is
    /// appended only when `n > idle_grace`.
    fn format_tick_line(
        &self,
        rows: u64,
        elapsed: Duration,
        idle_age: Option<u64>,
        idle_grace: u64,
    ) -> String {
        let rate = rows as f64 / elapsed.as_secs_f64();
        let last_seq = self
            .last_seen_seq
            .map(|s| s.to_string())
            .unwrap_or_else(|| "-".to_string());
        let lag_segment = if rows > 0 && self.lag_min_ms.is_finite() {
            let mean = self.lag_sum_ms / rows as f64;
            format!(
                " lag_ms={:.2}/{:.2}/{:.2}(min/mean/max)",
                self.lag_min_ms, mean, self.lag_max_ms
            )
        } else {
            String::new()
        };
        let idle_suffix = match idle_age {
            Some(n) if n > idle_grace => format!(" (idle {n}s)"),
            _ => String::new(),
        };
        format!(
            "deimos-console: tick rows={rows} rate={rate:.1}Hz last_seq={last_seq} wire_drops={} recv_drops={} overwritten_frames={} stalls_detected={} stale_rows_evicted={} pending={}{lag_segment}{idle_suffix}",
            self.dropped_frames_on_wire,
            receiver_dropped_frames(),
            receiver_overwritten_frames(),
            self.stalls_detected,
            self.stale_rows_evicted,
            self.pending_rows.len(),
        )
    }

    /// Emit a stderr line when `connection_health()` changes.
    fn maybe_log_health_transition(&mut self) {
        let current = self.connection_health();
        if self.last_health_logged != Some(current) {
            eprintln!(
                "{} deimos-console: health -> {current:?}",
                wall_clock_prefix()
            );
            self.last_health_logged = Some(current);
        }
    }

    /// Reconcile the panel config against the newly-arrived schema and populate
    /// `missing_channels` with any panel channel that is not declared in the schema.
    ///
    /// Also emits a one-time `eprintln!` warning for panels whose configured channels carry
    /// heterogeneous declared units (e.g. mixing V and K on the same plot). The warning fires
    /// once per panel per session to avoid log spam on periodic schema re-emission.
    fn reconcile_panels(&mut self) {
        let Some(schema) = &self.schema else { return };
        self.missing_channels.clear();
        for (panel_idx, panel) in self.config.panels.iter().enumerate() {
            for ch in &panel.channels {
                if !schema.name_to_index.contains_key(ch.as_str()) {
                    self.missing_channels
                        .push((panel.title.clone(), ch.clone()));
                }
            }

            // Check for mixed units across this panel's channels.
            if !self.warned_panels.contains(&panel_idx) {
                let declared_units: Vec<&str> = panel
                    .channels
                    .iter()
                    .filter_map(|ch| {
                        schema
                            .name_to_index
                            .get(ch.as_str())
                            .and_then(|&idx| schema.channel_units.get(idx))
                            .and_then(|u| u.as_deref())
                    })
                    .collect();

                if declared_units.len() >= 2 {
                    let first = declared_units[0];
                    let mixed = declared_units[1..].iter().any(|&u| u != first);
                    if mixed {
                        eprintln!(
                            "warning: panel '{}' has mixed units: {:?}",
                            panel.title, declared_units
                        );
                        self.warned_panels.insert(panel_idx);
                    }
                }
            }
        }
    }

    /// Insert a `Row` into the per-channel ring buffers and evict samples outside the window.
    ///
    /// Rows whose channel count mismatches the schema are silently skipped (schema drift).
    /// Sequence gaps insert a `(gap_ts, NAN)` sentinel so `egui_plot` renders a visible break.
    /// While frozen, eviction widens to `FREEZE_BUFFER_WINDOW_MULTIPLE * window_seconds` so the
    /// operator can unfreeze and see what happened during the pause without the buffer growing
    /// unbounded over a long freeze.
    fn process_row(
        &mut self,
        viewer_received_at: SystemTime,
        seq: u64,
        timestamp: f64,
        system_time: &str,
        values: Vec<f64>,
    ) {
        let Some(schema) = &self.schema else { return };
        if values.len() != schema.channel_names.len() {
            self.schema_drift_drops += 1;
            return;
        }

        let gap_detected = if let Some(last) = self.last_seen_seq {
            if seq >= last {
                let expected_next = last + 1;
                if seq > expected_next {
                    self.dropped_frames_on_wire += seq - expected_next;
                    true
                } else {
                    false
                }
            } else {
                // Out-of-order packet: skip gap counting, don't update last_seen_seq.
                false
            }
        } else {
            false
        };
        // Only advance the high-water mark on non-decreasing sequence numbers.
        if self.last_seen_seq.is_none_or(|last| seq >= last) {
            self.last_seen_seq = Some(seq);
        }
        self.last_row_received_at = Some(Instant::now());

        let window = self.config.window_seconds;
        let cutoff = if self.frozen {
            timestamp - FREEZE_BUFFER_WINDOW_MULTIPLE * window
        } else {
            timestamp - window
        };

        for (i, &value) in values.iter().enumerate() {
            let name = schema.channel_names[i].clone();
            let buf = self.channel_data.entry(name).or_default();

            if gap_detected {
                let gap_ts = timestamp - 1e-9;
                buf.push_back((gap_ts, f64::NAN));
            }

            buf.push_back((timestamp, value));

            while buf.front().is_some_and(|&(t, _)| t < cutoff) {
                buf.pop_front();
            }
        }

        if let Some(log) = &mut self.forensic_log {
            if let Err(e) = log.write_row(viewer_received_at, seq, timestamp, system_time, &values)
            {
                let reason = format!("write failed: {e}");
                eprintln!(
                    "{} deimos-console: forensic log {reason}; disabling log",
                    wall_clock_prefix()
                );
                self.forensic_disabled_reason = Some(reason);
                self.forensic_log = None;
            }
        }

        self.rows_since_last_tick += 1;

        // Compute rx-lag as viewer wall-clock receipt time minus the controller's cycle-start
        // wall clock. `system_time` is `SystemTime::now()` sampled at the top of the
        // controller's cycle (controller/mod.rs:1097) and handed through to `consume()`
        // verbatim, serialized as RFC3339-nanos UTC, so "lag" is the total staleness of the
        // data from the cycle's own clock reference to when the operator sees it.
        //
        // Contributors (approximate, 20 Hz loopback):
        //   - ~cycle_duration of controller-internal work between the cycle-start stamp and
        //     the dispatcher.consume() call (peripheral I/O + calcs)
        //   - UDP transmit + loopback/LAN hop
        //   - receiver-thread decode + bounded-channel traversal
        //   - UI-thread drain on its ~33 ms repaint tick
        //   - any clock skew between controller and viewer hosts
        //
        // At 20 Hz on loopback expect ~50-70 ms, dominated by the first bullet. A growing
        // trend over a run usually means the viewer is falling behind (correlates with
        // recv_drops); a constant offset across hosts is clock skew. This is intentionally
        // NOT a pure transport measurement — it answers "how old is what the operator sees"
        // rather than "how fast is the network."
        //
        // Alternative tried: schema.monotonic_epoch_ns + row.timestamp. That anchor is taken
        // at dispatcher init() (during Configuring) but row.timestamp counts from start-of-
        // Operating ~100-200 ms later, so it has a constant false offset.
        if let (Ok(rx_ns), Ok(produced)) = (
            viewer_received_at
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_nanos() as i128),
            chrono::DateTime::parse_from_rfc3339(system_time),
        ) {
            // `timestamp_nanos_opt` returns `None` for years outside ~1678-2262. Skipping
            // those rows for lag accounting is preferable to substituting 1970, which would
            // pollute lag_min/max with a ~56-year spurious value on a misconfigured clock.
            if let Some(produced_ns) = produced.timestamp_nanos_opt() {
                let lag_ms = (rx_ns - produced_ns as i128) as f64 / 1.0e6;
                self.lag_sum_ms += lag_ms;
                if lag_ms < self.lag_min_ms {
                    self.lag_min_ms = lag_ms;
                }
                if lag_ms > self.lag_max_ms {
                    self.lag_max_ms = lag_ms;
                }
            }
        }
    }

    /// Inspect a freshly-drained batch of `Row` metadata and decide whether the UI thread or
    /// the data path stalled.
    ///
    /// Two complementary signals fire as one `StallEvent`:
    ///
    /// 1. **UI-pause** — `Instant::now() - last_drain_at > staleness_threshold_secs`. The
    ///    previous drain finished long ago, which means `update` itself was paused (hidden
    ///    window, SIGSTOP, OS scheduling glitch). The UI-pause check requires a baseline, so
    ///    the very first drain (`last_drain_at == None`) is exempt.
    /// 2. **Data-staleness** — any drained row's lag
    ///    `viewer_received_at - controller_system_time` exceeds the staleness threshold. The
    ///    UI thread is healthy but the receiver fed us pre-stall rows from the bounded
    ///    channel.
    ///
    /// Returns `None` if the batch is empty (no rows means no `freshest_ts` to anchor a clear)
    /// or neither signal fired.
    fn detect_stall(
        &mut self,
        viewer_received_at: SystemTime,
        drained_rows: &[(u64, f64, &str)],
    ) -> Option<StallEvent> {
        if drained_rows.is_empty() {
            return None;
        }
        let threshold = Duration::from_secs_f64(self.config.staleness_threshold_secs);

        let ui_pause = matches!(self.last_drain_at, Some(prev) if prev.elapsed() > threshold);

        // Data-staleness: any row whose `viewer_received_at - controller_system_time` exceeds
        // the threshold. Both timestamps are wall-clock; we accept clock skew between hosts
        // as part of the lag (consistent with how `process_row` reports `rx-lag`).
        let rx_ns = viewer_received_at
            .duration_since(UNIX_EPOCH)
            .ok()
            .map(|d| d.as_nanos() as i128);
        let threshold_ns = (self.config.staleness_threshold_secs * 1.0e9) as i128;
        let data_stale = rx_ns.is_some_and(|rx| {
            drained_rows.iter().any(|(_, _, system_time)| {
                chrono::DateTime::parse_from_rfc3339(system_time)
                    .ok()
                    .and_then(|dt| dt.timestamp_nanos_opt())
                    .is_some_and(|produced_ns| rx - produced_ns as i128 > threshold_ns)
            })
        });

        if !ui_pause && !data_stale {
            return None;
        }

        // Anchor the buffer-clear to the freshest controller timestamp in the batch — this
        // is the x-coordinate the live plot is about to settle on, so eviction below it
        // strictly removes "older than what we're about to draw".
        let freshest_ts = drained_rows
            .iter()
            .map(|(_, t, _)| *t)
            .fold(f64::NEG_INFINITY, f64::max);
        if !freshest_ts.is_finite() {
            return None;
        }

        let trigger = if ui_pause {
            StallTrigger::UiPause
        } else {
            StallTrigger::DataStaleness
        };
        Some(StallEvent {
            trigger,
            freshest_ts,
        })
    }

    /// Record a stall event and defer the buffer-mutation step (eviction + NaN sentinel push)
    /// until after the catch-up batch has been appended in [`Self::drain_messages`]'s second
    /// pass. The stall counters and `last_stall_at` are updated immediately so the per-tick
    /// telemetry and `Recovering` health indicator are accurate even while the buffer mutation
    /// is pending.
    ///
    /// **Why deferred?** `egui_plot` renders points in buffer insertion order, not time order.
    /// If we pushed the sentinel here (before the second pass), any catch-up row with
    /// `timestamp < sentinel_ts` would land *after* the sentinel in the buffer and the line
    /// would render from the sentinel back through the stale rows — the discontinuity break
    /// would not separate them visually. Deferring lets us run a single eviction-and-sentinel
    /// pass against the post-batch buffer state, where everything below the cutoff is gone
    /// and the sentinel can be spliced into the right insertion-order slot.
    ///
    /// **While frozen** the deferred application also serves the spec requirement that a
    /// stall sentinel MUST be inserted: we record the pending stall now, the per-channel
    /// buffers stay untouched while frozen (preserving the history the operator paused to
    /// inspect), and the next non-frozen drain materializes the sentinel.
    ///
    /// If a stall is already pending and another one fires before the first has been applied
    /// (e.g. multiple stalls inside a single freeze), the pending entry is widened to the
    /// later `freshest_ts` so the eventual eviction cutoff reflects the freshest available
    /// anchor and only a single sentinel is emitted.
    fn apply_stall(&mut self, stall: StallEvent) {
        let sentinel_ts = stall.freshest_ts - 1e-9;
        let merged = match self.pending_stall {
            Some(prev) if prev.freshest_ts >= stall.freshest_ts => prev,
            _ => PendingStall {
                sentinel_ts,
                freshest_ts: stall.freshest_ts,
            },
        };
        self.pending_stall = Some(merged);
        self.stalls_detected += 1;
        self.last_stall_at = Some(Instant::now());
        eprintln!(
            "{} deimos-console: stall detected ({:?}) freshest_ts={:.3} sentinel_at={:.9} frozen={} (deferred)",
            wall_clock_prefix(),
            stall.trigger,
            stall.freshest_ts,
            sentinel_ts,
            self.frozen,
        );
    }

    /// Apply any pending stall's buffer-mutation step: evict per-channel samples below
    /// `freshest_ts - tail_keep_secs`, then splice the NaN discontinuity sentinel into the
    /// deque just before the first entry whose `timestamp >= sentinel_ts`. Does nothing if
    /// there is no pending stall, or if the view is currently frozen (the operator paused to
    /// inspect history; we keep the pending stall recorded and apply it on the first
    /// non-frozen drain).
    ///
    /// Called once per [`Self::drain_messages`] pass, after the second pass has appended any
    /// catch-up rows to the per-channel buffers, so the eviction sees the full post-batch
    /// state and the sentinel ends up at the trailing edge of the retained tail.
    fn apply_pending_stall(&mut self) {
        if self.frozen {
            return;
        }
        let Some(pending) = self.pending_stall.take() else {
            return;
        };

        // Eviction must walk the whole deque, not just the front. The catch-up batch's stale
        // rows were appended to the back by the second pass, so a `pop_front`-only walk would
        // miss them; `retain` scans every entry and drops anything below the tail-keep cutoff.
        // At typical buffer sizes (a few hundred entries) the cost is negligible — this only
        // runs on stall recovery, not per-row.
        //
        // Sentinel placement: `egui_plot` renders points in insertion order, so the sentinel
        // must sit BEFORE the freshest post-stall sample to draw a proper line break. After
        // retain, the deque is in (preserved) insertion order; we splice the sentinel in just
        // before the first entry whose `timestamp >= sentinel_ts` (i.e., the freshest sample
        // of the triggering batch and anything later). Anything earlier is retained tail —
        // those entries draw with a connecting line up to the sentinel, then the line breaks.
        let cutoff = pending.freshest_ts - self.config.tail_keep_secs;
        let mut popped: u64 = 0;
        for buf in self.channel_data.values_mut() {
            let before = buf.len();
            buf.retain(|&(t, _)| t >= cutoff);
            popped += (before - buf.len()) as u64;

            let split_at = buf
                .iter()
                .position(|&(t, _)| t >= pending.sentinel_ts)
                .unwrap_or(buf.len());
            buf.insert(split_at, (pending.sentinel_ts, f64::NAN));
        }
        self.stale_rows_evicted += popped;
        eprintln!(
            "{} deimos-console: stall applied freshest_ts={:.3} cleared={} sentinel_at={:.9}",
            wall_clock_prefix(),
            pending.freshest_ts,
            popped,
            pending.sentinel_ts,
        );
    }

    /// Drain all currently-available messages from the receiver channel and update state.
    ///
    /// Called once per `update` (i.e. once per repaint) so the egui-driven UI sees the latest
    /// data. The bounded receiver channel decouples the network thread from the UI; this method
    /// is its sink. If `try_recv` reports the channel is disconnected (the receiver thread has
    /// exited on a non-transient socket error), the sticky `receiver_dead` flag is set so the
    /// status bar can render a distinct state instead of pretending the link is still alive.
    ///
    /// The drain happens in two passes: first pull every available message into a local buffer
    /// so [`Self::detect_stall`] can inspect the full batch and decide whether a stall fired;
    /// then process each message normally. After the second pass, [`Self::apply_pending_stall`]
    /// runs to evict pre-stall samples and push the discontinuity sentinel. The eviction is
    /// deferred to *after* the second pass so the sentinel sits at the trailing edge of the
    /// retained tail in buffer-insertion order — `egui_plot` renders by insertion order, so
    /// pushing the sentinel before the catch-up rows would let any row with
    /// `timestamp < sentinel_ts` draw the line back through the stale data.
    ///
    /// Extracted from `update` so unit tests can exercise the message-handling logic without
    /// spinning up an `egui::Context`.
    pub(crate) fn drain_messages(&mut self) {
        // First pass: pull every available message into a local buffer.
        let mut drained: Vec<ReportingMessage> = Vec::new();
        loop {
            match self.rx.try_recv() {
                Err(crossbeam_channel::TryRecvError::Disconnected) => {
                    self.receiver_dead = true;
                    break;
                }
                Err(crossbeam_channel::TryRecvError::Empty) => break,
                Ok(msg) => drained.push(msg),
            }
        }

        // Stall detection needs one wall-clock anchor for the whole batch so every row in the
        // catch-up burst is evaluated against a single consistent "now" — without that, rows
        // processed later in the loop would compare against a slightly later wall-clock and
        // the data-staleness signal could flap mid-batch. This anchor is consumed only by
        // `detect_stall`. Per-row `viewer_received_at` for `process_row`'s lag accounting is
        // captured separately in the second pass below (see the rx-lag commentary there).
        let viewer_received_at = SystemTime::now();
        let row_meta: Vec<(u64, f64, &str)> = drained
            .iter()
            .filter_map(|m| match m {
                ReportingMessage::Row {
                    seq,
                    timestamp,
                    system_time,
                    ..
                } => Some((*seq, *timestamp, system_time.as_str())),
                _ => None,
            })
            .collect();

        // Capture the non-empty flag before the per-message loop below moves `drained`. Used
        // at the end of the function to decide whether to bump `last_drain_at`.
        let drained_any = !drained.is_empty();
        if let Some(stall) = self.detect_stall(viewer_received_at, &row_meta) {
            self.apply_stall(stall);
        }

        // Second pass: process each drained message with existing per-message logic.
        for msg in drained {
            match msg {
                ReportingMessage::Schema {
                    channel_names,
                    channel_units,
                    is_session_end,
                    monotonic_epoch_ns,
                } => {
                    // Detect a new session by comparing the epoch anchor. If the epoch
                    // changed (or no schema existed yet), reset gap-tracking state and
                    // clear stale channel data so we don't draw across session boundaries.
                    let new_session = self
                        .schema
                        .as_ref()
                        .is_none_or(|s| s.monotonic_epoch_ns != monotonic_epoch_ns);

                    let schema_kind = if is_session_end {
                        "session-end"
                    } else if new_session {
                        "new-session"
                    } else {
                        "re-emission"
                    };
                    let units_declared = channel_units.iter().filter(|u| u.is_some()).count();
                    eprintln!(
                        "{} deimos-console: schema [{schema_kind}] channels={} units_declared={} epoch_ns={monotonic_epoch_ns}",
                        wall_clock_prefix(),
                        channel_names.len(),
                        units_declared,
                    );

                    if new_session {
                        self.last_seen_seq = None;
                        self.dropped_frames_on_wire = 0;
                        self.channel_data.clear();

                        // Open a fresh forensic log for the new session.
                        // Each session gets a unique filename so prior sessions are never
                        // overwritten on viewer restart. The session stamp is derived from
                        // the wall clock at schema receipt time.
                        self.forensic_log = None;
                        self.forensic_disabled_reason = None;
                        if let Some(ref base_path) = self.config.forensic_log_path.clone() {
                            let session_path = session_stamped_path(base_path);
                            match ForensicLog::new(&session_path, &channel_names) {
                                Ok(log) => {
                                    self.forensic_log = Some(log);
                                }
                                Err(e) => {
                                    let reason =
                                        format!("open failed: {e} ({})", session_path.display());
                                    eprintln!(
                                        "{} deimos-console: forensic log {reason}",
                                        wall_clock_prefix()
                                    );
                                    self.forensic_disabled_reason = Some(reason);
                                }
                            }
                        }
                    }

                    self.schema = Some(SchemaState::new(
                        channel_names,
                        channel_units,
                        is_session_end,
                        monotonic_epoch_ns,
                    ));

                    // Reconcile panel config against the declared channels.
                    self.reconcile_panels();

                    // Drain any rows that arrived before the schema.
                    // Use SystemTime::now() as the viewer_received_at for buffered rows;
                    // the exact receipt time was not recorded before the schema was known.
                    let drain_time = SystemTime::now();
                    let buffered = std::mem::take(&mut self.pending_rows);
                    for pending in buffered.into_iter() {
                        if let ReportingMessage::Row {
                            seq,
                            timestamp,
                            system_time,
                            values,
                        } = pending
                        {
                            self.process_row(drain_time, seq, timestamp, &system_time, values);
                        }
                    }
                }

                ReportingMessage::Row {
                    seq,
                    timestamp,
                    system_time,
                    values,
                } => {
                    if self.schema.is_some() {
                        let viewer_received_at = SystemTime::now();
                        self.process_row(viewer_received_at, seq, timestamp, &system_time, values);
                    } else {
                        // Buffer the full message until a Schema arrives.
                        // The original system_time is preserved so the forensic log
                        // can use it when draining buffered rows after schema arrival.
                        if self.pending_rows.len() >= MAX_PENDING_ROWS {
                            self.pending_rows.pop_front();
                            self.pending_overflow_drops += 1;
                        }
                        self.pending_rows.push_back(ReportingMessage::Row {
                            seq,
                            timestamp,
                            system_time,
                            values,
                        });
                    }
                }
            }
        }

        // Apply any pending stall AFTER the second pass has appended catch-up rows. Two
        // reasons this is deferred to here rather than firing at `apply_stall` time:
        //   1. `egui_plot` renders points in insertion order. If we pushed the sentinel
        //      before the catch-up rows, any catch-up row with `timestamp < sentinel_ts`
        //      would land *after* the sentinel in the buffer and the rendered line would
        //      draw back through the stale data.
        //   2. While frozen, the deferred sentinel waits in `pending_stall` until the next
        //      non-frozen drain — so a stall that fires during freeze still surfaces a
        //      visible discontinuity once the operator unfreezes (spec requirement).
        // `apply_pending_stall` is a no-op if no stall is pending or if the view is frozen.
        self.apply_pending_stall();

        // Non-empty drain: bump the baseline. Empty drain: leave it alone — otherwise an idle
        // UI tick fired during a paused recv thread (SIGSTOP, scheduler starvation, etc.)
        // would mask the pause and the next non-empty drain would skip stall detection.
        // Regression: `empty_drain_does_not_advance_last_drain_at`.
        if drained_any {
            self.last_drain_at = Some(Instant::now());
        }
    }

    /// Compute the current connection health state.
    ///
    /// Precedence (highest first), so an operator sees the most-actionable label when several
    /// states could apply at once:
    ///
    /// 1. [`ConnectionHealth::ReceiverDead`] — receiver thread exited; nothing else matters.
    /// 2. [`ConnectionHealth::NoSchemaYet`] — never had a stream to assess.
    /// 3. [`ConnectionHealth::SessionEnded`] — controller signalled clean shutdown; takes
    ///    precedence over any prior `Recovering` so a stall just before shutdown is not
    ///    masked.
    /// 4. [`ConnectionHealth::Stale`] — no `Row` for longer than `staleness_threshold_secs`.
    ///    Outranks `Recovering` because "no data flowing" is more urgent than "data flowing
    ///    but recently bumpy".
    /// 5. [`ConnectionHealth::Recovering`] — last stall fired within `recovery_settle_secs`
    ///    and rows are otherwise fresh. Sticky for the settle window so the operator catches
    ///    the discontinuity even on a brief glance.
    /// 6. [`ConnectionHealth::Fresh`] — `Row` within the staleness threshold and no recent
    ///    stall.
    pub(crate) fn connection_health(&self) -> ConnectionHealth {
        if self.receiver_dead {
            return ConnectionHealth::ReceiverDead;
        }
        let Some(schema) = self.schema.as_ref() else {
            return ConnectionHealth::NoSchemaYet;
        };
        if schema.session_end_received {
            return ConnectionHealth::SessionEnded;
        }
        let staleness = Duration::from_secs_f64(self.config.staleness_threshold_secs);
        let fresh = matches!(self.last_row_received_at, Some(t) if t.elapsed() <= staleness);
        if !fresh {
            return ConnectionHealth::Stale;
        }
        let recovery = Duration::from_secs_f64(self.config.recovery_settle_secs);
        if matches!(self.last_stall_at, Some(t) if t.elapsed() <= recovery) {
            return ConnectionHealth::Recovering;
        }
        ConnectionHealth::Fresh
    }
}

impl eframe::App for DeimosConsoleApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.drain_messages();

        self.maybe_log_health_transition();
        self.maybe_emit_tick();

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.heading("Deimos Console");
                ui.label(format!(
                    "Listening on {}:{}",
                    self.config.multicast_group, self.config.port
                ));

                let freeze_label = if self.frozen { "Unfreeze" } else { "Freeze" };
                if ui.button(freeze_label).clicked() {
                    if self.frozen {
                        self.frozen = false;
                        self.frozen_xmax = None;

                        // Trim ring buffers back to the live window now that eviction resumes.
                        if let Some(latest_ts) = self
                            .channel_data
                            .values()
                            .flat_map(|buf| buf.back())
                            .map(|&(t, _)| t)
                            .reduce(f64::max)
                        {
                            let cutoff = latest_ts - self.config.window_seconds;
                            for buf in self.channel_data.values_mut() {
                                while buf.front().is_some_and(|&(t, _)| t < cutoff) {
                                    buf.pop_front();
                                }
                            }
                        }
                    } else {
                        self.frozen = true;
                        self.frozen_xmax = self
                            .channel_data
                            .values()
                            .flat_map(|buf| buf.back())
                            .map(|&(t, _)| t)
                            .reduce(f64::max);
                    }
                }

                if self.frozen {
                    ui.colored_label(egui::Color32::from_rgb(100, 180, 255), "[FROZEN]");
                }
            });
            ui.separator();

            // Connection-health indicator.
            //
            // The filled circle (\u25cf) acts as a colored status dot followed by a short
            // human-readable label describing the current state.
            {
                let health = self.connection_health();

                let (dot_color, label): (egui::Color32, String) = match health {
                    ConnectionHealth::NoSchemaYet => (
                        AMBER,
                        format!(
                            "No schema yet ({} row(s) buffered)",
                            self.pending_rows.len()
                        ),
                    ),
                    ConnectionHealth::Fresh => {
                        let ch_count = self
                            .schema
                            .as_ref()
                            .map_or(0, |s| s.channel_names.len());
                        (
                            MINT_GREEN,
                            format!("Fresh \u{2014} {ch_count} channel(s)"),
                        )
                    }
                    ConnectionHealth::Recovering => (
                        AMBER,
                        "Recovering \u{2014} stall just cleared".to_string(),
                    ),
                    ConnectionHealth::Stale => (
                        CORAL_RED,
                        format!(
                            "Stale \u{2014} no data for >{:.1} s",
                            self.config.staleness_threshold_secs
                        ),
                    ),
                    ConnectionHealth::SessionEnded => (
                        SKY_BLUE,
                        "Session ended \u{2014} controller signalled clean shutdown".to_string(),
                    ),
                    ConnectionHealth::ReceiverDead => (
                        ORCHID,
                        "Receiver dead \u{2014} socket error, restart the console".to_string(),
                    ),
                };

                ui.horizontal(|ui| {
                    ui.colored_label(dot_color, "\u{25cf}");
                    ui.label(label);
                });
            }

            // Missing-channel warnings
            if !self.missing_channels.is_empty() {
                ui.separator();
                ui.horizontal(|ui| {
                    ui.colored_label(AMBER, "\u{26a0}");
                    ui.label(format!(
                        "{} configured channel(s) not found in schema:",
                        self.missing_channels.len()
                    ));
                });
                for (panel_title, channel_name) in &self.missing_channels {
                    ui.label(format!(
                        "    panel \"{panel_title}\": channel \"{channel_name}\" not in schema"
                    ));
                }
            }

            // Status bar: dropped-frame counters.
            //
            // Sources of drops:
            //   1. Wire gaps (controller-side drops or network packet loss): `dropped_frames_on_wire`
            //   2. Receiver-thread backpressure drops (channel-full): `receiver_dropped_frames()`
            //   3. Schema-drift drops (Row channel count mismatches schema): `schema_drift_drops`
            //   4. Pre-schema pending-queue overflow: `pending_overflow_drops`
            {
                let wire_drops = self.dropped_frames_on_wire;
                let recv_drops = receiver_dropped_frames();
                let drift_drops = self.schema_drift_drops;
                let pending_drops = self.pending_overflow_drops;
                let any_drops = wire_drops > 0 || recv_drops > 0 || drift_drops > 0 || pending_drops > 0;
                let mut status = format!(
                    "Dropped frames \u{2014} wire gaps: {wire_drops}  receiver backpressure: {recv_drops}"
                );
                if drift_drops > 0 {
                    status.push_str(&format!("  drift:{drift_drops}"));
                }
                if pending_drops > 0 {
                    status.push_str(&format!("  preoverflow:{pending_drops}"));
                }
                // When drops are zero, keep the line readable in the theme's default text
                // colour — a grayed-out "everything fine" row is visually the same as a
                // disabled widget. When drops accumulate, color only the leading glyph so
                // the eye is drawn to it without sacrificing legibility of the number.
                if any_drops {
                    ui.horizontal(|ui| {
                        ui.colored_label(CORAL_RED, "\u{26a0}");
                        ui.label(status);
                    });
                } else {
                    ui.label(status);
                }
            }

            // Forensic log disabled indicator.
            if let Some(ref reason) = self.forensic_disabled_reason {
                ui.horizontal(|ui| {
                    ui.colored_label(AMBER, "\u{26a0}");
                    ui.label(format!("Forensic log disabled: {reason}"));
                });
            }

            ui.separator();

            // Render one plot per configured panel.
            for panel in &self.config.panels {
                ui.label(egui::RichText::new(&panel.title).strong());

                // Collect the y-axis unit label: use the first channel in this panel that
                // has a declared unit. If units conflict across channels, the first wins;
                // mixed-unit panels are a user-config error.
                let y_label: Option<String> = self.schema.as_ref().and_then(|schema| {
                    panel.channels.iter().find_map(|ch| {
                        schema
                            .name_to_index
                            .get(ch.as_str())
                            .and_then(|&idx| schema.channel_units.get(idx))
                            .and_then(|u| u.clone())
                    })
                });

                let mut plot = Plot::new(&panel.title)
                    .height(200.0)
                    .allow_scroll(false);

                if let Some(ref unit) = y_label {
                    plot = plot.y_axis_label(unit.clone());
                }

                // When frozen, pin the x-axis to the captured snapshot window so that
                // the view stays fixed even as new samples accumulate in the ring buffer.
                // `include_x` nudges egui_plot to ensure both endpoints are in view;
                // combined with `allow_scroll(false)` this holds the window in place.
                if self.frozen {
                    if let Some(xmax) = self.frozen_xmax {
                        let xmin = xmax - self.config.window_seconds;
                        plot = plot
                            .include_x(xmin)
                            .include_x(xmax)
                            .auto_bounds(egui::Vec2b::new(false, true));
                    }
                }

                plot.show(ui, |plot_ui| {
                    for ch in &panel.channels {
                        if let Some(buf) = self.channel_data.get(ch.as_str()) {
                            let points: Vec<[f64; 2]> =
                                buf.iter().map(|&(t, v)| [t, v]).collect();
                            let line = Line::new(PlotPoints::new(points)).name(ch.as_str());
                            plot_ui.line(line);
                        }
                    }
                });

                ui.add_space(4.0);
            }

            if self.config.panels.is_empty() {
                ui.label("No panels configured. Add [[panels]] entries to the config file.");
            }
        });

        // Fixed 30 Hz repaint cadence: schedule the next repaint ~33 ms from now.
        ctx.request_repaint_after(Duration::from_millis(33));
    }
}

/// Return an `HH:MM:SS.mmm` local-time prefix for stderr telemetry lines so they
/// can be correlated against the controller's ISO-timestamped tracing output.
fn wall_clock_prefix() -> String {
    chrono::Local::now().format("[%H:%M:%S%.3f]").to_string()
}

/// Derive a per-session forensic log filename from the user-configured base path.
///
/// Appends the current wall-clock Unix epoch in nanoseconds to the file stem so that
/// each viewer session produces a distinct file and prior sessions are never overwritten.
///
/// Example: `/data/logs/console-session.csv` → `/data/logs/console-session_1713530412345678900.csv`
///
/// The file rotation suffix scheme (`_1.csv`, `_2.csv`, …) applies on top of the session
/// stamp: if the stamped file grows past 64 MiB it rotates to
/// `/data/logs/console-session_1713530412345678900_1.csv`, etc.
fn session_stamped_path(base: &Path) -> PathBuf {
    let now_ns = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let stem = base
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let ext = base
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy()))
        .unwrap_or_default();
    let filename = format!("{stem}_{now_ns}{ext}");
    base.with_file_name(filename)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    /// Build a minimal `DeimosConsoleConfig` with a tight staleness threshold for testing.
    fn test_config(staleness_threshold_secs: f64) -> DeimosConsoleConfig {
        DeimosConsoleConfig {
            multicast_group: Ipv4Addr::new(239, 255, 0, 1),
            port: 5000,
            interface: None,
            window_seconds: 30.0,
            staleness_threshold_secs,
            tail_keep_secs: 0.5,
            recovery_settle_secs: 2.0,
            panels: Vec::new(),
            forensic_log_path: None,
        }
    }

    /// Create a `DeimosConsoleApp` with a disconnected receiver channel, suitable for unit tests
    /// that exercise state transitions without needing a real receiver thread.
    fn test_app(staleness_threshold_secs: f64) -> DeimosConsoleApp {
        let (_tx, rx) = crossbeam_channel::unbounded();
        DeimosConsoleApp::new(test_config(staleness_threshold_secs), rx)
    }

    /// Create a `DeimosConsoleApp` plus its live `Sender`, so tests can inject messages on the
    /// channel and then drive `drain_messages` on the app.
    fn test_app_with_tx(
        staleness_threshold_secs: f64,
    ) -> (
        DeimosConsoleApp,
        crossbeam_channel::Sender<ReportingMessage>,
    ) {
        let (tx, rx) = crossbeam_channel::unbounded();
        let app = DeimosConsoleApp::new(test_config(staleness_threshold_secs), rx);
        (app, tx)
    }

    /// Before any Schema is received, `connection_health` must return `NoSchemaYet`.
    #[test]
    fn health_no_schema_yet() {
        let app = test_app(2.0);
        assert_eq!(app.connection_health(), ConnectionHealth::NoSchemaYet);
    }

    /// After a Schema arrives but before any Row, `connection_health` must return `Stale`.
    #[test]
    fn health_stale_before_first_row() {
        let mut app = test_app(2.0);
        app.schema = Some(SchemaState::new(
            vec!["ch0".to_string()],
            vec![None],
            false,
            0,
        ));
        assert_eq!(app.connection_health(), ConnectionHealth::Stale);
    }

    /// A session-end `Schema` must surface as `SessionEnded` regardless of how recent
    /// the last `Row` was *or* whether a stall just fired, so an operator can immediately
    /// distinguish a clean shutdown from a silent stall or a recently-bumpy stream.
    #[test]
    fn health_session_ended_overrides_fresh() {
        let mut app = test_app(2.0);
        app.schema = Some(SchemaState::new(
            vec!["ch0".to_string()],
            vec![None],
            true,
            0,
        ));
        app.last_row_received_at = Some(Instant::now());
        // Also assert the Recovering state cannot mask a session-end signal.
        app.last_stall_at = Some(Instant::now());
        assert_eq!(app.connection_health(), ConnectionHealth::SessionEnded);
    }

    /// After a Schema and an immediate Row, `connection_health` must return `Fresh`, then
    /// transition to `Stale` once the staleness threshold has elapsed. With no stall ever
    /// recorded, the new `Recovering` state must NOT appear in either snapshot — it only
    /// activates after a real stall fires.
    #[test]
    fn health_fresh_then_stale_after_threshold() {
        // Use a tight threshold so the test completes quickly.
        let threshold_ms = 50u64;
        let mut app = test_app(threshold_ms as f64 / 1000.0);

        // Simulate Schema arrival.
        app.schema = Some(SchemaState::new(
            vec!["ch0".to_string()],
            vec![None],
            false,
            0,
        ));

        // Simulate Row arrival.
        app.last_row_received_at = Some(Instant::now());

        // No stall has ever fired, so `last_stall_at` stays None — the precedence rules
        // must skip Recovering and land on Fresh.
        assert_eq!(app.last_stall_at, None);
        assert_eq!(
            app.connection_health(),
            ConnectionHealth::Fresh,
            "expected Fresh immediately after Row arrival"
        );

        // Wait longer than the staleness threshold and assert the transition to Stale.
        // Stale must outrank Recovering, so even if a stall had been recorded the answer
        // would still be Stale once rows stop flowing.
        std::thread::sleep(Duration::from_millis(threshold_ms * 2));

        assert_eq!(
            app.connection_health(),
            ConnectionHealth::Stale,
            "expected Stale after staleness threshold elapsed"
        );
    }

    /// When the receiver thread exits, its `Sender` is dropped and the bounded channel
    /// reports `Disconnected`. The drain loop must flip the sticky `receiver_dead` flag
    /// so `connection_health` resolves to `ReceiverDead` instead of pretending the link
    /// is still alive.
    #[test]
    fn receiver_dead_flag_set_when_sender_drops() {
        let (mut app, tx) = test_app_with_tx(2.0);

        // Channel is connected and empty; drain leaves the flag clear.
        app.drain_messages();
        assert!(
            !app.receiver_dead,
            "receiver_dead must stay clear while the sender is alive"
        );
        assert_eq!(app.connection_health(), ConnectionHealth::NoSchemaYet);

        // Drop the sender to simulate the receiver thread exiting.
        drop(tx);

        app.drain_messages();
        assert!(
            app.receiver_dead,
            "receiver_dead must be set after the sender is dropped"
        );
        assert_eq!(
            app.connection_health(),
            ConnectionHealth::ReceiverDead,
            "ReceiverDead must override every other health state"
        );
    }

    /// A `Row` whose channel-value count disagrees with the active schema is silently
    /// dropped (the alternative — drawing it — would produce visually-wrong plots).
    /// `schema_drift_drops` counts these so an operator can see drift in the status bar.
    #[test]
    fn schema_drift_drops_row_with_mismatched_channel_count() {
        let (mut app, tx) = test_app_with_tx(2.0);

        // Send a Schema declaring two channels.
        tx.send(ReportingMessage::Schema {
            channel_names: vec!["a".to_string(), "b".to_string()],
            channel_units: vec![None, None],
            monotonic_epoch_ns: 1,
            is_session_end: false,
        })
        .expect("send Schema");

        // Send a Row carrying three values — schema drift.
        tx.send(ReportingMessage::Row {
            seq: 0,
            timestamp: 0.0,
            system_time: "2026-04-26T00:00:00Z".to_string(),
            values: vec![1.0, 2.0, 3.0],
        })
        .expect("send Row");

        app.drain_messages();

        assert_eq!(
            app.schema_drift_drops, 1,
            "schema-drift drop counter must increment for a mismatched-arity Row"
        );
        // The drop must not pollute the channel ring buffers.
        assert!(
            app.channel_data.is_empty() || app.channel_data.values().all(|buf| buf.is_empty()),
            "channel ring buffers must remain empty after a schema-drift drop"
        );
    }

    /// Format a `SystemTime` as an RFC3339 nanosecond string the way the controller serializes
    /// `system_time` on the wire. Matches `process_row`'s parser exactly so the lag computation
    /// reproduces what production code sees.
    fn rfc3339_nanos(t: SystemTime) -> String {
        let dt: chrono::DateTime<chrono::Utc> = t.into();
        dt.to_rfc3339_opts(chrono::SecondsFormat::Nanos, true)
    }

    /// A drained batch that mixes a stale backlog with one live row must trigger exactly one
    /// stall event, clear pre-stall samples down to the `tail_keep_secs` cutoff, and leave a
    /// NaN sentinel marking the discontinuity.
    ///
    /// The test simulates the realistic flow: a first drain primes the per-channel ring
    /// buffer with two pre-stall samples, then a second drain delivers a backlog of rows with
    /// stale `system_time` (older than `staleness_threshold_secs`) plus one live row. The
    /// data-staleness signal should fire on the second drain, the stall handler should evict
    /// the pre-stall samples that fall below `freshest_ts - tail_keep_secs`, and the NaN
    /// sentinel should be inserted as the visible discontinuity.
    #[test]
    fn stall_clears_pre_stall_samples_and_emits_sentinel() {
        let (mut app, tx) = test_app_with_tx(0.5);
        // Use a tail that is small enough relative to the controller-timestamp gap below that
        // every pre-stall sample will fall below the cutoff and be evicted.
        app.config.tail_keep_secs = 0.1;
        let now = SystemTime::now();

        // --- Drain #1: prime channel_data with two "pre-stall" samples. ---
        // The system_time is fresh so this drain is treated as live (no stall fires).
        tx.send(ReportingMessage::Schema {
            channel_names: vec!["a".to_string()],
            channel_units: vec![None],
            monotonic_epoch_ns: 1,
            is_session_end: false,
        })
        .expect("send Schema");
        for (seq, ts) in [(0u64, 0.0_f64), (1, 0.05)] {
            tx.send(ReportingMessage::Row {
                seq,
                timestamp: ts,
                system_time: rfc3339_nanos(now),
                values: vec![ts],
            })
            .expect("send pre-stall Row");
        }
        app.drain_messages();
        assert_eq!(
            app.stalls_detected, 0,
            "first drain with fresh data must not trigger a stall"
        );
        let buf = app
            .channel_data
            .get("a")
            .expect("channel a must be populated after the priming drain");
        assert_eq!(buf.len(), 2, "two pre-stall samples should be present");

        // --- Drain #2: deliver a backlog of stale rows + one live row. ---
        // Stale rows carry a system_time older than staleness_threshold_secs, so the
        // data-staleness signal fires regardless of UI-pause timing.
        let stale_st = rfc3339_nanos(now - Duration::from_secs(10));
        let live_st = rfc3339_nanos(SystemTime::now());
        for (seq, ts) in [(2u64, 1.0_f64), (3, 1.5), (4, 2.0)] {
            tx.send(ReportingMessage::Row {
                seq,
                timestamp: ts,
                system_time: stale_st.clone(),
                values: vec![ts],
            })
            .expect("send stale backlog Row");
        }
        let live_ts = 5.0_f64;
        tx.send(ReportingMessage::Row {
            seq: 5,
            timestamp: live_ts,
            system_time: live_st,
            values: vec![live_ts],
        })
        .expect("send live Row");

        app.drain_messages();

        assert_eq!(
            app.stalls_detected, 1,
            "data-staleness signal should fire exactly once on the catch-up drain"
        );
        assert!(
            app.stale_rows_evicted >= 2,
            "both pre-stall priming samples should have been evicted (got {})",
            app.stale_rows_evicted
        );

        let buf = app
            .channel_data
            .get("a")
            .expect("channel a must still exist after the stall");
        // The two priming samples at t={0.0, 0.05} and the stale catch-up rows at
        // t={1.0, 1.5, 2.0} were all below `freshest_ts - tail_keep_secs` (= 4.9) at
        // apply-pending-stall time, so they must have been evicted from the visible buffer.
        assert!(
            !buf.iter().any(|&(t, _)| t < 4.9),
            "all samples below the tail-keep cutoff (4.9) must be evicted, got {:?}",
            buf.iter().collect::<Vec<_>>()
        );
        assert!(
            buf.iter().any(|&(_, v)| v.is_nan()),
            "the discontinuity sentinel (NaN) must be present in the buffer"
        );
        // The live row must still be present so the operator sees the post-stall trace.
        assert!(
            buf.iter()
                .any(|&(t, v)| !v.is_nan() && (t - live_ts).abs() < 1e-9),
            "the live row at t={live_ts} must remain after stall handling"
        );

        // Sentinel-position invariant (regression for "stale catch-up rows still rendered"):
        // egui_plot draws points in buffer-insertion order, so any finite point inserted AFTER
        // the sentinel that has a smaller timestamp would render the line back through stale
        // data. Assert that no finite point in the buffer satisfies
        // `t < sentinel_ts && index > sentinel_index`.
        let sentinel_index = buf
            .iter()
            .position(|&(_, v)| v.is_nan())
            .expect("sentinel must exist by the assertion above");
        let sentinel_ts = buf[sentinel_index].0;
        for (i, &(t, v)) in buf.iter().enumerate() {
            if v.is_nan() {
                continue;
            }
            if i > sentinel_index {
                assert!(
                    t >= sentinel_ts,
                    "finite point at index {i} (t={t}) sits after the sentinel (ts={sentinel_ts}) but has an older timestamp; egui_plot would render the line back through stale data"
                );
            }
        }
    }

    /// A stall that fired within `recovery_settle_secs` must surface as `Recovering`, even
    /// while rows are otherwise fresh. Once the settle window has elapsed, the same state
    /// must collapse back to `Fresh`. The test ages `last_stall_at` by subtracting from
    /// `Instant::now()` so the assertion is deterministic without sleeping.
    #[test]
    fn health_recovering_then_fresh_after_settle_window() {
        let settle_secs = 2.0_f64;
        let mut app = test_app(60.0);
        app.config.recovery_settle_secs = settle_secs;

        // Schema received and a Row just arrived — without the stall mark this would be Fresh.
        app.schema = Some(SchemaState::new(
            vec!["ch0".to_string()],
            vec![None],
            false,
            0,
        ));
        app.last_row_received_at = Some(Instant::now());
        assert_eq!(
            app.connection_health(),
            ConnectionHealth::Fresh,
            "without a recorded stall the baseline must be Fresh"
        );

        // Drive a stall: set `last_stall_at = now`. Indicator must flip to Recovering.
        app.last_stall_at = Some(Instant::now());
        assert_eq!(
            app.connection_health(),
            ConnectionHealth::Recovering,
            "Recovering must surface immediately after a stall is recorded"
        );

        // Refresh the row mark too (so the row is still fresh) and age `last_stall_at`
        // past the settle window. Indicator must collapse back to Fresh without sleeping.
        app.last_row_received_at = Some(Instant::now());
        let aged = Instant::now()
            .checked_sub(Duration::from_secs_f64(settle_secs * 2.0))
            .expect("aging Instant by 2x settle window must not underflow");
        app.last_stall_at = Some(aged);
        assert_eq!(
            app.connection_health(),
            ConnectionHealth::Fresh,
            "Recovering must collapse to Fresh once the settle window has elapsed"
        );
    }

    /// A contiguous live stream — fresh `system_time` on every row, frequent drains — must
    /// never flag a stall. The drain-time wall-clock check needs `last_drain_at` to be older
    /// than `staleness_threshold_secs`, and the data-staleness check needs row lag above the
    /// same threshold; neither holds for a healthy stream.
    #[test]
    fn contiguous_live_stream_does_not_trigger_stall() {
        let (mut app, tx) = test_app_with_tx(0.5);

        tx.send(ReportingMessage::Schema {
            channel_names: vec!["a".to_string()],
            channel_units: vec![None],
            monotonic_epoch_ns: 1,
            is_session_end: false,
        })
        .expect("send Schema");
        app.drain_messages();
        assert_eq!(
            app.stalls_detected, 0,
            "schema-only drain must not flag a stall"
        );

        // Drive five contiguous drains, each with a freshly-stamped row, with a tiny pause
        // between drains so `last_drain_at` advances naturally but stays well under the
        // 0.5 s threshold.
        for seq in 0u64..5 {
            tx.send(ReportingMessage::Row {
                seq,
                timestamp: seq as f64 * 0.01,
                system_time: rfc3339_nanos(SystemTime::now()),
                values: vec![seq as f64],
            })
            .expect("send live Row");
            app.drain_messages();
            std::thread::sleep(Duration::from_millis(10));
        }

        assert_eq!(
            app.stalls_detected, 0,
            "a contiguous live stream must never trigger stall detection"
        );
        assert_eq!(
            app.stale_rows_evicted, 0,
            "no rows should be evicted by stall handling on a healthy stream"
        );
    }

    /// Empty drains must NOT advance `last_drain_at`; otherwise a paused recv thread (e.g.
    /// SIGSTOP, or just a starved scheduler) is masked. While the recv thread is paused the
    /// UI tick can still fire and produce a series of empty drains. If each empty drain
    /// bumped `last_drain_at` to "now", the next non-empty drain (after resume) would see
    /// `elapsed() < threshold` and skip stall detection, so the operator would never get the
    /// discontinuity sentinel they need.
    ///
    /// Reproduces the bug observed under the 5 kHz / 20 s SIGSTOP experiment, where the
    /// first post-resume `update()` tick fired before the recv thread had pushed rows into
    /// the channel; that empty drain reset `last_drain_at`, and the subsequent non-empty
    /// drain reported `stalls_detected == 0`.
    #[test]
    fn empty_drain_does_not_advance_last_drain_at() {
        let staleness_threshold_secs = 0.5_f64;
        let (mut app, tx) = test_app_with_tx(staleness_threshold_secs);

        // Prime with Schema + one row so `last_drain_at` is set by a non-empty drain.
        tx.send(ReportingMessage::Schema {
            channel_names: vec!["a".to_string()],
            channel_units: vec![None],
            monotonic_epoch_ns: 1,
            is_session_end: false,
        })
        .expect("send Schema");
        tx.send(ReportingMessage::Row {
            seq: 0,
            timestamp: 0.0,
            system_time: rfc3339_nanos(SystemTime::now()),
            values: vec![0.0],
        })
        .expect("send priming Row");
        app.drain_messages();
        let baseline = app
            .last_drain_at
            .expect("priming drain must initialize last_drain_at");

        // Several empty drains: `last_drain_at` must not advance past the baseline.
        for _ in 0..5 {
            app.drain_messages();
            assert_eq!(
                app.last_drain_at,
                Some(baseline),
                "empty drains must not advance last_drain_at"
            );
        }

        // Simulate a long pause by aging `last_drain_at` past the staleness threshold. Use
        // `Instant::now().checked_sub(...)` so the test is deterministic and doesn't sleep.
        // (Same trick as `health_recovering_then_fresh_after_settle_window`.)
        let aged = Instant::now()
            .checked_sub(Duration::from_secs_f64(staleness_threshold_secs * 2.0))
            .expect("aging Instant by 2x threshold must not underflow");
        app.last_drain_at = Some(aged);

        // A fresh row arrives and is drained. The aged `last_drain_at` triggers the
        // UI-pause signal in `detect_stall`, so `stalls_detected` must increment.
        // Pre-fix, intervening empty drains would have bumped `last_drain_at` to "now",
        // masking the pause.
        tx.send(ReportingMessage::Row {
            seq: 1,
            timestamp: 1.0,
            system_time: rfc3339_nanos(SystemTime::now()),
            values: vec![1.0],
        })
        .expect("send post-pause Row");
        app.drain_messages();

        assert_eq!(
            app.stalls_detected, 1,
            "post-pause non-empty drain must detect the stall masked by empty-drain advancement"
        );
    }

    /// Apply a stall while frozen: the operator paused specifically to inspect history, so
    /// `apply_stall` MUST NOT mutate the per-channel ring buffers (no eviction, no NaN
    /// sentinel). The stall counter and `last_stall_at` MUST still advance so the per-tick
    /// telemetry is accurate and the connection-health indicator surfaces `Recovering`. The
    /// pending-stall slot MUST be populated so the deferred sentinel can materialize on the
    /// first non-frozen drain after unfreeze (see
    /// `frozen_stall_emits_deferred_sentinel_on_unfreeze` for that half).
    #[test]
    fn stall_while_frozen_preserves_buffers_but_records_event() {
        let mut app = test_app(2.0);
        app.config.tail_keep_secs = 0.5;

        // Seed channel "a" with a few historical samples and engage freeze.
        let mut buf = VecDeque::new();
        for (t, v) in [(0.0, 1.0_f64), (1.0, 2.0), (2.0, 3.0)] {
            buf.push_back((t, v));
        }
        app.channel_data.insert("a".to_string(), buf);
        app.frozen = true;
        app.frozen_xmax = Some(2.0);

        let snapshot: Vec<(f64, f64)> = app
            .channel_data
            .get("a")
            .expect("channel a was just inserted")
            .iter()
            .copied()
            .collect();

        // freshest_ts sits well past frozen_xmax; pre-fix this would evict every historical
        // sample (cutoff = 60.0 - 0.5 = 59.5, so every t < 59.5 would be popped).
        app.apply_stall(StallEvent {
            trigger: StallTrigger::DataStaleness,
            freshest_ts: 60.0,
        });

        assert_eq!(
            app.stalls_detected, 1,
            "stalls_detected must increment even while frozen"
        );
        assert!(
            app.last_stall_at.is_some(),
            "last_stall_at must be set so unfreeze surfaces Recovering"
        );
        assert_eq!(
            app.stale_rows_evicted, 0,
            "no rows may be evicted while frozen"
        );
        assert!(
            app.pending_stall.is_some(),
            "frozen stall must record a pending sentinel so unfreeze can emit it"
        );

        let after: Vec<(f64, f64)> = app
            .channel_data
            .get("a")
            .expect("channel a must still exist after a frozen stall")
            .iter()
            .copied()
            .collect();
        assert_eq!(
            after, snapshot,
            "frozen channel buffer must be byte-identical after apply_stall: no eviction, no NaN sentinel (the sentinel is recorded as pending, not pushed)"
        );
    }

    /// A stall that fires while the view is frozen MUST eventually surface its discontinuity
    /// sentinel — the spec requires the visible discontinuity, with no freeze exemption. The
    /// sentinel is deferred at apply time and materialized on the first non-frozen drain
    /// after unfreeze (also serves as a regression for the buffer-mutation skip that hid the
    /// stall entirely pre-fix).
    #[test]
    fn frozen_stall_emits_deferred_sentinel_on_unfreeze() {
        let (mut app, tx) = test_app_with_tx(0.5);
        app.config.tail_keep_secs = 0.5;
        app.schema = Some(SchemaState::new(
            vec!["a".to_string()],
            vec![None],
            false,
            0,
        ));

        // Seed channel "a" with a few historical samples that all sit above the eventual
        // tail-keep cutoff (so they survive eviction) and engage freeze.
        let mut buf = VecDeque::new();
        for (t, v) in [(9.6_f64, 96.0_f64), (9.7, 97.0), (9.8, 98.0)] {
            buf.push_back((t, v));
        }
        app.channel_data.insert("a".to_string(), buf);
        app.frozen = true;
        app.frozen_xmax = Some(9.8);

        // Stall fires while frozen — pending_stall is recorded, channel buffer untouched.
        app.apply_stall(StallEvent {
            trigger: StallTrigger::DataStaleness,
            freshest_ts: 10.0,
        });
        assert!(
            app.pending_stall.is_some(),
            "pending_stall must be set after a frozen stall"
        );
        assert!(
            app.channel_data
                .get("a")
                .expect("channel a present")
                .iter()
                .all(|&(_, v)| !v.is_nan()),
            "no sentinel may be inserted into the channel buffer while frozen"
        );

        // Operator unfreezes and a fresh row arrives. The drain runs `apply_pending_stall`
        // after the second pass, materializing the sentinel.
        app.frozen = false;
        app.frozen_xmax = None;
        tx.send(ReportingMessage::Row {
            seq: 0,
            timestamp: 10.05,
            system_time: rfc3339_nanos(SystemTime::now()),
            values: vec![10.05],
        })
        .expect("send post-unfreeze Row");
        app.drain_messages();

        assert!(
            app.pending_stall.is_none(),
            "pending_stall must be cleared once the deferred sentinel has been applied"
        );
        let buf = app
            .channel_data
            .get("a")
            .expect("channel a must still exist after unfreeze drain");
        assert!(
            buf.iter().any(|&(_, v)| v.is_nan()),
            "the deferred sentinel must materialize on the first non-frozen drain"
        );
        // Sentinel position invariant: every finite point inserted after the sentinel must
        // have `t >= sentinel_ts`. (Without this, the rendered line draws back through stale
        // data — see `stall_clears_pre_stall_samples_and_emits_sentinel`.)
        let sentinel_index = buf
            .iter()
            .position(|&(_, v)| v.is_nan())
            .expect("sentinel exists by previous assertion");
        let sentinel_ts = buf[sentinel_index].0;
        for (i, &(t, v)) in buf.iter().enumerate() {
            if !v.is_nan() && i > sentinel_index {
                assert!(
                    t >= sentinel_ts,
                    "finite point at index {i} (t={t}) sits after the sentinel (ts={sentinel_ts}) but has an older timestamp"
                );
            }
        }
    }

    /// The per-second tick line MUST advertise every counter the spec lists, because
    /// downstream log parsers (and the operators reading them) key off these field names. A
    /// rename or accidental drop would silently break the contract — this test pins the key
    /// set without depending on stderr capture.
    #[test]
    fn tick_line_includes_all_required_keys() {
        let app = test_app(2.0);
        let line = app.format_tick_line(42, Duration::from_secs(1), None, 3);

        for key in [
            "rows=",
            "rate=",
            "last_seq=",
            "wire_drops=",
            "recv_drops=",
            "overwritten_frames=",
            "stalls_detected=",
            "stale_rows_evicted=",
            "pending=",
        ] {
            assert!(
                line.contains(key),
                "tick line must include `{key}` field; got: {line}"
            );
        }
    }
}
