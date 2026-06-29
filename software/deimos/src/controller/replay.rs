//! Offline replay of captured peripheral outputs through the current calc graph.

use std::{
    collections::{BTreeMap, VecDeque},
    path::{Path, PathBuf},
    time::SystemTime,
};

use crate::{dispatcher::Dispatcher, dispatcher::load_csv, peripheral::Peripheral};

/// One replayed controller cycle.
///
/// `peripheral_outputs` is keyed by controller peripheral name. Each value must
/// follow that peripheral's [`Peripheral::output_names`] order.
pub struct ReplayCycle {
    /// Wall-clock time associated with this replayed row.
    pub time: SystemTime,
    /// Controller-relative timestamp in nanoseconds.
    pub timestamp: i64,
    /// Raw peripheral outputs to inject before running calcs.
    pub peripheral_outputs: BTreeMap<String, Vec<f64>>,
}

/// Source of raw peripheral outputs for [`crate::Controller::replay`].
///
/// Implementations may load data eagerly or stream it from disk/network. The
/// `init` hook receives the controller's peripheral definitions so sources can
/// validate that they can provide every required raw output in the right order.
pub trait ReplaySource {
    /// Prepare the source for replay with the controller's peripheral set.
    fn init(&mut self, peripherals: &BTreeMap<String, Box<dyn Peripheral>>) -> Result<(), String>;

    /// Return the next cycle, or `Ok(None)` when replay is complete.
    fn next_cycle(&mut self) -> Result<Option<ReplayCycle>, String>;
}

/// CSV-backed replay source.
///
/// The CSV must contain `timestamp` and `time` columns plus one column for each
/// raw peripheral output, named as `<peripheral>.<output>`, for example
/// `p1.ain0`.
pub struct CsvReplaySource {
    path: PathBuf,
    rows: VecDeque<ReplayCycle>,
}

impl CsvReplaySource {
    /// Construct a source from a CSV path.
    ///
    /// The file is not opened until [`ReplaySource::init`] so construction can
    /// stay cheap and independent of the controller peripheral set.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        // Rows are populated during `init`, once the source can validate CSV
        // columns against the controller's registered peripherals.
        Self {
            path: path.into(),
            rows: VecDeque::new(),
        }
    }

    /// Path being replayed.
    pub fn path(&self) -> &Path {
        // Return the configured path for diagnostics or caller-side logging.
        &self.path
    }
}

impl ReplaySource for CsvReplaySource {
    /// Load and validate replay rows from the CSV file.
    fn init(&mut self, peripherals: &BTreeMap<String, Box<dyn Peripheral>>) -> Result<(), String> {
        // Rebuild the row queue every time replay initializes this source, so
        // a source value can be reused after the CSV file changes on disk.
        self.rows = read_csv_replay_rows(&self.path, peripherals)?;
        Ok(())
    }

    /// Pop the next CSV row from the in-memory replay queue.
    fn next_cycle(&mut self) -> Result<Option<ReplayCycle>, String> {
        // `pop_front` naturally encodes end-of-file as `None`.
        Ok(self.rows.pop_front())
    }
}

/// Build replay rows from a dispatcher-style CSV file.
fn read_csv_replay_rows(
    path: &Path,
    peripherals: &BTreeMap<String, Box<dyn Peripheral>>,
) -> Result<VecDeque<ReplayCycle>, String> {
    // Load the generic dispatcher CSV first; required raw peripheral columns
    // are checked against the loaded channel list below.
    let csv = load_csv(path)?;
    let peripheral_columns = peripheral_output_columns(csv.channel_names(), peripherals)?;

    let mut rows = VecDeque::new();
    for row in csv.rows() {
        // Reconstruct each peripheral's raw output vector in the exact order
        // expected by `Peripheral::parse_operating_roundtrip`.
        let mut peripheral_outputs = BTreeMap::new();
        for (peripheral_name, column_indices) in &peripheral_columns {
            let mut outputs = Vec::with_capacity(column_indices.len());
            for &column_idx in column_indices {
                outputs.push(row.channel_values[column_idx]);
            }
            peripheral_outputs.insert(peripheral_name.clone(), outputs);
        }

        // Store eagerly for the first implementation. This keeps the replay
        // trait simple; a streaming CSV source can be added later if needed.
        rows.push_back(ReplayCycle {
            time: row.time,
            timestamp: row.timestamp,
            peripheral_outputs,
        });
    }

    Ok(rows)
}

/// Map each peripheral's output vector order to source CSV column indices.
fn peripheral_output_columns(
    channel_names: &[String],
    peripherals: &BTreeMap<String, Box<dyn Peripheral>>,
) -> Result<BTreeMap<String, Vec<usize>>, String> {
    // BTreeMap iteration preserves the same deterministic peripheral order used
    // elsewhere in the controller.
    let mut columns = BTreeMap::new();

    for (peripheral_name, peripheral) in peripherals {
        // Keep column indices in peripheral output order so replay can rebuild
        // the exact output slice expected by the orchestrator.
        let mut peripheral_columns = Vec::new();
        for output_name in peripheral.output_names() {
            let field_name = format!("{peripheral_name}.{output_name}");
            peripheral_columns.push(
                channel_names
                    .iter()
                    .position(|channel_name| channel_name == &field_name)
                    .ok_or_else(|| {
                        format!("Replay CSV is missing required raw output `{field_name}`")
                    })?,
            );
        }
        columns.insert(peripheral_name.clone(), peripheral_columns);
    }

    Ok(columns)
}

/// Initialize replay dispatchers using the orchestrator's normal dispatch channels.
pub(super) fn init_replay_dispatchers(
    ctx: &mut crate::ControllerCtx,
    dispatchers: &mut BTreeMap<String, Box<dyn Dispatcher>>,
    channel_names: &[String],
    channel_units: Vec<Option<String>>,
) -> Result<(), String> {
    // Match the live controller behavior by making channel units visible in the
    // context before dispatcher initialization.
    ctx.channel_units = channel_units;
    for (core_assignment, dispatcher) in dispatchers.values_mut().enumerate() {
        // There is no real core assignment during replay, but dispatchers use
        // this value only as a worker-affinity hint, so deterministic indices
        // are sufficient.
        dispatcher.init(ctx, channel_names, core_assignment)?;
    }
    Ok(())
}
