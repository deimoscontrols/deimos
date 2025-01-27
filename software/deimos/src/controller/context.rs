//! Information about the current operation
//! that may be used by the controller's appendages.

use std::default::Default;
use std::path::PathBuf;
use std::time::SystemTime;

use chrono::{DateTime, Utc};

use serde::{Deserialize, Serialize};

/// Operation context for
#[derive(Serialize, Deserialize, Clone, Debug)]
#[non_exhaustive]
pub struct ControllerCtx {
    /// Control cycle period in nanoseconds
    pub dt_ns: u32,

    /// Configuration time window, after which the peripherals time out to the Operating state
    pub timeout_to_operating_ns: u32,

    /// Number of consecutive missed control inputs after which the peripheral
    /// should assume contact has been lost, de-energize their outputs, and wait
    /// to bind a controller again.
    pub peripheral_loss_of_contact_limit: u16,

    /// Number of consecutive missed responses from a peripheral
    /// after which the controller should assume contact has been lost
    pub controller_loss_of_contact_limit: u16,

    /// A name for this controller op,
    /// which will be used as the name of the file/table/bucket/etc
    /// of each data dispatcher and must be compatible with that use.
    pub op_name: String,

    /// A directory to find file inputs and place outputs.
    pub op_dir: PathBuf,

    /// A last-resort escape hatch for sideloading
    /// (likely json-encoded) user context that is not yet implemented
    /// as a standalone field.
    pub user_ctx: Vec<String>,
}

impl Default for ControllerCtx {
    fn default() -> Self {
        // Use current time with seconds as op name and use working directory as op dir,
        // replacing characters in the name that would be invalid on Windows.
        let op_name = DateTime::<Utc>::from(SystemTime::now())
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
            .replace(":", "");
        Self {
            dt_ns: 0,
            timeout_to_operating_ns: 0,
            peripheral_loss_of_contact_limit: 10,
            controller_loss_of_contact_limit: 10,
            op_name,
            op_dir: std::fs::canonicalize("./").unwrap_or_default(),
            user_ctx: Vec::new(),
        }
    }
}
