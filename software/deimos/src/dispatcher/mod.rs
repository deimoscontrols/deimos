//! Dispatchers send data to an outside consumer, usually a database or display

use chrono::{DateTime, Utc};
use core_affinity::CoreId;
use std::time::SystemTime;

mod tsdb;
pub use tsdb::TimescaleDbDispatcher;

mod csv;
pub use csv::{CsvDispatcher, Overflow};

/// A data pipeline plugin that receives data from the control loop
/// one row at a time.
#[typetag::serde(tag = "type")]
pub trait Dispatcher: Send + Sync {
    fn initialize(
        &mut self,
        dt_ns: u32,
        channel_names: &Vec<String>,
        op_name: &str,
        core_assignment: CoreId,
    ) -> Result<(), String>;

    fn consume(
        &mut self,
        time: SystemTime,
        timestamp: i64,
        channel_values: Vec<f64>,
    ) -> Result<(), String>;
}

/// Generate header strings including the time indices given some channel names
pub fn header_columns(channel_names: &Vec<String>) -> Vec<String> {
    let mut out = vec!["timestamp".to_owned(), "time".to_owned()];
    out.extend(channel_names.iter().cloned());
    out
}

/// Generate CSV header row given some channel names
pub fn csv_header(channel_names: &Vec<String>) -> String {
    let cols = header_columns(channel_names);
    let mut header_string = String::new();
    let n = cols.len();
    for i in 0..n {
        header_string.push_str(&cols[i]);
        if i < n - 1 {
            header_string.push_str(",");
        } else {
            header_string.push_str("\n");
        }
    }
    header_string
}

/// Format a CSV row that guarantees fixed width for a given number of columns
pub fn csv_row_fixed_width(stringbuf: &mut String, vals: (SystemTime, i64, Vec<f64>)) {
    stringbuf.clear();
    let (time, timestamp, channel_values) = vals;

    // This format guarantees fixed-width date format by zero-padding sub-second decimal
    let t_iso8601 = DateTime::<Utc>::from(time).to_rfc3339_opts(chrono::SecondsFormat::Nanos, true);
    // Timestamp and floats need some effort to maintain fixed width
    let timestamp_fixed_width = fmt_i64(timestamp);
    stringbuf.extend(format!("{timestamp_fixed_width},{t_iso8601}").chars());
    for c in channel_values {
        stringbuf.push_str(",");
        stringbuf.extend(fmt_f64(c).chars());
    }
    stringbuf.extend("\n".chars());
}

/// Smaller-size and faster-eval CSV row that does not guarantee fixed width
pub fn csv_row(stringbuf: &mut String, vals: (SystemTime, i64, Vec<f64>)) {
    stringbuf.clear();
    let (time, timestamp, channel_values) = vals;

    // This format guarantees fixed-width date format by zero-padding sub-second decimal
    let t_iso8601 = DateTime::<Utc>::from(time).to_rfc3339_opts(chrono::SecondsFormat::Nanos, true);
    // Timestamp and floats need some effort to maintain fixed width
    stringbuf.extend(format!("{timestamp},{t_iso8601}").chars());
    for c in channel_values {
        stringbuf.push_str(&format!(",{c}"));
    }
    stringbuf.extend("\n".chars());
}

/// Fixed-width formatting of float values
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
