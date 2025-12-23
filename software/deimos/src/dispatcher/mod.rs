//! Dispatchers send data to an outside consumer, usually a database or display

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::time::SystemTime;
mod tsdb;
pub use tsdb::TimescaleDbDispatcher;
mod df;
pub use df::DataFrameDispatcher;
mod latest;
pub use latest::{LatestValueDispatcher, LatestValueHandle, RowCell};
mod channel_filter;
pub use channel_filter::ChannelFilter;
mod decimation;
pub use decimation::DecimationDispatcher;
mod low_pass;
pub use low_pass::LowPassDispatcher;

mod csv;
pub use csv::CsvDispatcher;

use crate::buffer_pool::BufferLease;
use crate::controller::context::ControllerCtx;

#[cfg(feature = "python")]
use pyo3::prelude::*;

/// Choice of behavior when the current file is full
#[cfg_attr(feature = "python", pyclass)]
#[derive(Serialize, Deserialize, Default, Clone, Copy, Debug)]
pub enum Overflow {
    /// Wrap back to the beginning of the file and
    /// overwrite, starting with the oldest data
    #[default]
    Wrap,

    /// Create a new file
    NewFile,

    /// Error on overflow if neither wrapping nor creating a new file is viable
    Error,
}

#[derive(Serialize, Deserialize, Default, Debug)]
pub struct Row {
    pub system_time: String,
    pub timestamp: i64,
    pub channel_values: Vec<f64>,
}

/// A data pipeline plugin that receives data from the control loop
/// one row at a time.
#[typetag::serde(tag = "type")]
pub trait Dispatcher: Send + Sync {
    /// Set up the dispatcher at the start of a run
    fn init(
        &mut self,
        ctx: &ControllerCtx,
        channel_names: &[String],
        core_assignment: usize,
    ) -> Result<(), String>;

    /// Ingest a row of data
    fn consume(
        &mut self,
        time: SystemTime,
        timestamp: i64,
        channel_values: BufferLease<Vec<f64>>,
    ) -> Result<(), String>;

    /// Shut down the dispatcher and reset internal state for the next run
    fn terminate(&mut self) -> Result<(), String>;
}

/// Generate header strings including the time indices given some channel names
pub fn header_columns(channel_names: &[String]) -> Vec<String> {
    let mut out = vec!["timestamp".to_owned(), "time".to_owned()];
    out.extend(channel_names.iter().cloned());
    out
}

/// Generate CSV header row given some channel names
pub fn csv_header(channel_names: &[String]) -> String {
    let mut header_string = header_columns(channel_names).join(",");
    header_string.push('\n');
    header_string
}

/// Fixed-width ISO-8601 UTC timestamp with zero-padded sub-second nanoseconds and Z-suffix
pub fn fmt_time(time: SystemTime) -> String {
    DateTime::<Utc>::from(time).to_rfc3339_opts(chrono::SecondsFormat::Nanos, true)
}

/// Format a CSV row that guarantees fixed width for a given number of columns
pub fn csv_row_fixed_width(stringbuf: &mut String, vals: (SystemTime, i64, &[f64])) {
    stringbuf.clear();
    let (time, timestamp, channel_values) = vals;

    // This format guarantees fixed-width date format by zero-padding sub-second decimal
    let t_iso8601 = fmt_time(time);
    // Timestamp and floats need some effort to maintain fixed width
    let timestamp_fixed_width = fmt_i64(timestamp);
    stringbuf.extend(format!("{timestamp_fixed_width},{t_iso8601}").chars());
    for c in channel_values {
        stringbuf.push(',');
        stringbuf.push_str(&fmt_f64(*c));
    }
    stringbuf.push('\n');
}

/// Smaller-size and faster-eval CSV row that does not guarantee fixed width
pub fn csv_row(stringbuf: &mut String, vals: (SystemTime, i64, &[f64])) {
    stringbuf.clear();
    let (time, timestamp, channel_values) = vals;

    // This format guarantees fixed-width date format by zero-padding sub-second decimal
    let t_iso8601 = fmt_time(time);
    // Timestamp and floats need some effort to maintain fixed width
    stringbuf.extend(format!("{timestamp},{t_iso8601}").chars());
    for c in channel_values {
        stringbuf.push_str(&format!(",{}", *c));
    }
    stringbuf.push('\n');
}

/// Fixed-width formatting of float values
#[allow(clippy::manual_strip)]
pub fn fmt_f64(num: f64) -> String {
    let width = 0;
    let precision = 17;
    let exp_pad = 3;

    let prefix = match num {
        x if x >= 0.0 => "+",
        _ => "",
    };

    let mut numstr = format!("{prefix}{:.precision$e}", num, precision = precision);
    // Safe to `unwrap` as `num` is guaranteed to contain `'e'`
    let exp = numstr.split_off(numstr.find('e').unwrap());

    let (sign, exp) = if exp.starts_with("e-") {
        ('-', &exp[2..])
    } else {
        ('+', &exp[1..])
    };
    numstr.push_str(&format!("e{}{:0>pad$}", sign, exp, pad = exp_pad));

    format!("{:>width$}", numstr, width = width)
}

/// Fixed-width formatting of integer value for timestamp
/// 20 is the largest size.
pub fn fmt_i64(num: i64) -> String {
    let prefix = match num {
        x if x >= 0 => "+",
        _ => "",
    };
    format!("{prefix}{num:0>20}")
}
