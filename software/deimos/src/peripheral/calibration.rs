//! Storage and retrieval of calibration records.

use crate::fmt_time;
use serde::{Deserialize, Serialize};
use std::time::SystemTime;

pub const CURRENT_CAL_SCHEMA_VERSION: u16 = 1;

#[derive(Serialize, Deserialize, Debug, Default)]
pub struct Calibrator {
    pub make: String,
    pub model: String,
    pub serial: String,
}

/// Basic information to be included in all top-level calibration records.
#[derive(Serialize, Deserialize, Debug, Default)]
pub struct CalRecordCore {
    pub schema_version: u16,
    pub peripheral_kind: String,
    pub model_number: u64,
    pub serial_number: u64,
    pub procedure: String,
    pub procedure_version: u16,
    pub generated_at_utc: String,
    pub calibrators: Vec<Calibrator>,
}

impl CalRecordCore {
    pub fn new(
        peripheral_kind: impl Into<String>,
        model_number: u64,
        serial_number: u64,
        procedure: impl Into<String>,
        procedure_version: u16,
        calibrators: Vec<Calibrator>,
    ) -> Self {
        Self {
            schema_version: CURRENT_CAL_SCHEMA_VERSION,
            peripheral_kind: peripheral_kind.into(),
            model_number,
            serial_number,
            procedure: procedure.into(),
            procedure_version,
            generated_at_utc: fmt_time(SystemTime::now()),
            calibrators,
        }
    }
}
