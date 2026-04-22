use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use eframe::egui;
use egui_plot::{Line, Plot, PlotPoints};

use deimos::dispatcher::ReportingMessage;

use crate::config::DeimosConsoleConfig;
use crate::forensic::ForensicLog;
use crate::receiver::receiver_dropped_frames;

/// Four-state connection health indicator for the operator console.
///
/// Computed each UI frame from the current schema state, the wall-clock time of
/// the last received `Row`, and whether the receiver thread is still alive.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ConnectionHealth {
    /// No `Schema` has been received yet; cannot assess liveness.
    NoSchemaYet,
    /// A `Row` was received within the staleness threshold.
    Fresh,
    /// A `Schema` exists but no `Row` has arrived, or the last `Row` arrived more
    /// than `staleness_threshold` ago.
    Stale,
    /// The receiver thread has exited due to a non-transient socket error. No further
    /// packets will arrive in this session without restarting the console.
    ReceiverDead,
}

/// Maximum `Row` messages buffered before a `Schema` is received.
/// When full, the oldest entry is dropped so the most recent N rows are available on schema arrival.
const MAX_PENDING_ROWS: usize = 1000;

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
    /// The UI uses this to display a "session ended" indicator.
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
    /// While frozen, samples are appended but eviction is skipped so unfreezing restores history.
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
        let cutoff = timestamp - window;

        for (i, &value) in values.iter().enumerate() {
            let name = schema.channel_names[i].clone();
            let buf = self.channel_data.entry(name).or_default();

            if gap_detected {
                let gap_ts = timestamp - 1e-9;
                buf.push_back((gap_ts, f64::NAN));
            }

            buf.push_back((timestamp, value));

            if !self.frozen {
                while buf.front().is_some_and(|&(t, _)| t < cutoff) {
                    buf.pop_front();
                }
            }
        }

        if let Some(log) = &mut self.forensic_log {
            if let Err(e) = log.write_row(viewer_received_at, seq, timestamp, system_time, &values)
            {
                let reason = format!("write failed: {e}");
                eprintln!("deimos-console: forensic log {reason}; disabling log");
                self.forensic_disabled_reason = Some(reason);
                self.forensic_log = None;
            }
        }
    }

    /// Compute the current connection health state.
    ///
    /// - [`ConnectionHealth::NoSchemaYet`] — no `Schema` has been received.
    /// - [`ConnectionHealth::Fresh`] — schema received and a `Row` arrived within the
    ///   configured staleness threshold.
    /// - [`ConnectionHealth::Stale`] — schema received but no `Row` has been seen, or the
    ///   last `Row` arrived more than `staleness_threshold_secs` ago.
    pub(crate) fn connection_health(&self) -> ConnectionHealth {
        if self.receiver_dead {
            return ConnectionHealth::ReceiverDead;
        }
        if self.schema.is_none() {
            return ConnectionHealth::NoSchemaYet;
        }
        let threshold = Duration::from_secs_f64(self.config.staleness_threshold_secs);
        match self.last_row_received_at {
            Some(t) if t.elapsed() <= threshold => ConnectionHealth::Fresh,
            _ => ConnectionHealth::Stale,
        }
    }
}

impl eframe::App for DeimosConsoleApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Drain all pending messages from the receiver channel.
        // If the channel is disconnected (receiver thread has died), set the sticky flag
        // so the UI can display a distinct "receiver dead" state.
        loop {
            match self.rx.try_recv() {
                Err(crossbeam_channel::TryRecvError::Disconnected) => {
                    self.receiver_dead = true;
                    break;
                }
                Err(crossbeam_channel::TryRecvError::Empty) => break,
                Ok(msg) => match msg {
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
                                        let reason = format!(
                                            "open failed: {e} ({})",
                                            session_path.display()
                                        );
                                        eprintln!("deimos-console: forensic log {reason}");
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
                            self.process_row(
                                viewer_received_at,
                                seq,
                                timestamp,
                                &system_time,
                                values,
                            );
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
                },
            }
        }

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
                        egui::Color32::YELLOW,
                        format!(
                            "No schema yet ({} row(s) buffered)",
                            self.pending_rows.len()
                        ),
                    ),
                    ConnectionHealth::Fresh => {
                        let ch_count = self.schema.as_ref().map_or(0, |s| s.channel_names.len());
                        (
                            egui::Color32::GREEN,
                            format!("Fresh \u{2014} {ch_count} channel(s)"),
                        )
                    }
                    ConnectionHealth::Stale => (
                        egui::Color32::RED,
                        format!(
                            "Stale \u{2014} no data for >{:.1} s",
                            self.config.staleness_threshold_secs
                        ),
                    ),
                    ConnectionHealth::ReceiverDead => (
                        egui::Color32::from_rgb(180, 0, 255),
                        "Receiver dead \u{2014} socket error, restart the console".to_string(),
                    ),
                };

                ui.horizontal(|ui| {
                    ui.colored_label(dot_color, "\u{25cf}");
                    ui.colored_label(dot_color, label);
                });

                // Session-end overlay rendered below the health indicator when applicable.
                if self
                    .schema
                    .as_ref()
                    .map_or(false, |s| s.session_end_received)
                {
                    ui.colored_label(egui::Color32::LIGHT_BLUE, "Session ended.");
                }
            }

            // Missing-channel warnings
            if !self.missing_channels.is_empty() {
                ui.separator();
                ui.colored_label(
                    egui::Color32::from_rgb(255, 160, 0),
                    format!(
                        "Warning: {} configured channel(s) not found in schema:",
                        self.missing_channels.len()
                    ),
                );
                for (panel_title, channel_name) in &self.missing_channels {
                    ui.colored_label(
                        egui::Color32::from_rgb(255, 160, 0),
                        format!(
                            "  panel \"{panel_title}\": channel \"{channel_name}\" not in schema"
                        ),
                    );
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
                let color =
                    if wire_drops > 0 || recv_drops > 0 || drift_drops > 0 || pending_drops > 0 {
                        egui::Color32::from_rgb(255, 80, 80)
                    } else {
                        egui::Color32::GRAY
                    };
                let mut status = format!(
                    "Dropped frames — wire gaps: {wire_drops}  receiver backpressure: {recv_drops}"
                );
                if drift_drops > 0 {
                    status.push_str(&format!("  drift:{drift_drops}"));
                }
                if pending_drops > 0 {
                    status.push_str(&format!("  preoverflow:{pending_drops}"));
                }
                ui.colored_label(color, status);
            }

            // Forensic log disabled indicator.
            if let Some(ref reason) = self.forensic_disabled_reason {
                ui.colored_label(
                    egui::Color32::from_rgb(255, 140, 0),
                    format!("Forensic log disabled: {reason}"),
                );
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

                let mut plot = Plot::new(&panel.title).height(200.0).allow_scroll(false);

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
                            let points: Vec<[f64; 2]> = buf.iter().map(|&(t, v)| [t, v]).collect();
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

    /// After a Schema and an immediate Row, `connection_health` must return `Fresh`, then
    /// transition to `Stale` once the staleness threshold has elapsed.
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

        // Immediately after Row arrival the connection should be Fresh.
        assert_eq!(
            app.connection_health(),
            ConnectionHealth::Fresh,
            "expected Fresh immediately after Row arrival"
        );

        // Wait longer than the staleness threshold and assert the transition to Stale.
        std::thread::sleep(Duration::from_millis(threshold_ms * 2));

        assert_eq!(
            app.connection_health(),
            ConnectionHealth::Stale,
            "expected Stale after staleness threshold elapsed"
        );
    }
}
