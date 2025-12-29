use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, RwLock};
use std::thread::{Builder, JoinHandle};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

#[cfg(feature = "python")]
use pyo3::prelude::*;

use crate::controller::context::ControllerCtx;
use crate::py_json_methods;

use super::{Dispatcher, Row, fmt_time, header_columns};

/// Thread-safe cell holding the latest `Row` snapshot.
#[derive(Clone, Default)]
pub struct RowCell {
    inner: Arc<RwLock<Arc<Row>>>,
}

impl RowCell {
    pub fn new(row: Row) -> Self {
        Self {
            inner: Arc::new(RwLock::new(Arc::new(row))),
        }
    }

    /// Replace the stored row with a new snapshot.
    pub fn store(&self, row: Row) {
        if let Ok(mut w) = self.inner.write() {
            *w = Arc::new(row);
        }
    }

    /// Clone the latest snapshot handle.
    pub fn load(&self) -> Arc<Row> {
        self.inner
            .read()
            .map(|r| Arc::clone(&*r))
            .unwrap_or_else(|_| Arc::new(Row::default()))
    }
}

/// Cloneable handle for accessing the latest row and associated column metadata.
#[derive(Clone, Default)]
pub struct LatestValueHandle {
    row: RowCell,
    channel_names: Arc<RwLock<Vec<String>>>,
}

impl LatestValueHandle {
    /// Get the latest row snapshot.
    pub fn latest_row(&self) -> Arc<Row> {
        self.row.load()
    }

    /// Get the most recently configured channel names.
    pub fn channel_names(&self) -> Vec<String> {
        self.channel_names
            .read()
            .map(|v| v.clone())
            .unwrap_or_default()
    }

    /// Get header strings including timestamp/time and channel names.
    pub fn headers(&self) -> Vec<String> {
        header_columns(&self.channel_names())
    }
}

/// Dispatcher that always keeps the latest row available via a shared handle.
#[derive(Serialize, Deserialize, Default)]
#[cfg_attr(feature = "python", pyclass)]
pub struct LatestValueDispatcher {
    #[serde(skip)]
    handle: LatestValueHandle,
    #[serde(skip)]
    worker: Option<WorkerHandle>,
}

impl LatestValueDispatcher {
    pub fn new() -> (Self, LatestValueHandle) {
        let cell = RowCell::new(Row::default());
        let handle = LatestValueHandle {
            row: cell.clone(),
            channel_names: Arc::new(RwLock::new(Vec::new())),
        };
        (
            Self {
                handle: handle.clone(),
                worker: None,
            },
            handle,
        )
    }
}

py_json_methods!(
    LatestValueDispatcher,
    Dispatcher,
    #[new]
    fn py_new() -> PyResult<Self> {
        let (dispatcher, _handle) = Self::new();
        Ok(dispatcher)
    }
);

#[typetag::serde]
impl Dispatcher for LatestValueDispatcher {
    fn init(
        &mut self,
        _ctx: &ControllerCtx,
        channel_names: &[String],
        core_assignment: usize,
    ) -> Result<(), String> {
        // Stop any existing worker
        self.worker = None;

        // Store channel names for header generation.
        if let Ok(mut w) = self.handle.channel_names.write() {
            *w = channel_names.to_vec();
        }

        // Reset to a placeholder row at init.
        // Populate channel_values to the correct length so readers can rely on the shape.
        self.handle.row.store(Row {
            system_time: fmt_time(SystemTime::UNIX_EPOCH),
            timestamp: 0,
            channel_values: vec![f64::NAN; channel_names.len()],
        });

        // Start worker to keep controller thread non-blocking.
        self.worker = Some(WorkerHandle::new(self.handle.clone(), core_assignment)?);
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
                .map_err(|e| format!("Failed to queue latest value: {e}")),
            None => Err("Dispatcher must be initialized before consuming data".to_string()),
        }
    }

    fn terminate(&mut self) -> Result<(), String> {
        // Drop worker handle to signal shutdown
        self.worker = None;
        Ok(())
    }
}

struct WorkerHandle {
    pub tx: Sender<(SystemTime, i64, Vec<f64>)>,
    _thread: JoinHandle<()>,
}

impl WorkerHandle {
    fn new(handle: LatestValueHandle, core_assignment: usize) -> Result<Self, String> {
        let (tx, rx) = channel::<(SystemTime, i64, Vec<f64>)>();
        let _thread = Builder::new()
            .name("latest-dispatcher".to_string())
            .spawn(move || {
                // Bind to assigned core
                core_affinity::set_for_current(core_affinity::CoreId {
                    id: core_assignment,
                });

                while let Ok((time, timestamp, channel_values)) = rx.recv() {
                    let row = Row {
                        system_time: fmt_time(time),
                        timestamp,
                        channel_values,
                    };
                    handle.row.store(row);
                }
            })
            .expect("Failed to spawn latest dispatcher thread");

        Ok(Self { tx, _thread })
    }
}
