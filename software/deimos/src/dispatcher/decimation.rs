//! Dispatcher wrapper that decimates rows before forwarding them.

use std::num::NonZeroUsize;
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

#[cfg(feature = "python")]
use pyo3::prelude::*;

use crate::controller::context::ControllerCtx;
use crate::py_json_methods;

use super::Dispatcher;

/// Wraps another dispatcher and forwards every Nth row.
#[derive(Serialize, Deserialize)]
#[cfg_attr(feature = "python", pyclass)]
pub struct DecimationDispatcher {
    nth: NonZeroUsize,
    inner: Box<dyn Dispatcher>,

    #[serde(skip)]
    remaining: usize,
    #[serde(skip)]
    initialized: bool,
}

impl DecimationDispatcher {
    pub fn new(inner: Box<dyn Dispatcher>, nth: NonZeroUsize) -> Box<Self> {
        Box::new(Self {
            nth,
            inner,
            remaining: 0,
            initialized: false,
        })
    }
}

py_json_methods!(
    DecimationDispatcher,
    Dispatcher,
    #[new]
    fn py_new(inner: Box<dyn Dispatcher>, nth: usize) -> PyResult<Self> {
        let nth = NonZeroUsize::new(nth).ok_or_else(|| {
            pyo3::exceptions::PyValueError::new_err("Decimation factor must be >= 1")
        })?;
        Ok(*Self::new(inner, nth))
    }
);

#[typetag::serde]
impl Dispatcher for DecimationDispatcher {
    fn init(
        &mut self,
        ctx: &ControllerCtx,
        channel_names: &[String],
        core_assignment: usize,
    ) -> Result<(), String> {
        self.remaining = 0;
        let result = self.inner.init(ctx, channel_names, core_assignment);
        self.initialized = result.is_ok();
        result
    }

    fn consume(
        &mut self,
        time: SystemTime,
        timestamp: i64,
        channel_values: Vec<f64>,
    ) -> Result<(), String> {
        if !self.initialized {
            return Err(
                "DecimationDispatcher must be initialized before consuming data".to_string(),
            );
        }

        if self.remaining == 0 {
            self.remaining = self.nth.get() - 1;
            self.inner.consume(time, timestamp, channel_values)
        } else {
            self.remaining -= 1;
            Ok(())
        }
    }

    fn terminate(&mut self) -> Result<(), String> {
        let result = self.inner.terminate();
        self.remaining = 0;
        self.initialized = false;
        result
    }
}
