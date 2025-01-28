//! Information about the current operation
//! that may be used by the controller's appendages.

use std::{collections::BTreeMap, default::Default};
use std::path::PathBuf;
use std::time::{SystemTime, Duration};

use chrono::{DateTime, Utc};

use serde::{Deserialize, Serialize};

/// Criteria for exiting the control program
#[derive(Serialize, Deserialize)]
#[non_exhaustive]
pub enum Termination {
    /// A duration after which the control program should terminate.
    /// The controller will use the monotonic clock, not system realtime clock,
    /// to determine when this threshold occurs; as a result, if used for
    /// scheduling relative to a "realtime" date or time, it will accumulate
    /// some error as the monotonic clock drifts.
    Timeout(Duration),

    /// Schedule termination at a specific "realtime" date or time.
    Scheduled(SystemTime),

    /// Terminate on any nonzero output of this calc output.
    Calc(String)
}

/// Response to losing contact with a peripheral
#[derive(Serialize, Deserialize)]
#[non_exhaustive]
pub enum LossOfContactPolicy {
    /// Terminate the control program
    Terminate,

    // TODO: implement reconnection
    // /// Attempt to reconnect indefinitely
    // Reconnect,

    // /// Attempt to reconnect until some time has elapsed,
    // /// then terminate if unsuccessful
    // ReconnectWithTimeout(Duration)
}

/// Operation context for
#[derive(Serialize, Deserialize)]
#[non_exhaustive]
pub struct ControllerCtx {
    /// A name for this controller op,
    /// which will be used as the name of the file/table/bucket/etc
    /// of each data dispatcher and must be compatible with that use.
    pub op_name: String,

    /// A directory to find file inputs and place outputs.
    pub op_dir: PathBuf,

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

    /// A set of conditions for exiting the control loop, to be checked at each cycle
    pub termination_criteria: Vec<Termination>,

    /// Response to losing contact with a peripheral
    pub loss_of_contact_policy: LossOfContactPolicy,

    /// A last-resort escape hatch for sideloading user context (likely json-encoded)
    /// that is not yet implemented as a standalone field.
    pub user_ctx: BTreeMap<String, String>,
}

impl Default for ControllerCtx {
    fn default() -> Self {
        // Use current time with seconds as op name and use working directory as op dir,
        // replacing characters in the name that would be invalid on some platforms.
        let op_name = DateTime::<Utc>::from(SystemTime::now())
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
            .replace(":", "");
        Self {
            op_name,
            op_dir: std::fs::canonicalize("./").unwrap_or_default(),
            dt_ns: 0,
            timeout_to_operating_ns: 0,
            peripheral_loss_of_contact_limit: 10,
            controller_loss_of_contact_limit: 10,
            termination_criteria: Vec::new(),
            loss_of_contact_policy: LossOfContactPolicy::Terminate,
            user_ctx: BTreeMap::new(),
        }
    }
}
