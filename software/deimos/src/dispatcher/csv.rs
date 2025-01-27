//! A plain-text CSV data target with fixed-width row formatting.

use std::io::{BufWriter, Seek, SeekFrom, Write};
use std::path::PathBuf;

use std::fs::File;
use std::time::SystemTime;

use std::sync::mpsc::{channel, Sender};
use std::thread::{self, spawn, JoinHandle};

use core_affinity::CoreId;

use serde::{Deserialize, Serialize};

use crate::controller::context::ControllerCtx;

use super::{csv_header, csv_row_fixed_width, Dispatcher};

/// Choice of behavior when the current file is full
#[derive(Serialize, Deserialize, Default, Clone, Copy, Debug)]
pub enum Overflow {
    /// Wrap back to the beginning of the file and
    /// overwrite, starting with the oldest data
    #[default]
    Wrap,

    /// Create a new file
    NewFile,

    /// Panic on overflow if neither wrapping nor creating a new file is viable
    Panic,
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

#[typetag::serde]
impl Dispatcher for CsvDispatcher {
    fn initialize(
        &mut self,
        ctx: &ControllerCtx,
        _dt_ns: u32,
        channel_names: &[String],
        core_assignment: CoreId,
    ) -> Result<(), String> {
        // Shut down any existing workers by dropping their tx handle
        self.worker = None;

        // Assemble header row
        let header = csv_header(channel_names);

        // Preallocate output file
        let total_len = 1024 * 1_000 * self.chunk_size_megabytes;
        let filepath = ctx.op_dir.join(format!("{}.csv", ctx.op_name));

        // Spawn worker
        self.worker = Some(WorkerHandle::new(
            filepath,
            header,
            total_len,
            self.overflow_behavior,
            core_assignment,
        ));

        Ok(())
    }

    fn consume(
        &mut self,
        time: SystemTime,
        timestamp: i64,
        channel_values: Vec<f64>,
    ) -> Result<(), String> {
        match &mut self.worker {
            Some(worker) => {
                worker.tx.send((time, timestamp, channel_values)).unwrap();
            }
            None => panic!("Dispatcher must be initialized before consuming data"),
        }

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
        core_assignment: CoreId,
    ) -> Self {
        let (tx, rx) = channel::<(SystemTime, i64, Vec<f64>)>();

        let original_filename = path.file_stem().unwrap().to_str().unwrap().to_owned();
        let header_len = header.len();

        // Allocate first file and buffered writer
        let mut writer = new_file(&path, &header, total_size);

        let mut file_size = header_len;
        let mut shard_number: u64 = 0;

        let _thread = spawn(move || {
            // Bind to assigned core, and set priority only if the core is not shared with the control loop
            core_affinity::set_for_current(core_assignment);
            if core_assignment.id > 0 {
                let _ = thread_priority::set_current_thread_priority(
                    thread_priority::ThreadPriority::Max,
                );
            }

            // Make single-line buffer that will grow and
            // permanently maintain the largest line length that has been seen
            // so that reallocations should happen rarely if ever
            let mut stringbuf = String::new();
            loop {
                match rx.recv() {
                    Err(_) => {
                        let _ = writer.flush();
                        let mut file = writer.into_inner().unwrap();
                        let len = get_file_loc(&mut file);
                        file.set_len(len).unwrap();
                        return;
                    }
                    Ok(vals) => {
                        csv_row_fixed_width(&mut stringbuf, vals);

                        // Make sure there is space in the file,
                        // otherwise wrap back to the beginning and overwrite
                        // TODO: right now this may cause a tear in the file if NaN values are encountered
                        let n_to_write = stringbuf.len();
                        if get_file_loc(&mut writer) as usize + n_to_write > total_size {
                            match overflow_behavior {
                                Overflow::Wrap => {
                                    writer.seek(SeekFrom::Start(header_len as u64)).unwrap();
                                }
                                Overflow::Panic => {
                                    panic!(
                                        "CSV dispatcher overflowed with `Panic` behavior selected"
                                    );
                                }
                                Overflow::NewFile => {
                                    shard_number += 1;

                                    // Assemble new file name
                                    let filename_new =
                                        format!("{original_filename}_{shard_number}.csv");
                                    let path_new: PathBuf =
                                        path.parent().unwrap().join(filename_new);

                                    writer = new_file(&path_new, &header, total_size);
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
        });

        Self { tx, _thread }
    }
}

/// Get current cursor location in file
fn get_file_loc<T: Seek>(f: &mut T) -> u64 {
    f.stream_position().unwrap()
}

/// Create a new file with a fixed length, and return a buffered writer
fn new_file(path: &PathBuf, header: &str, total_size: usize) -> BufWriter<File> {
    let file = File::create(path).unwrap();
    file.set_len(total_size as u64).unwrap();
    let mut writer: BufWriter<File> = BufWriter::new(file);
    writer.write_all(header.as_bytes()).unwrap();
    writer
}
