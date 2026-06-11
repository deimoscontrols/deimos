//! Dispatcher wrapper that applies a low-pass filter before forwarding rows.

use std::time::SystemTime;

use serde::{Deserialize, Serialize};

#[cfg(feature = "python")]
use pyo3::prelude::*;

use deimos_numerics::{
    control::lti::butter,
    embedded::fixed::lti::{DeltaSos as FixedDeltaSos, DeltaSosState as FixedDeltaSosState},
};

use crate::controller::context::ControllerCtx;
use crate::py_json_methods;

use super::Dispatcher;

const MAX_CUTOFF_RATIO: f64 = 0.4;

type LowPassFilter = FixedDeltaSos<f64, 1, 1>;
type LowPassFilterState = FixedDeltaSosState<f64, 1, 1>;

/// Wraps another dispatcher and applies a 2nd order Butterworth low-pass filter per channel.
///
/// The cutoff frequency is clamped to a conservative upper ratio of the sample rate.
/// The filter is primed on the first row by setting each channel's steady-state
/// value to the incoming sample, avoiding a cold-start transient.
#[derive(Serialize, Deserialize)]
#[cfg_attr(feature = "python", pyclass)]
pub struct LowPassDispatcher {
    cutoff_hz: f64,
    inner: Box<dyn Dispatcher>,

    #[serde(skip)]
    filter: Option<LowPassFilter>,
    #[serde(skip)]
    filter_states: Vec<LowPassFilterState>,
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
            filter: None,
            filter_states: Vec::new(),
            primed: false,
            initialized: false,
        })
    }

    fn build_filter(&self, sample_rate_hz: f64) -> Result<LowPassFilter, String> {
        let cutoff_ratio = (self.cutoff_hz / sample_rate_hz).min(MAX_CUTOFF_RATIO);

        LowPassFilter::try_from(
            &butter::<2>(cutoff_ratio)
                .map_err(|e| format!("Failed to construct butter2 filter: {e}"))?,
        )
        .map_err(|e| format!("Failed to convert butter2 filter to fixed delta SOS: {e}"))
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
        let filter = self.build_filter(sample_rate_hz)?;
        self.filter_states = vec![filter.reset_state(); channel_names.len()];
        self.filter = Some(filter);
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
        let filter = self.filter.as_ref().ok_or_else(|| {
            "LowPassDispatcher must be initialized before consuming data".to_string()
        })?;

        if !self.primed {
            // On the first consume, use the value to initialize the filter.
            for (state, value) in self.filter_states.iter_mut().zip(values.iter()) {
                filter.set_steady_state(state, [*value]);
            }
            self.primed = true;
        } else {
            // After the first consume, update the filter and return the filtered value.
            for (state, value) in self.filter_states.iter_mut().zip(values.iter_mut()) {
                *value = filter.step(state, [*value])[0];
            }
        }

        self.inner.consume(time, timestamp, channel_values)
    }

    fn terminate(&mut self) -> Result<(), String> {
        let result = self.inner.terminate();
        self.filter = None;
        self.filter_states.clear();
        self.primed = false;
        self.initialized = false;
        result
    }
}
