//! Information about the current operation
//! that may be used by the controller's appendages.

use std::path::PathBuf;
use std::time::{Duration, SystemTime};
use std::{collections::BTreeMap, default::Default};

use super::channel::{Channel, Endpoint};
use crate::buffer_pool::BufferPool;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::ops::Deref;
use std::sync::{Arc, RwLock};

#[cfg(feature = "python")]
use pyo3::prelude::*;
#[cfg(feature = "python")]
use tracing::warn;

const DEFAULT_DISPATCHER_BUFFER_POOL_CAPACITY: usize = 8;

fn default_dispatcher_buffer_pool() -> BufferPool<Vec<f64>> {
    BufferPool::with_factory(DEFAULT_DISPATCHER_BUFFER_POOL_CAPACITY, Vec::new)
}

/// Criteria for exiting the control program
#[derive(Serialize, Deserialize, Clone, Debug)]
#[cfg_attr(feature = "python", pyclass)]
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
}

#[cfg(feature = "python")]
#[pymethods]
impl Termination {
    #[staticmethod]
    pub fn timeout_s(s: f64) -> Self {
        let duration_s = if s <= 0.0 {
            warn!("Termination timeout is zero or negative; clamping to zero.");
            0.0
        } else {
            s
        };
        Self::Timeout(Duration::from_secs_f64(duration_s))
    }

    #[staticmethod]
    pub fn scheduled_epoch_ns(ns: u64) -> PyResult<Self> {
        let when = SystemTime::UNIX_EPOCH
            .checked_add(Duration::from_nanos(ns))
            .ok_or_else(|| pyo3::exceptions::PyValueError::new_err("Invalid epoch nanoseconds"))?;
        Ok(Self::Scheduled(when))
    }
}

/// Response to losing contact with a peripheral
#[derive(Serialize, Deserialize, Clone, Debug)]
#[cfg_attr(feature = "python", pyclass)]
pub enum LossOfContactPolicy {
    /// Terminate the control program
    Terminate(),

    /// Attempt to reconnect until some time has elapsed,
    /// then terminate if unsuccessful. If no timeout is set,
    /// attempt reconnection indefinitely.
    Reconnect(Option<Duration>),
}

#[cfg(feature = "python")]
#[pymethods]
impl LossOfContactPolicy {
    #[staticmethod]
    pub fn terminate() -> Self {
        Self::Terminate()
    }

    #[staticmethod]
    pub fn reconnect_s(timeout_s: f64) -> Self {
        let duration = if timeout_s.is_sign_negative() {
            Duration::ZERO
        } else {
            Duration::from_secs_f64(timeout_s)
        };
        Self::Reconnect(Some(duration))
    }

    #[staticmethod]
    pub fn reconnect_indefinite() -> Self {
        Self::Reconnect(None)
    }
}

/// Whether to prioritize performance or efficiency in control loop.
///
/// When prioritizing performance, the control loop will consume 100% of time
/// on the first CPU in order to ensure timing accuracy.
///
/// When prioritizing efficiency, the control loop will rely on the operating system
/// to wake the thread when packets are received from peripherals, which drastically reduces
/// CPU usage, but degrades performance due to OS timing granularity and context-switching
/// overhead.
#[derive(Serialize, Deserialize, Clone, Debug)]
#[cfg_attr(feature = "python", pyclass)]
pub enum LoopMethod {
    /// Use 100% of CPU 0 to ensure cycle timing and prevent context switching.
    Performant,

    /// Use operating system scheduling to sleep until packets are received.
    ///
    /// Reduces CPU usage by roughly a factor of 100, but degrades
    /// performance at higher control rates (above about 100Hz).
    Efficient,
}

#[cfg(feature = "python")]
#[pymethods]
impl LoopMethod {
    #[staticmethod]
    pub fn performant() -> Self {
        Self::Performant
    }

    #[staticmethod]
    pub fn efficient() -> Self {
        Self::Efficient
    }
}

/// Operation context, provided to appendages during init
#[derive(Serialize, Deserialize, Clone, Debug)]
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

    /// Delay from end of configuration window after which
    /// the peripherals time out to the Operating state
    pub timeout_to_operating_ns: u32,

    /// Duration to wait for responses to binding
    pub binding_timeout_ms: u16,

    /// Duration for peripherals to wait to receive configuration
    /// after being bound
    pub configuring_timeout_ms: u16,

    /// Number of consecutive missed control inputs after which the peripheral
    /// should assume contact has been lost, de-energize their outputs, and wait
    /// to bind a controller again.
    pub peripheral_loss_of_contact_limit: u16,

    /// Number of consecutive missed responses from a peripheral
    /// after which the controller should assume contact has been lost
    pub controller_loss_of_contact_limit: u16,

    /// A condition for exiting the control loop, to be checked at each cycle
    pub termination_criteria: Option<Termination>,

    /// Response to losing contact with a peripheral
    pub loss_of_contact_policy: LossOfContactPolicy,

    /// Whether to prioritize performance or efficiency in the
    /// control loop.
    pub loop_method: LoopMethod,

    /// An escape hatch for sideloading user context (likely json-encoded)
    /// that is not yet implemented as a standalone field.
    pub user_ctx: BTreeMap<String, String>,

    /// An escape hatch for sideloading communication between appendages.
    /// Each channel is a bidirectional MPMC message pipe.
    ///
    /// Because bidirectional channels may not close until the program terminates
    /// on its own, the status of these channels should not be used to indicate
    /// when a freerunning thread should terminate, as this will often result in
    /// a resource leak.
    pub user_channels: Arc<RwLock<BTreeMap<String, Channel>>>,

    /// Shared buffer pool for dispatcher outputs.
    #[serde(skip, default = "default_dispatcher_buffer_pool")]
    pub dispatcher_buffer_pool: BufferPool<Vec<f64>>,
}

impl ControllerCtx {
    /// Get a handle to a source endpoint tx/rx pair for the channel,
    /// creating the channel if it does not exist.
    pub fn source_endpoint(&self, channel_name: &str) -> Endpoint {
        let map = &self.user_channels;
        let inner = map.deref();
        let mut writer = inner.try_write().unwrap();
        let channel = writer.entry(channel_name.to_owned()).or_default();
        channel.source_endpoint()
    }

    /// Get a handle to a sink endpoint tx/rx pair for the channel,
    /// creating the channel if it does not exist.
    pub fn sink_endpoint(&self, channel_name: &str) -> Endpoint {
        let map = &self.user_channels;
        let inner = map.deref();
        let mut writer = inner.try_write().unwrap();
        let channel = writer.entry(channel_name.to_owned()).or_default();
        channel.sink_endpoint()
    }
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
            timeout_to_operating_ns: 100_000_000,
            binding_timeout_ms: 10,
            configuring_timeout_ms: 20,
            peripheral_loss_of_contact_limit: 10,
            controller_loss_of_contact_limit: 10,
            termination_criteria: None,
            loss_of_contact_policy: LossOfContactPolicy::Terminate(),
            loop_method: LoopMethod::Performant,
            user_ctx: BTreeMap::new(),
            user_channels: Arc::new(RwLock::new(BTreeMap::new())),
            dispatcher_buffer_pool: default_dispatcher_buffer_pool(),
        }
    }
}
