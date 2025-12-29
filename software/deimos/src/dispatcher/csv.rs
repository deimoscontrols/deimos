//! A plain-text CSV data target with fixed-width row formatting.

use std::io::{BufWriter, Seek, SeekFrom, Write};
use std::path::PathBuf;

use std::fs::File;
use std::time::SystemTime;

use serde::{Deserialize, Serialize};
use std::sync::mpsc::{Sender, channel};
use std::thread::{self, Builder, JoinHandle};
use tracing::{error, info, warn};

#[cfg(feature = "python")]
use pyo3::prelude::*;

use crate::controller::context::ControllerCtx;
use crate::py_json_methods;

use super::{Dispatcher, Overflow, csv_header, csv_row_fixed_width};

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

    #[serde(skip)]
    worker: Option<WorkerHandle>,
}

impl CsvDispatcher {
    pub fn new(chunk_size_megabytes: usize, overflow_behavior: Overflow) -> Self {
        Self {
            chunk_size_megabytes,
            overflow_behavior,

            worker: None,
        }
    }
}

py_json_methods!(
    CsvDispatcher,
    Dispatcher,
    #[new]
    fn py_new(chunk_size_megabytes: usize, overflow_behavior: Overflow) -> PyResult<Self> {
        Ok(Self::new(chunk_size_megabytes, overflow_behavior))
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
        // Shut down any existing workers by dropping their tx handle
        self.worker = None;

        // Assemble header row
        let header = csv_header(channel_names);

        // Preallocate output file
        let total_len = 1024 * 1_024 * self.chunk_size_megabytes;
        let filepath = ctx.op_dir.join(format!("{}.csv", ctx.op_name));

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
        // Drop worker handle, closing thread channel,
        // which will indicate to the detached worker that it should
        // finish storing its buffered data and shut down.
        self.worker = None;
        Ok(())
    }
}

struct WorkerHandle {
    pub tx: Sender<(SystemTime, i64, Vec<f64>)>,
    _thread: JoinHandle<()>,
}

impl WorkerHandle {
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

        let _thread = Builder::new()
            .name("csv-dispatcher".to_string())
            .spawn(move || {
                // Bind to assigned core, and set priority only if the core is not shared with the control loop
                {
                    let success = core_affinity::set_for_current(core_affinity::CoreId {
                        id: core_assignment,
                    });
                    if !success {
                        warn!("Failed to set core affinity for CSV dispatcher");
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
            .expect("Failed to spawn CSV dispatcher thread");

        Ok(Self { tx, _thread })
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
