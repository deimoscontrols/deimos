//! Dataframe dispatcher for in-memory data collection.
//! Stores data in a simple library-agnostic in-memory format.

use serde::{Deserialize, Serialize};
use std::{
    sync::{Arc, RwLock, RwLockWriteGuard},
    time::SystemTime,
};
use tracing::info;

#[cfg(feature = "python")]
use pyo3::prelude::*;

use crate::controller::context::ControllerCtx;
use crate::py_json_methods;

use super::{Dispatcher, Overflow, Row, fmt_time, header_columns};

#[derive(Serialize, Deserialize, Default, Debug)]
pub struct SimpleDataFrame {
    channel_names: Vec<String>,
    rows: Vec<Row>,
}

impl SimpleDataFrame {
    /// Make an empty dataframe that can hold `capacity` rows without reallocating.
    pub fn new(channel_names: Vec<String>, capacity: usize) -> Self {
        Self {
            channel_names,
            rows: Vec::with_capacity(capacity),
        }
    }

    /// Push a new row to the end of the list.
    pub fn push(&mut self, row: Row) {
        debug_assert!({
            if !self.rows.is_empty() {
                row.channel_values.len() == self.rows[0].channel_values.len()
            } else {
                true
            }
        });

        self.rows.push(row);
    }

    /// Place a new value at the target index, overwriting existing data.
    /// If the new index is out of bounds, pushes the new value to the end instead of crashing.
    pub fn put(&mut self, row: Row, loc: usize) {
        debug_assert!({
            if !self.rows.is_empty() {
                row.channel_values.len() == self.rows[0].channel_values.len()
            } else {
                true
            }
        });

        if loc >= self.len() {
            self.push(row);
        } else {
            self.rows[loc] = row;
        }
    }

    /// The current number of rows stored
    pub fn len(&self) -> usize {
        self.rows.len()
    }

    /// Get column header names including the two time columns
    pub fn headers(&self) -> Vec<String> {
        header_columns(&self.channel_names)
    }

    /// Read-only access to stored row data
    pub fn rows(&self) -> &[Row] {
        &self.rows
    }
}

/// Store collected data in in-memory columns, moving columns into
/// a dataframe behind a shared reference at termination.
///
/// To avoid deadlocks, the dataframe is not updated until after
/// the run is complete. The dataframe is cleared at the start of each run.
#[derive(Serialize, Deserialize, Default)]
#[cfg_attr(feature = "python", pyclass)]
pub struct DataFrameDispatcher {
    max_size_megabytes: usize,
    overflow_behavior: Overflow,
    #[serde(skip)]
    nrows: usize,
    #[serde(skip)]
    row_index: usize,
    #[serde(skip)]
    df: Arc<RwLock<SimpleDataFrame>>,
}

impl DataFrameDispatcher {
    /// Create a dispatcher that will clear the data in the dataframe
    /// at the start of a run, and store data in the dataframe at the
    /// end of the run.
    ///
    /// To prevent crashes, the stored data will occupy a fixed maximum size in RAM.
    /// Above that amount, additional writes will cause it to either wrap (overwriting
    /// data starting with the oldest) or produce an error. Producing a new chunk
    /// is not a valid overflow behavior for this dispatcher.
    pub fn new(
        max_size_megabytes: usize,
        overflow_behavior: Overflow,
        df: Option<Arc<RwLock<SimpleDataFrame>>>,
    ) -> (Self, Arc<RwLock<SimpleDataFrame>>) {
        // Check if the overflow behavior is valid
        match overflow_behavior {
            Overflow::Wrap => (),
            x => unimplemented!("Overflow behavior {x:?} is not available for DataFrameDispatcher"),
        }

        // Either use an existing dataframe handle, if it is provided,
        // or create a new empty one.
        let df = df.unwrap_or_default();
        let df_handle = df.clone();

        (
            Self {
                max_size_megabytes,
                overflow_behavior,
                df,
                ..Default::default()
            },
            df_handle,
        )
    }

    /// Get a write handle to the dataframe, if possible
    fn write(&self) -> Result<RwLockWriteGuard<'_, SimpleDataFrame>, String> {
        self.df
            .try_write()
            .map_err(|e| format!("Unable to lock dataframe: {e}"))
    }
}

py_json_methods!(
    DataFrameDispatcher,
    Dispatcher,
    #[new]
    fn py_new(max_size_megabytes: usize, overflow_behavior: Overflow) -> PyResult<Self> {
        let (dispatcher, _df_handle) = Self::new(max_size_megabytes, overflow_behavior, None);
        Ok(dispatcher)
    }
);

#[typetag::serde]
impl Dispatcher for DataFrameDispatcher {
    fn init(
        &mut self,
        _ctx: &ControllerCtx,
        channel_names: &[String],
        _core_assignment: usize,
    ) -> Result<(), String> {
        info!("Initializing dataframe dispatcher");
        // Store channel names
        let channel_names = channel_names.to_vec();

        // Reset current row
        self.row_index = 0;

        // Determine number of rows to store
        let time_size = fmt_time(SystemTime::now()).len();
        let row_size = time_size + (1 + channel_names.len()) * 8;
        self.nrows = (self.max_size_megabytes * 1024 * 1024) / row_size;

        // Make a fresh dataframe
        *self.write()? = SimpleDataFrame::new(channel_names, self.nrows);

        Ok(())
    }

    fn consume(
        &mut self,
        time: SystemTime,
        timestamp: i64,
        channel_values: Vec<f64>,
    ) -> Result<(), String> {
        // Store data
        let i = self.row_index;
        let row = Row {
            system_time: fmt_time(time),
            timestamp,
            channel_values,
        };
        self.df
            .write()
            .map_err(|e| format!("Failed to write dataframe row: {e}"))?
            .put(row, i);

        // Increment
        self.row_index += 1;

        // Handle overflow
        if self.row_index > self.nrows {
            match self.overflow_behavior {
                Overflow::Wrap => self.row_index = 0,
                Overflow::Error => return Err("DataFrame out of memory".to_string()),
                x => unimplemented!(
                    "Overflow behavior {x:?} is not available for DataFrameDispatcher"
                ),
            }
        }

        Ok(())
    }

    fn terminate(&mut self) -> Result<(), String> {
        // Clear state, keeping a handle to the same dataframe
        // so that we can run again if needed
        let (dispatcher, _df_handle) = Self::new(
            self.max_size_megabytes,
            self.overflow_behavior,
            Some(self.df.clone()),
        );
        (*self) = dispatcher;

        Ok(())
    }
}
