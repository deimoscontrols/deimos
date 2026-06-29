use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use eframe::egui;
use egui_plot::{Line, Plot, PlotPoints};

use deimos::dispatcher::ReportingMessage;

use crate::config::DeimosConsoleConfig;
use crate::forensic::ForensicLog;
use crate::receiver::ReceiverCounters;

/// Buffer-processor thread cadence. 125 Hz (8 ms) keeps data-to-display lag below human
/// perception while leaving headroom over a 60 Hz repaint cadence: each repaint at ~16 ms
/// picks up at most two processor ticks of accumulated work. Fixed in code, not config.
const PROCESSOR_TICK_INTERVAL: Duration = Duration::from_millis(8);

// Muted dot-glyph palette: egui's saturated `YELLOW`/`GREEN`/`RED` are hard to read on either
// theme. Reserved for indicator dots only; text uses the theme foreground.
const MINT_GREEN: egui::Color32 = egui::Color32::from_rgb(110, 200, 140);
const AMBER: egui::Color32 = egui::Color32::from_rgb(230, 180, 60);
const CORAL_RED: egui::Color32 = egui::Color32::from_rgb(220, 100, 100);
const SKY_BLUE: egui::Color32 = egui::Color32::from_rgb(120, 180, 240);
const ORCHID: egui::Color32 = egui::Color32::from_rgb(190, 120, 220);

/// Six-state connection health indicator. Computed each UI frame from the current schema, the
/// wall-clock time of the last `Row` and the most recent stall, and whether the receiver thread
/// is alive.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ConnectionHealth {
    /// No `Schema` received yet; liveness unknown.
    NoSchemaYet,
    /// A `Row` arrived within the staleness threshold.
    Fresh,
    /// A stall fired within `recovery_settle_secs` and rows are flowing again. Sticky for the
    /// settle window so the operator notices the discontinuity on a brief glance.
    Recovering,
    /// `Schema` exists but no `Row` has arrived (or none within `staleness_threshold`).
    Stale,
    /// Controller emitted a session-end `Schema` — a clean shutdown, distinct from `Stale`.
    SessionEnded,
    /// Receiver thread exited on a non-transient socket error. Restart required.
    ReceiverDead,
}

/// Pre-schema buffer cap; drop-oldest preserves the most recent N rows.
const MAX_PENDING_ROWS: usize = 1000;

/// Why a stall fired. Currently one variant; the enum shape is retained so the stall log line
/// keeps an explicit name via `Debug` and so a future second signal slots in without re-adding
/// the type.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum StallTrigger {
    /// A drained row's `viewer_received_at - controller_system_time` lag exceeded
    /// `staleness_threshold_secs` — typically the bounded channel was at capacity and the
    /// drop-oldest policy is flushing a catch-up tail.
    DataStaleness,
}

/// Outcome of [`ConsoleState::detect_stall`]: the trigger plus the freshest controller
/// timestamp in the triggering batch (used as eviction cutoff and sentinel anchor).
#[derive(Clone, Copy, Debug)]
pub struct StallEvent {
    pub trigger: StallTrigger,
    pub freshest_ts: f64,
}

/// A stall whose buffer-mutation step (eviction + sentinel push) is deferred until after the
/// catch-up batch's rows have been appended. Two reasons to defer:
///
/// 1. `egui_plot` renders in insertion order, not time order. A sentinel pushed before catch-up
///    rows with `timestamp < sentinel_ts` would draw a line backward through the stale data.
/// 2. While frozen, we must not mutate the buffers, but the sentinel still has to land once
///    the operator unfreezes. Recording the pending stall lets the next non-frozen drain do it.
#[derive(Clone, Copy, Debug)]
struct PendingStall {
    /// `freshest_ts - 1e-9`; x-coordinate where the NaN sentinel will be pushed.
    sentinel_ts: f64,
    /// `freshest_ts` from the triggering batch. The eviction cutoff is `freshest_ts -
    /// tail_keep_secs`, keeping a small pre-stall tail and dropping everything older.
    freshest_ts: f64,
}

/// While frozen, the live-window eviction would discard the history the operator paused to
/// inspect, but unbounded growth lets a long freeze OOM the viewer. Cap per-channel buffers
/// at this multiple of `window_seconds`.
const FREEZE_BUFFER_WINDOW_MULTIPLE: f64 = 10.0;

/// Current session's channel layout. Populated on first `Schema`; replaced on any non-session-end
/// `Schema` re-emission.
pub struct SchemaState {
    pub channel_names: Vec<String>,
    /// `None` means unknown or not applicable; used for plot y-axis labels.
    pub channel_units: Vec<Option<String>>,
    pub name_to_index: HashMap<String, usize>,
    pub session_end_received: bool,
    /// Wall-clock anchor reported by the controller. A change between successive Schemas means
    /// the controller restarted, triggering gap-tracking reset. Also logged so a viewer-side
    /// session can be correlated with the controller run.
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

/// Main application state. Held behind an `Arc<Mutex<…>>` by [`DeimosConsoleApp`]: the buffer-
/// processor thread mutates it via [`Self::background_tick`] on a fixed cadence and the UI thread
/// reaches it briefly via [`Self::render_ui`] to snapshot per-panel data before [`Plot::show`].
pub struct ConsoleState {
    config: DeimosConsoleConfig,
    /// Incoming messages from the receiver thread. Drained on the buffer-processor thread.
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
    /// Stall events fired by [`ConsoleState::detect_stall`]; surfaced in per-tick telemetry.
    stalls_detected: u64,
    /// Pre-stall samples popped across all stall events. The visual "stale segment" the operator
    /// did not see live; the forensic log retains it.
    stale_rows_evicted: u64,
    /// Most-recent stall instant. Drives the `Recovering` indicator: sticky while
    /// `last_stall_at.elapsed() <= recovery_settle_secs`.
    last_stall_at: Option<Instant>,
    /// Stall whose eviction + sentinel push is deferred until the catch-up batch lands (or, if
    /// frozen, until the next non-frozen drain). See [`PendingStall`].
    pending_stall: Option<PendingStall>,
    /// Receiver-thread counters (drop-oldest evictions + defensive enqueue failures).
    counters: ReceiverCounters,
}

impl ConsoleState {
    fn new(
        config: DeimosConsoleConfig,
        rx: crossbeam_channel::Receiver<ReportingMessage>,
        counters: ReceiverCounters,
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
            stalls_detected: 0,
            stale_rows_evicted: 0,
            last_stall_at: None,
            pending_stall: None,
            counters,
        }
    }

    /// Emit a per-second telemetry line to stderr (row rate, rx-lag stats, drop/stall counters).
    /// After three consecutive zero-row ticks, suppress to one heartbeat per minute. Fields are
    /// rendered as `key=value`; see the README for the full key list.
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
            "deimos-console: tick rows={rows} rate={rate:.1}Hz last_seq={last_seq} wire_drops={} recv_drops={} overwritten_frames={} schema_drift_drops={} pending_overflow_drops={} stalls_detected={} stale_rows_evicted={} pending={}{lag_segment}{idle_suffix}",
            self.dropped_frames_on_wire,
            self.counters.dropped.load(Ordering::Relaxed),
            self.counters.overwritten.load(Ordering::Relaxed),
            self.schema_drift_drops,
            self.pending_overflow_drops,
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

        if let Some(log) = &mut self.forensic_log
            && let Err(e) = log.write_row(viewer_received_at, seq, timestamp, system_time, &values)
        {
            let reason = format!("write failed: {e}");
            eprintln!(
                "{} deimos-console: forensic log {reason}; disabling log",
                wall_clock_prefix()
            );
            self.forensic_disabled_reason = Some(reason);
            self.forensic_log = None;
        }

        self.rows_since_last_tick += 1;

        // rx-lag = viewer-receipt wall clock minus the controller's cycle-start wall clock
        // (`system_time` is `SystemTime::now()` from the top of the cycle). Answers "how old is
        // what the operator sees", not "how fast is the network". `epoch_ns + row.timestamp`
        // was tried as the anchor but `epoch_ns` is sampled at `init` and `row.timestamp`
        // counts from start-of-Operating, leaving a constant false offset.
        if let (Ok(rx_ns), Ok(produced)) = (
            viewer_received_at
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_nanos() as i128),
            chrono::DateTime::parse_from_rfc3339(system_time),
        ) {
            // `timestamp_nanos_opt` returns `None` outside ~1678-2262; skip rather than
            // substitute 1970, which would pollute lag_min/max with a spurious 56-year value.
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

    /// Inspect a freshly-drained batch of `Row` metadata and decide whether the data path
    /// stalled. Fires when any drained row's lag
    /// `viewer_received_at - controller_system_time` exceeds the staleness threshold — i.e.
    /// the receiver fed us pre-stall rows from the bounded channel.
    ///
    /// Returns `None` if the batch is empty (no rows means no `freshest_ts` to anchor a clear)
    /// or no row's lag exceeded the threshold. A repaint gap with fresh data behind it is not
    /// a stall — buffer maintenance runs on the processor thread independently of the repaint
    /// cycle, so the UI just hadn't drawn yet.
    fn detect_stall(
        &mut self,
        viewer_received_at: SystemTime,
        drained_rows: &[(u64, f64, &str)],
    ) -> Option<StallEvent> {
        if drained_rows.is_empty() {
            return None;
        }

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

        if !data_stale {
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

        Some(StallEvent {
            trigger: StallTrigger::DataStaleness,
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

        // Catch-up rows were appended to the back, so `retain` scans every entry rather than
        // `pop_front`-walking. egui_plot draws in insertion order; the sentinel must precede
        // the freshest post-stall sample to render a line break.
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
    /// Called from [`Self::background_tick`] on the buffer-processor thread every
    /// [`PROCESSOR_TICK_INTERVAL`] (8 ms / 125 Hz), independent of `eframe::App::update` and
    /// the repaint cadence. The bounded receiver channel decouples the network thread from
    /// the rest of the viewer; this method is its sink. If `try_recv` reports the channel is
    /// disconnected (the receiver thread has exited on a non-transient socket error), the
    /// sticky `receiver_dead` flag is set so the status bar can render a distinct state
    /// instead of pretending the link is still alive.
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
    /// Kept as a pub(crate) method (rather than inlined into `background_tick`) so unit tests
    /// can exercise the message-handling logic directly without driving the processor thread.
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
                    // Epoch change (or no schema yet) means a new session: reset gap tracking
                    // and clear stale channel data so we don't draw across session boundaries.
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

                        // Fresh forensic log; the session stamp from `session_stamped_path`
                        // keeps prior sessions intact across viewer restarts.
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

                    self.reconcile_panels();

                    // Drain rows that arrived before the schema. The original receipt time was
                    // not recorded pre-schema, so `SystemTime::now()` substitutes.
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
                        // Buffer until Schema; preserve `system_time` so the forensic log can
                        // use it on drain.
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

        // After the second pass has appended catch-up rows; see [`Self::apply_pending_stall`].
        self.apply_pending_stall();
    }

    /// Current connection health. Precedence high → low: `ReceiverDead`, `NoSchemaYet`,
    /// `SessionEnded`, `Stale`, `Recovering`, `Fresh`. Stale outranks Recovering because
    /// "no data flowing" is more urgent than "data flowing but recently bumpy".
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

    /// One cadence step of buffer maintenance, run by the buffer-processor thread.
    ///
    /// Calls — in order — `drain_messages` (channel pull, stall detection, ring-buffer mutation,
    /// forensic log writes), `maybe_log_health_transition` (stderr line on state change), and
    /// `maybe_emit_tick` (per-second telemetry). Each runs under the shared `Mutex` held by the
    /// processor closure; `render_ui` takes the same lock from the UI thread.
    ///
    /// Independent of `eframe::App::update`: this runs every [`PROCESSOR_TICK_INTERVAL`] regardless
    /// of whether the host window manager is currently driving repaints. That is the whole point
    /// of the decoupling — buffer freshness no longer depends on the window being visible.
    pub(crate) fn background_tick(&mut self) {
        self.drain_messages();
        self.maybe_log_health_transition();
        self.maybe_emit_tick();
    }

    /// Render the operator console UI into `ctx`.
    ///
    /// Manages the shared `Mutex` itself rather than running entirely under it: takes a brief
    /// lock to handle UI-side state mutation (the freeze button) and to build a [`RenderSnapshot`]
    /// of everything the plots need, then drops the lock before [`Plot::show`] is called for each
    /// panel. Plot rendering can dominate frame time when many panels are configured; releasing
    /// the lock around it lets the buffer-processor thread keep draining the receiver channel
    /// instead of blocking on a multi-millisecond render.
    ///
    /// Pre-render buffer maintenance (drain, health log, telemetry tick) lives on the processor
    /// thread — see [`Self::background_tick`]. The freeze/unfreeze button is the one UI-side path
    /// that still mutates the per-channel ring buffers (it trims to the live window on unfreeze);
    /// everything else this method touches is UI-only state.
    pub(crate) fn render_ui(state_arc: &Arc<Mutex<Self>>, ui: &mut egui::Ui) {
        egui::CentralPanel::default().show_inside(ui, |ui| {
            // Phase 1: take the lock just long enough to render the always-fast top section
            // (header + freeze button + status indicators) and to copy out the per-panel plot
            // data. The freeze button must run under the lock because clicking it mutates ring
            // buffers; the status rows are read-only but cheap, so doing them here keeps the
            // lock-acquire count down to one per frame.
            let snapshot = {
                let mut state = state_arc
                    .lock()
                    .expect("deimos-console state mutex poisoned");
                state.render_top_section(ui);
                state.build_render_snapshot()
            };
            // Lock released here. Plot::show below runs without holding it.

            // Phase 2: render the per-panel plots from the snapshot. No state access needed.
            Self::render_panels(ui, &snapshot);
        });

        // Fixed 30 Hz repaint cadence: schedule the next repaint ~33 ms from now.
        ui.ctx().request_repaint_after(Duration::from_millis(33));
    }

    /// Render the always-cheap top section: header bar with the freeze button, the
    /// connection-health indicator, missing-channel warnings, the dropped-frames status line,
    /// and the forensic-disabled indicator. Runs under the shared lock — the freeze button
    /// mutates ring buffers and the health indicator reads transient state.
    fn render_top_section(&mut self, ui: &mut egui::Ui) {
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

        // Connection-health indicator: colored \u25cf dot + short label.
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
                    let ch_count = self.schema.as_ref().map_or(0, |s| s.channel_names.len());
                    (MINT_GREEN, format!("Fresh \u{2014} {ch_count} channel(s)"))
                }
                ConnectionHealth::Recovering => {
                    (AMBER, "Recovering \u{2014} stall just cleared".to_string())
                }
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

        // Status bar: dropped-frame counters (wire gaps, receiver backpressure, schema drift,
        // pre-schema pending-queue overflow).
        {
            let wire_drops = self.dropped_frames_on_wire;
            let recv_drops = self.counters.dropped.load(Ordering::Relaxed);
            let drift_drops = self.schema_drift_drops;
            let pending_drops = self.pending_overflow_drops;
            let any_drops =
                wire_drops > 0 || recv_drops > 0 || drift_drops > 0 || pending_drops > 0;
            let mut status = format!(
                "Dropped frames \u{2014} wire gaps: {wire_drops}  receiver backpressure: {recv_drops}"
            );
            if drift_drops > 0 {
                status.push_str(&format!("  drift:{drift_drops}"));
            }
            if pending_drops > 0 {
                status.push_str(&format!("  preoverflow:{pending_drops}"));
            }
            // Color only the leading glyph when drops accumulate; the theme default for the
            // number itself keeps it legible.
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
    }

    /// Snapshot per-panel plot inputs so the shared lock can be released before [`Plot::show`].
    /// `egui_plot` consumes points by value via [`PlotPoints`], so the clone is unavoidable —
    /// this just shifts it under the lock instead of inside the long render closure.
    fn build_render_snapshot(&self) -> RenderSnapshot {
        let panels = self
            .config
            .panels
            .iter()
            .map(|panel| {
                // First declared unit in the panel wins; mixed-unit panels are warned at
                // `reconcile_panels` time.
                let y_label = self.schema.as_ref().and_then(|schema| {
                    panel.channels.iter().find_map(|ch| {
                        schema
                            .name_to_index
                            .get(ch.as_str())
                            .and_then(|&idx| schema.channel_units.get(idx))
                            .and_then(|u| u.clone())
                    })
                });
                let series = panel
                    .channels
                    .iter()
                    .filter_map(|ch| {
                        self.channel_data.get(ch.as_str()).map(|buf| {
                            let points: Vec<[f64; 2]> = buf.iter().map(|&(t, v)| [t, v]).collect();
                            (ch.clone(), points)
                        })
                    })
                    .collect();
                PanelSnapshot {
                    title: panel.title.clone(),
                    y_label,
                    series,
                }
            })
            .collect();

        RenderSnapshot {
            frozen: self.frozen,
            frozen_xmax: self.frozen_xmax,
            window_seconds: self.config.window_seconds,
            columns: self.config.columns.max(1),
            panels,
        }
    }

    /// Render the per-panel plots from a [`RenderSnapshot`]. Runs without the shared lock so a
    /// slow [`Plot::show`] on a heavily-populated panel cannot stall the buffer-processor thread.
    fn render_panels(ui: &mut egui::Ui, snapshot: &RenderSnapshot) {
        let columns = snapshot.columns.max(1);
        let gap = 8.0;
        let total_gap = gap * (columns.saturating_sub(1) as f32);
        let panel_width = ((ui.available_width() - total_gap) / columns as f32).max(180.0);
        egui::Grid::new("deimos_console_panel_grid")
            .num_columns(columns)
            .spacing(egui::vec2(gap, 8.0))
            .show(ui, |ui| {
                for (idx, panel) in snapshot.panels.iter().enumerate() {
                    Self::render_panel(ui, snapshot, panel, panel_width);
                    if (idx + 1) % columns == 0 {
                        ui.end_row();
                    }
                }
            });

        if snapshot.panels.is_empty() {
            ui.label("No panels configured. Add [[panels]] entries to the config file.");
        }
    }

    fn render_panel(
        ui: &mut egui::Ui,
        snapshot: &RenderSnapshot,
        panel: &PanelSnapshot,
        panel_width: f32,
    ) {
        ui.vertical(|ui| {
            ui.set_width(panel_width);
            ui.label(egui::RichText::new(&panel.title).strong());

            let mut plot = Plot::new(&panel.title)
                .height(200.0)
                .width(panel_width)
                .allow_scroll(false);

            if let Some(ref unit) = panel.y_label {
                plot = plot.y_axis_label(unit.clone());
            }

            // When frozen, pin the x-axis to the captured snapshot window so that
            // the view stays fixed even as new samples accumulate in the ring buffer.
            // `include_x` nudges egui_plot to ensure both endpoints are in view;
            // combined with `allow_scroll(false)` this holds the window in place.
            if snapshot.frozen
                && let Some(xmax) = snapshot.frozen_xmax
            {
                let xmin = xmax - snapshot.window_seconds;
                plot = plot
                    .include_x(xmin)
                    .include_x(xmax)
                    .auto_bounds(egui::Vec2b::new(false, true));
            }

            plot.show(ui, |plot_ui| {
                for (ch, points) in &panel.series {
                    let line = Line::new(ch.as_str(), PlotPoints::new(points.clone()));
                    plot_ui.line(line);
                }
            });
        });
    }
}

/// Owned, lock-free copy of the per-frame plot inputs. Built by
/// [`ConsoleState::build_render_snapshot`] under the shared lock; consumed by
/// [`ConsoleState::render_panels`] after the lock is released, so the long [`Plot::show`] calls
/// do not block the buffer-processor thread.
struct RenderSnapshot {
    frozen: bool,
    frozen_xmax: Option<f64>,
    window_seconds: f64,
    columns: usize,
    panels: Vec<PanelSnapshot>,
}

/// Per-panel inputs for a single [`Plot::show`] call.
struct PanelSnapshot {
    title: String,
    y_label: Option<String>,
    /// `(channel_name, points)` in panel-config order. Channels missing from the live ring
    /// buffer are omitted — they would render as empty lines anyway.
    series: Vec<(String, Vec<[f64; 2]>)>,
}

/// eframe-facing wrapper. Owns the shared [`ConsoleState`] and the buffer-processor thread.
/// Drop flips the shutdown atomic and joins the processor (worst case one cadence tick).
pub struct DeimosConsoleApp {
    state: Arc<Mutex<ConsoleState>>,
    shutdown: Arc<AtomicBool>,
    /// `Option` so [`Drop`] can `take()` and join.
    processor: Option<thread::JoinHandle<()>>,
}

impl DeimosConsoleApp {
    /// Build the wrapper, the shared state, and spawn the buffer-processor thread.
    ///
    /// The processor thread is named `deimos-console-processor` so it shows up identifiably in
    /// debuggers and `ps`. It loops on the shutdown atomic, calling `background_tick` under the
    /// shared mutex each iteration and sleeping for [`PROCESSOR_TICK_INTERVAL`] in between.
    pub fn new(
        config: DeimosConsoleConfig,
        rx: crossbeam_channel::Receiver<ReportingMessage>,
        counters: ReceiverCounters,
    ) -> Self {
        let state = Arc::new(Mutex::new(ConsoleState::new(config, rx, counters)));
        let shutdown = Arc::new(AtomicBool::new(false));

        let state_for_thread = Arc::clone(&state);
        let shutdown_for_thread = Arc::clone(&shutdown);
        let processor = thread::Builder::new()
            .name("deimos-console-processor".to_string())
            .spawn(move || {
                while !shutdown_for_thread.load(Ordering::Relaxed) {
                    // Propagate poisoning: a panic mid-tick has left state inconsistent and
                    // recovering on the processor side would mask the original fault.
                    state_for_thread
                        .lock()
                        .expect("deimos-console state mutex poisoned")
                        .background_tick();
                    thread::sleep(PROCESSOR_TICK_INTERVAL);
                }
            })
            .expect("failed to spawn deimos-console-processor thread");

        Self {
            state,
            shutdown,
            processor: Some(processor),
        }
    }
}

impl eframe::App for DeimosConsoleApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        // `render_ui` manages the shared lock itself: it takes the lock briefly to render the
        // top section and snapshot per-panel data, then drops it before each `Plot::show` runs
        // so the buffer-processor thread is not blocked on the long render.
        ConsoleState::render_ui(&self.state, ui);
    }
}

impl Drop for DeimosConsoleApp {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Relaxed);
        if let Some(h) = self.processor.take() {
            // Discard the `JoinHandle::join` result: a panic payload here would only re-panic
            // into eframe's teardown, where there is no useful logging target left.
            let _ = h.join();
        }
    }
}

/// Return an `HH:MM:SS.mmm` local-time prefix for stderr telemetry lines so they
/// can be correlated against the controller's ISO-timestamped tracing output.
fn wall_clock_prefix() -> String {
    chrono::Local::now().format("[%H:%M:%S%.3f]").to_string()
}

/// Stamp the forensic log path with the current epoch-ns so each session writes a distinct
/// file. The 64-MiB shard suffix (`_1.csv`, `_2.csv`, …) layers on top.
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
mod tests;
