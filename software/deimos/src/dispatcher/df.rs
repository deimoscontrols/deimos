//! Dataframe dispatcher for in-memory data collection

use polars::prelude::*;

use std::{
    sync::{Arc, RwLock, RwLockReadGuard, RwLockWriteGuard},
    time::SystemTime,
};
use serde::{Deserialize, Serialize};

use crate::controller::context::ControllerCtx;

use super::{Dispatcher, Overflow, fmt_time, header_columns};

/// Store collected data in in-memory columns, moving columns into
/// a dataframe behind a shared reference at termination.
///
/// To avoid deadlocks, the dataframe is not updated until after
/// the run is complete. The dataframe is cleared at the start of each run.
#[derive(Serialize, Deserialize)]
#[derive(Default)]
pub struct DataFrameDispatcher {
    max_size_megabytes: usize,
    overflow_behavior: Overflow,
    channel_names: Vec<String>,
    nrows: usize,
    row_index: usize,
    cols: (Vec<String>, Vec<i64>, Vec<Vec<f64>>),
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
        df: Arc<RwLock<DataFrame>>,
        max_size_megabytes: usize,
        overflow_behavior: Overflow,
    ) -> Self {
        // Check if the overflow behavior is valid
        match overflow_behavior {
            Overflow::Wrap => (),
            Overflow::Error => (),
            x => unimplemented!("Overflow behavior {x:?} is not available for DataFrameDispatcher"),
        }

        Self {
            max_size_megabytes,
            overflow_behavior,
            df,
            ..Default::default()
        }
    }

    /// Get a write handle to the dataframe, if possible
    fn write(&self) -> Result<RwLockWriteGuard<'_, DataFrame>, String> {
        self.df
            .try_write()
            .map_err(|_| "Unable to lock dataframe".to_string())
    }

    /// Get a read handle to the dataframe, if possible
    fn read(&self) -> Result<RwLockReadGuard<'_, DataFrame>, String> {
        self.df
            .try_read()
            .map_err(|_| "Unable to lock dataframe".to_string())
    }
}

#[typetag::serde]
impl Dispatcher for DataFrameDispatcher {
    fn init(
        &mut self,
        _ctx: &ControllerCtx,
        channel_names: &[String],
        _core_assignment: usize,
    ) -> Result<(), String> {
        // Store channel names for
        self.channel_names = channel_names.to_vec();

        // Clear dataframe
        *self.write()? = DataFrame::empty();
        assert!(self.read()?.is_empty());

        // Reset current row
        self.row_index = 0;

        // Determine number of rows to store
        let time_size = fmt_time(SystemTime::now()).bytes().len();
        let row_size = time_size + (1 + channel_names.len()) * 8;
        self.nrows = (self.max_size_megabytes * 1024 * 1024) / row_size;

        // Set column sizes
        self.cols = (
            Vec::with_capacity(self.nrows),
            Vec::with_capacity(self.nrows),
            vec![Vec::with_capacity(self.nrows); channel_names.len()],
        );

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
        put(&mut self.cols.0, fmt_time(time), i);
        put(&mut self.cols.1, timestamp, i);
        for j in 0..self.channel_names.len() {
            put(&mut self.cols.2[j], channel_values[j], i);
        }

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
        *self = Self::new(
            self.max_size_megabytes,
            self.overflow_behavior,
        );

        Ok(())
    }
}

/// Put a value at an index, or, if the index is out of bounds, push the value
fn put<T>(x: &mut Vec<T>, v: T, i: usize) {
    if i >= x.len() {
        x.push(v);
    } else {
        x[i] = v;
    }
}
