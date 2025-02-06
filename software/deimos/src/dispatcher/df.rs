//! Dataframe dispatcher for in-memory data collection

use polars::{prelude::*, series::Series};

use std::{
    sync::{Arc, RwLock, RwLockReadGuard, RwLockWriteGuard},
    time::SystemTime,
};

use core_affinity::CoreId;

use serde::{Deserialize, Serialize};

use crate::controller::context::ControllerCtx;

use super::{fmt_time, header_columns, Dispatcher, Overflow};

#[derive(Serialize, Deserialize, Default)]
pub struct DataFrameDispatcher {
    max_size_megabytes: usize,
    overflow_behavior: Overflow,

    #[serde(skip)]
    df: Arc<RwLock<DataFrame>>,
    #[serde(skip)]
    channel_names: Vec<String>,
    #[serde(skip)]
    nrows: usize,
    #[serde(skip)]
    row_index: usize,
    #[serde(skip)]
    cols: (Vec<String>, Vec<i64>, Vec<Vec<f64>>),
}

impl DataFrameDispatcher {
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

    fn write(&self) -> Result<RwLockWriteGuard<'_, DataFrame>, String> {
        self.df
            .write()
            .map_err(|_| "Unable to lock dataframe".to_string())
    }

    fn read(&self) -> Result<RwLockReadGuard<'_, DataFrame>, String> {
        self.df
            .read()
            .map_err(|_| "Unable to lock dataframe".to_string())
    }
}

#[typetag::serde]
impl Dispatcher for DataFrameDispatcher {
    fn init(
        &mut self,
        _ctx: &ControllerCtx,
        channel_names: &[String],
        _core_assignment: CoreId,
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

    fn reset(&mut self) -> Result<(), String> {
        // Store columns in dataframe
        let headers = header_columns(&self.channel_names);
        self.write()?
            .set_column_names(&headers)
            .map_err(|e| format!("Unable to set dataframe columns: {e}"))?;
        self.write()?
            .insert_column(0, Series::new((&headers[0]).into(), &self.cols.0))
            .map_err(|e| format!("Unable to store data in dataframe: {e}"))?;
        self.write()?
            .insert_column(1, Series::new((&headers[1]).into(), &self.cols.1))
            .map_err(|e| format!("Unable to store data in dataframe: {e}"))?;
        for j in 0..self.channel_names.len() {
            self.write()?
                .insert_column(
                    j + 2,
                    Series::new((&headers[j + 2]).into(), &self.cols.2[j]),
                )
                .map_err(|e| format!("Unable to store data in dataframe: {e}"))?;
        }

        // Clear state, keeping a handle to the same dataframe
        *self = Self::new(self.df.clone(), self.max_size_megabytes, self.overflow_behavior);
        
        // so that we can run again if needed
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
