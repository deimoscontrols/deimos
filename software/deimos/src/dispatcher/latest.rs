use std::sync::{Arc, RwLock};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

use crate::controller::context::ControllerCtx;

use super::{fmt_time, header_columns, Dispatcher, Row};

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
pub struct LatestValueDispatcher {
    #[serde(skip)]
    handle: LatestValueHandle,
}

impl LatestValueDispatcher {
    pub fn new() -> (Self, LatestValueHandle) {
        let cell = RowCell::new(Row::default());
        let handle = LatestValueHandle {
            row: cell.clone(),
            channel_names: Arc::new(RwLock::new(Vec::new())),
        };
        (Self { handle: handle.clone() }, handle)
    }
}

#[typetag::serde]
impl Dispatcher for LatestValueDispatcher {
    fn init(
        &mut self,
        _ctx: &ControllerCtx,
        channel_names: &[String],
        _core_assignment: usize,
    ) -> Result<(), String> {
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
        Ok(())
    }

    fn consume(
        &mut self,
        time: SystemTime,
        timestamp: i64,
        channel_values: Vec<f64>,
    ) -> Result<(), String> {
        let row = Row {
            system_time: fmt_time(time),
            timestamp,
            channel_values,
        };
        self.handle.row.store(row);
        Ok(())
    }

    fn terminate(&mut self) -> Result<(), String> {
        Ok(())
    }
}
