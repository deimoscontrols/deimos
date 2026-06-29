//! A plain-text CSV data target with fixed-width row formatting.

use chrono::{DateTime, Utc};
use std::io::{BufWriter, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use std::fs::File;
use std::time::SystemTime;

use serde::{Deserialize, Serialize};
use std::sync::mpsc::{Sender, channel};
use std::thread::{self, Builder, JoinHandle};
use tracing::{debug, error, info};

#[cfg(feature = "python")]
use pyo3::prelude::*;

use crate::controller::context::ControllerCtx;
use crate::py_json_methods;

use super::{Dispatcher, Overflow, csv_header, csv_row_fixed_width, resource_name_with_suffix};

/// In-memory contents of a dispatcher CSV file.
///
/// `channel_names` excludes the fixed `timestamp` and `time` columns. Each
/// row's `channel_values` follows the same order.
#[derive(Clone, Debug)]
pub struct LoadedCsv {
    channel_names: Vec<String>,
    rows: Vec<LoadedCsvRow>,
}

impl LoadedCsv {
    /// Names of loaded data channels, excluding `timestamp` and `time`.
    pub fn channel_names(&self) -> &[String] {
        // Expose the loaded schema without letting callers mutate the row order
        // that all `LoadedCsvRow::channel_values` depend on.
        &self.channel_names
    }

    /// Loaded CSV rows in file order.
    pub fn rows(&self) -> &[LoadedCsvRow] {
        // Rows are kept in the same order as the source file so replay and
        // postprocessing preserve capture chronology.
        &self.rows
    }

    /// Find one channel by exact name.
    pub fn channel_index(&self, name: &str) -> Option<usize> {
        // Exact matching keeps dispatcher field names unambiguous.
        self.channel_names
            .iter()
            .position(|channel| channel == name)
    }

    /// Resolve a set of required channels to row-value indices.
    ///
    /// # Errors
    ///
    /// Returns an error if any requested channel is absent.
    pub fn required_channel_indices<'a>(
        &self,
        names: impl IntoIterator<Item = &'a str>,
    ) -> Result<Vec<usize>, String> {
        // Defer channel-specific validation to callers while keeping the common
        // missing-channel error wording in one place.
        names
            .into_iter()
            .map(|name| {
                self.channel_index(name)
                    .ok_or_else(|| format!("CSV is missing required channel `{name}`"))
            })
            .collect()
    }
}

/// One loaded CSV row.
#[derive(Clone, Debug)]
pub struct LoadedCsvRow {
    /// Wall-clock time parsed from the `time` column.
    pub time: SystemTime,
    /// Controller-relative timestamp in nanoseconds.
    pub timestamp: i64,
    /// Channel values in [`LoadedCsv::channel_names`] order.
    pub channel_values: Vec<f64>,
}

/// Load a complete dispatcher CSV into memory.
///
/// The loader only understands the common dispatcher CSV shape: fixed
/// `timestamp` and `time` columns plus any number of channel value columns. It
/// does not validate which channels are present for a particular use case;
/// callers should use [`LoadedCsv::required_channel_indices`] after loading.
///
/// # Errors
///
/// Returns an error if the file cannot be opened, if fixed time columns are
/// missing, or if any row has invalid timestamp, time, or numeric channel data.
pub fn load_csv(path: impl AsRef<Path>) -> Result<LoadedCsv, String> {
    let path = path.as_ref();
    // Use the csv crate rather than ad hoc splitting so quoted fields and
    // future CSV formatting changes are handled consistently.
    let mut reader = ::csv::ReaderBuilder::new()
        .has_headers(true)
        .from_path(path)
        .map_err(|e| format!("Failed to open CSV {}: {e}", path.display()))?;

    // Clone headers because iterating over records needs mutable access to the
    // reader.
    let headers = reader
        .headers()
        .map_err(|e| format!("Failed to read CSV header from {}: {e}", path.display()))?
        .clone();

    let timestamp_idx = csv_column_index(&headers, "timestamp")?;
    let time_idx = csv_column_index(&headers, "time")?;

    // Keep every non-time column as a channel. This makes the loader generic;
    // channel filtering and required-channel checks happen after loading.
    let mut channel_names = Vec::new();
    let mut channel_indices = Vec::new();
    for (idx, header) in headers.iter().enumerate() {
        if idx == timestamp_idx || idx == time_idx {
            continue;
        }
        channel_names.push(header.to_owned());
        channel_indices.push(idx);
    }

    let mut rows = Vec::new();
    for (row_idx, record) in reader.records().enumerate() {
        let record = record.map_err(|e| {
            format!(
                "Failed to read CSV row {row_idx} from {}: {e}",
                path.display()
            )
        })?;
        let timestamp = parse_csv_timestamp(
            csv_field(&record, timestamp_idx, "timestamp", row_idx)?,
            row_idx,
        )?;
        let time = parse_csv_time(csv_field(&record, time_idx, "time", row_idx)?, row_idx)?;

        let mut channel_values = Vec::with_capacity(channel_indices.len());
        for (&column_idx, channel_name) in channel_indices.iter().zip(channel_names.iter()) {
            channel_values.push(parse_csv_f64(
                csv_field(&record, column_idx, channel_name, row_idx)?,
                row_idx,
                channel_name,
            )?);
        }

        rows.push(LoadedCsvRow {
            time,
            timestamp,
            channel_values,
        });
    }

    Ok(LoadedCsv {
        channel_names,
        rows,
    })
}

/// Find one required CSV column by exact header name.
fn csv_column_index(headers: &::csv::StringRecord, name: &str) -> Result<usize, String> {
    // Header names are exact dispatcher field names.
    headers
        .iter()
        .position(|header| header == name)
        .ok_or_else(|| format!("CSV is missing required column `{name}`"))
}

/// Return a CSV field with row/column context for errors.
fn csv_field<'a>(
    record: &'a ::csv::StringRecord,
    index: usize,
    name: &str,
    row_idx: usize,
) -> Result<&'a str, String> {
    // The csv crate generally enforces rectangular records, but explicit
    // checking keeps malformed files from panicking downstream.
    record
        .get(index)
        .ok_or_else(|| format!("CSV row {row_idx} is missing column `{name}`"))
}

/// Parse the dispatcher-relative nanosecond timestamp column.
fn parse_csv_timestamp(value: &str, row_idx: usize) -> Result<i64, String> {
    // Dispatcher CSV timestamps are controller-relative nanoseconds.
    value
        .trim()
        .parse::<i64>()
        .map_err(|e| format!("Invalid CSV timestamp on row {row_idx}: {e}"))
}

/// Parse the dispatcher wall-clock time column.
fn parse_csv_time(value: &str, row_idx: usize) -> Result<SystemTime, String> {
    // Dispatcher CSV wall time is RFC3339 UTC; parsing accepts any RFC3339
    // offset and normalizes to `SystemTime`.
    DateTime::parse_from_rfc3339(value.trim())
        .map(|time| DateTime::<Utc>::from(time).into())
        .map_err(|e| format!("Invalid CSV wall-clock time on row {row_idx}: {e}"))
}

/// Parse one numeric channel value.
fn parse_csv_f64(value: &str, row_idx: usize, column_name: &str) -> Result<f64, String> {
    // Preserve Rust float parsing semantics, including NaN/inf if a dispatcher
    // emitted them.
    value
        .trim()
        .parse::<f64>()
        .map_err(|e| format!("Invalid CSV value for `{column_name}` on row {row_idx}: {e}"))
}

/// A plain-text CSV data target, which uses a pre-sized file
/// to prevent sudden increases in write latency during file resizing.
///
/// On reaching the end of the configured data size, it can be configured to either
/// * Wrap (and start overwriting the existing data from the beginning),
/// * Start a new file, or
/// * Panic
///
/// depending on the appropriate response for the task at hand.
///
/// Each line in this CSV format is fixed-width, meaning the line length will not change
/// with each line. As a result, it is possible to read to a specific time or line
/// in O(1) time by simple arithmetic rather than by reading the whole file.
/// This guarantee will be broken when crossing from year 9999 to year 10000
/// and so on, or if non-finite values are encountered in measurements, calcs, or metrics.
///
/// Writes to disk on a separate thread to avoid blocking the control loop.
#[derive(Serialize, Deserialize, Default)]
#[cfg_attr(feature = "python", pyclass)]
pub struct CsvDispatcher {
    /// Size per file
    chunk_size_megabytes: usize,

    /// Choice of behavior when the current file is full
    overflow_behavior: Overflow,

    /// Optional suffix appended to the op name for the output file stem.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    op_name_suffix: Option<String>,

    #[serde(skip)]
    worker: Option<WorkerHandle>,
}

impl CsvDispatcher {
    pub fn new(chunk_size_megabytes: usize, overflow_behavior: Overflow) -> Box<Self> {
        Box::new(Self {
            chunk_size_megabytes,
            overflow_behavior,
            op_name_suffix: None,

            worker: None,
        })
    }

    /// Override the file stem derived from the controller op name by appending
    /// `_{suffix}` to the base name.
    pub fn with_op_name_suffix(mut self: Box<Self>, suffix: &str) -> Box<Self> {
        self.op_name_suffix = if suffix.is_empty() {
            None
        } else {
            Some(suffix.to_owned())
        };
        self
    }
}

py_json_methods!(
    CsvDispatcher,
    Dispatcher,
    #[new]
    #[pyo3(signature=(chunk_size_megabytes, overflow_behavior, op_name_suffix=None))]
    fn py_new(
        chunk_size_megabytes: usize,
        overflow_behavior: Overflow,
        op_name_suffix: Option<String>,
    ) -> PyResult<Self> {
        let dispatcher = Self::new(chunk_size_megabytes, overflow_behavior);
        let dispatcher = if let Some(suffix) = op_name_suffix {
            dispatcher.with_op_name_suffix(&suffix)
        } else {
            dispatcher
        };
        Ok(*dispatcher)
    }
);

#[typetag::serde]
impl Dispatcher for CsvDispatcher {
    fn init(
        &mut self,
        ctx: &ControllerCtx,
        channel_names: &[String],
        core_assignment: usize,
    ) -> Result<(), String> {
        // Finish any existing worker before reinitializing so callers never
        // see a stale writer racing with the new output file.
        if let Some(worker) = self.worker.take() {
            worker.terminate()?;
        }

        // Assemble header row
        let header = csv_header(channel_names);

        // Preallocate output file
        let total_len = 1024 * 1_024 * self.chunk_size_megabytes;
        let resource_name = resource_name_with_suffix(&ctx.op_name, self.op_name_suffix.as_deref());
        let filepath = ctx.op_dir.join(format!("{resource_name}.csv"));

        info!(
            "Initializing CSV dispatcher with file path: {:?}",
            &filepath
        );

        // Spawn worker
        self.worker = Some(WorkerHandle::new(
            filepath,
            header,
            total_len,
            self.overflow_behavior,
            core_assignment,
        )?);

        Ok(())
    }

    fn consume(
        &mut self,
        time: SystemTime,
        timestamp: i64,
        channel_values: Vec<f64>,
    ) -> Result<(), String> {
        match &mut self.worker {
            Some(worker) => worker
                .tx
                .send((time, timestamp, channel_values))
                .map_err(|e| format!("Failed to queue data to write to CSV: {e}")),
            None => Err("Dispatcher must be initialized before consuming data".to_string()),
        }
    }

    fn terminate(&mut self) -> Result<(), String> {
        // Close the channel and wait for the writer thread to drain queued
        // rows, flush its buffer, and trim the preallocated file. Callers may
        // immediately read the CSV after `terminate` returns.
        if let Some(worker) = self.worker.take() {
            worker.terminate()
        } else {
            Ok(())
        }
    }
}

/// Handle for the background CSV writer thread.
struct WorkerHandle {
    pub tx: Sender<(SystemTime, i64, Vec<f64>)>,
    thread: JoinHandle<()>,
}

impl WorkerHandle {
    /// Spawn a background writer for one CSV output stream.
    fn new(
        path: PathBuf,
        header: String,
        total_size: usize,
        overflow_behavior: Overflow,
        core_assignment: usize,
    ) -> Result<Self, String> {
        let (tx, rx) = channel::<(SystemTime, i64, Vec<f64>)>();

        let original_filename = path.file_stem().unwrap().to_str().unwrap().to_owned();
        let header_len = header.len();

        // Allocate first file and buffered writer
        let mut writer = new_file(&path, &header, total_size)?;

        let mut file_size = header_len;
        let mut shard_number: u64 = 0;

        let thread = Builder::new()
            .name("csv-dispatcher".to_string())
            .spawn(move || {
                // Bind to assigned core, and set priority only if the core is not shared with
                // the control loop. Affinity is a best-effort hint for this dispatcher — some
                // platforms (notably macOS, which only supports advisory thread placement hints)
                // return `false` here on every call, and the dispatcher still works correctly
                // without pinning. Log at debug level so operators see it when they want to, but
                // don't spam the default run log on every macOS session.
                {
                    let success = core_affinity::set_for_current(core_affinity::CoreId {
                        id: core_assignment,
                    });
                    if !success {
                        debug!(
                            "CSV dispatcher: core_affinity::set_for_current returned false \
                             (expected on macOS and other platforms without hard affinity)"
                        );
                    }
                }

                // Make single-line buffer that will grow and
                // permanently maintain the largest line length that has been seen
                // so that reallocations should happen rarely if ever
                let mut stringbuf = String::new();
                loop {
                    match rx.recv() {
                        Err(_) => {
                            // If we are shutting down, flush the buffer and
                            // trim the file length to release remaining reserved space
                            let _ = writer.flush();
                            let mut file = writer.into_inner().unwrap();
                            let len = get_file_loc(&mut file);
                            let _ = file.set_len(len);
                            return;
                        }
                        Ok((time, timestamp, channel_values)) => {
                            csv_row_fixed_width(
                                &mut stringbuf,
                                (time, timestamp, channel_values.as_slice()),
                            );

                            // Make sure there is space in the file,
                            // otherwise wrap back to the beginning and overwrite
                            // TODO: right now this may cause a tear in the file if NaN values are encountered
                            let n_to_write = stringbuf.len();
                            if get_file_loc(&mut writer) as usize + n_to_write > total_size {
                                match overflow_behavior {
                                    Overflow::Wrap => {
                                        writer.seek(SeekFrom::Start(header_len as u64)).unwrap();
                                    }
                                    Overflow::Error => {
                                        error!(
                                            "CSV file is full with overflow policy set to Error"
                                        );
                                        return;
                                    }
                                    Overflow::NewFile => {
                                        shard_number += 1;

                                        // Assemble new file name
                                        let filename_new =
                                            format!("{original_filename}_{shard_number}.csv");
                                        info!("Reserving new CSV file at {}", &filename_new);
                                        let path_new: PathBuf =
                                            path.parent().unwrap().join(filename_new);

                                        writer = new_file(&path_new, &header, total_size).unwrap();

                                        info!("CSV dispatcher moved to new file at {path_new:?}")
                                    }
                                }
                            }

                            // Write to disk
                            writer.write_all(stringbuf.as_bytes()).unwrap();

                            // Keep track of the largest file size so far
                            file_size = file_size.max(get_file_loc(&mut writer) as usize);
                        }
                    }

                    // Deliberately yield the thread to make time for the OS
                    // and to avoid interfering with other loops
                    thread::yield_now();
                }
            })
            .map_err(|e| format!("Failed to spawn CSV dispatcher thread: {e}"))?;

        Ok(Self { tx, thread })
    }

    /// Shut down the writer and wait until the CSV file is fully finalized.
    fn terminate(self) -> Result<(), String> {
        // Dropping the final sender lets the worker drain any queued rows and
        // then exit through the channel-disconnected branch.
        drop(self.tx);

        self.thread.join().map_err(|payload| {
            format!("CSV dispatcher worker panicked: {}", panic_message(payload))
        })
    }
}

/// Convert a thread panic payload into a readable error string.
fn panic_message(payload: Box<dyn std::any::Any + Send + 'static>) -> String {
    // Rust panic payloads are commonly `&str` or `String`; keep a fallback for
    // custom payloads.
    if let Some(message) = payload.downcast_ref::<&str>() {
        (*message).to_owned()
    } else if let Some(message) = payload.downcast_ref::<String>() {
        message.clone()
    } else {
        "unknown panic payload".to_owned()
    }
}

/// Get current cursor location in file
fn get_file_loc<T: Seek>(f: &mut T) -> u64 {
    f.stream_position().unwrap()
}

/// Create a new file with a fixed length, and return a buffered writer
fn new_file(path: &PathBuf, header: &str, total_size: usize) -> Result<BufWriter<File>, String> {
    let file = File::create(path).map_err(|e| format!("{e}"))?;
    file.set_len(total_size as u64)
        .map_err(|e| format!("{e}"))?;
    let mut writer: BufWriter<File> = BufWriter::new(file);
    writer
        .write_all(header.as_bytes())
        .map_err(|e| format!("{e}"))?;
    Ok(writer)
}

#[cfg(test)]
mod tests {
    use super::{CsvDispatcher, load_csv};
    use crate::controller::context::ControllerCtx;
    use crate::dispatcher::{Dispatcher, Overflow};
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{Duration, UNIX_EPOCH};

    static NEXT_TMP_DIR_ID: AtomicU64 = AtomicU64::new(0);

    /// Build a unique temporary output directory without adding a test-only dependency.
    fn unique_temp_dir() -> PathBuf {
        // Include the process id and a monotonic counter so parallel test runs
        // do not collide under the shared system temp directory.
        let id = NEXT_TMP_DIR_ID.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "deimos_csv_dispatcher_{}_{}",
            std::process::id(),
            id
        ))
    }

    #[test]
    fn terminate_finalizes_csv_before_returning() {
        let dir = unique_temp_dir();
        fs::create_dir_all(&dir).expect("create CSV test output dir");

        let mut ctx = ControllerCtx {
            op_name: "cal_run".to_owned(),
            op_dir: dir.clone(),
            ..ControllerCtx::default()
        };
        // Keep the test independent of whatever default control timing is used
        // elsewhere in the controller context.
        ctx.dt_ns = 10_000_000;

        let channel_names = vec!["ain3".to_owned()];
        let mut dispatcher = CsvDispatcher::new(1, Overflow::Error);
        dispatcher
            .init(&ctx, &channel_names, 0)
            .expect("initialize CSV dispatcher");

        for idx in 0..1_000_i64 {
            // Queue enough rows that the worker has real buffered work to drain
            // during termination.
            dispatcher
                .consume(
                    UNIX_EPOCH + Duration::from_millis(idx as u64),
                    idx,
                    vec![idx as f64],
                )
                .expect("queue CSV row");
        }

        dispatcher.terminate().expect("terminate CSV dispatcher");

        let csv_path = dir.join("cal_run.csv");
        let loaded = load_csv(&csv_path).expect("load finalized CSV");
        assert_eq!(loaded.channel_names(), channel_names.as_slice());
        assert_eq!(loaded.rows().len(), 1_000);

        fs::remove_dir_all(dir).expect("remove CSV test output dir");
    }
}
