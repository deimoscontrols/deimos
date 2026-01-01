//! Dispatcher wrapper that applies a low-pass filter before forwarding rows.

use std::time::SystemTime;

use serde::{Deserialize, Serialize};

#[cfg(feature = "python")]
use pyo3::prelude::*;

use flaw::{
    SisoIirFilter, butter2,
    generated::butter::butter2::{MAX_CUTOFF_RATIO, MIN_CUTOFF_RATIO},
};

use crate::controller::context::ControllerCtx;
use crate::py_json_methods;

use super::Dispatcher;

/// Wraps another dispatcher and applies a 2nd order Butterworth low-pass filter per channel.
///
/// The cutoff frequency is clamped to a stable ratio of the sample rate.
/// The filter is primed on the first row by setting each channel's steady-state
/// value to the incoming sample, avoiding a cold-start transient.
#[derive(Serialize, Deserialize)]
#[cfg_attr(feature = "python", pyclass)]
pub struct LowPassDispatcher {
    cutoff_hz: f64,
    inner: Box<dyn Dispatcher>,

    #[serde(skip)]
    filters: Vec<SisoIirFilter<2>>,
    #[serde(skip)]
    primed: bool,
    #[serde(skip)]
    initialized: bool,
}

impl LowPassDispatcher {
    /// Create a low-pass wrapper with the requested cutoff frequency in Hz.
    pub fn new(inner: Box<dyn Dispatcher>, cutoff_hz: f64) -> Box<Self> {
        Box::new(Self {
            cutoff_hz,
            inner,
            filters: Vec::new(),
            primed: false,
            initialized: false,
        })
    }

    fn build_filters(
        &self,
        channel_count: usize,
        sample_rate_hz: f64,
    ) -> Result<Vec<SisoIirFilter<2>>, String> {
        let cutoff_ratio =
            (self.cutoff_hz / sample_rate_hz).clamp(MIN_CUTOFF_RATIO, MAX_CUTOFF_RATIO);

        let mut filters = Vec::with_capacity(channel_count);
        let filter_proto = butter2(cutoff_ratio)
            .map_err(|e| format!("Failed to construct butter2 filter: {e}"))?;
        for _ in 0..channel_count {
            filters.push(filter_proto.clone());
        }
        Ok(filters)
    }
}

py_json_methods!(
    LowPassDispatcher,
    Dispatcher,
    #[new]
    fn py_new(inner: Box<dyn Dispatcher>, cutoff_hz: f64) -> PyResult<Self> {
        Ok(*Self::new(inner, cutoff_hz))
    }
);

#[typetag::serde]
impl Dispatcher for LowPassDispatcher {
    fn init(
        &mut self,
        ctx: &ControllerCtx,
        channel_names: &[String],
        core_assignment: usize,
    ) -> Result<(), String> {
        if ctx.dt_ns == 0 {
            return Err("dt_ns must be > 0".to_string());
        }

        let sample_rate_hz = 1e9f64 / f64::from(ctx.dt_ns);
        self.filters = self.build_filters(channel_names.len(), sample_rate_hz)?;
        self.primed = false;

        let result = self.inner.init(ctx, channel_names, core_assignment);
        self.initialized = result.is_ok();
        result
    }

    fn consume(
        &mut self,
        time: SystemTime,
        timestamp: i64,
        mut channel_values: Vec<f64>,
    ) -> Result<(), String> {
        if !self.initialized {
            return Err("LowPassDispatcher must be initialized before consuming data".to_string());
        }

        // Update in-place and reuse the Vec to avoid reallocating
        let values = &mut channel_values[..];

        if !self.primed {
            // On the first consume, use the value to initialize the filter.
            for (filter, value) in self.filters.iter_mut().zip(values.iter()) {
                filter.set_steady_state(*value as f32);
            }
            self.primed = true;
        } else {
            // After the first consume, update the filter and return the filtered value.
            for (filter, value) in self.filters.iter_mut().zip(values.iter_mut()) {
                *value = filter.update(*value as f32) as f64;
            }
        }

        self.inner.consume(time, timestamp, channel_values)
    }

    fn terminate(&mut self) -> Result<(), String> {
        let result = self.inner.terminate();
        self.filters.clear();
        self.primed = false;
        self.initialized = false;
        result
    }
}
