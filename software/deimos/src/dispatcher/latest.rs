use std::sync::{Arc, RwLock};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

use crate::controller::context::ControllerCtx;

use super::{fmt_time, Dispatcher, Row};

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

/// Dispatcher that always keeps the latest row available via a shared handle.
#[derive(Serialize, Deserialize, Default)]
pub struct LatestValueDispatcher {
    #[serde(skip)]
    latest: RowCell,
}

impl LatestValueDispatcher {
    pub fn new() -> (Self, RowCell) {
        let cell = RowCell::new(Row::default());
        (Self { latest: cell.clone() }, cell)
    }
}

#[typetag::serde]
impl Dispatcher for LatestValueDispatcher {
    fn init(
        &mut self,
        _ctx: &ControllerCtx,
        _channel_names: &[String],
        _core_assignment: usize,
    ) -> Result<(), String> {
        // Reset to a blank row at init; caller likely holds the handle already.
        self.latest.store(Row::default());
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
        self.latest.store(row);
        Ok(())
    }

    fn terminate(&mut self) -> Result<(), String> {
        Ok(())
    }
}
