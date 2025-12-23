//! Dispatcher wrapper that filters channel data before forwarding it.

use std::collections::{HashMap, HashSet};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

#[cfg(feature = "python")]
use pyo3::prelude::*;

use crate::buffer_pool::{BufferLease, BufferPool};
use crate::controller::context::ControllerCtx;
use crate::py_json_methods;

use super::Dispatcher;

/// Wraps another dispatcher and forwards a subset of channels by name.
#[derive(Serialize, Deserialize)]
#[cfg_attr(feature = "python", pyclass)]
pub struct ChannelFilter {
    channels: Vec<String>,
    inner: Box<dyn Dispatcher>,

    #[serde(skip)]
    indices: Vec<usize>,
    #[serde(skip)]
    initialized: bool,
    #[serde(skip)]
    buffer_pool: Option<BufferPool<Vec<f64>>>,
}

impl ChannelFilter {
    pub fn new(inner: Box<dyn Dispatcher>, channels: Vec<String>) -> Self {
        Self {
            channels,
            inner,
            indices: Vec::new(),
            initialized: false,
            buffer_pool: None,
        }
    }

    fn build_indices(&self, channel_names: &[String]) -> Result<Vec<usize>, String> {
        let mut lookup: HashMap<&str, usize> = HashMap::new();
        for (idx, name) in channel_names.iter().enumerate() {
            lookup.insert(name.as_str(), idx);
        }

        let mut seen: HashSet<&str> = HashSet::new();
        for name in &self.channels {
            if !seen.insert(name.as_str()) {
                return Err(format!("Channel filter contains duplicate name '{name}'"));
            }
        }

        let mut indices = Vec::with_capacity(self.channels.len());
        for name in &self.channels {
            let idx = lookup
                .get(name.as_str())
                .copied()
                .ok_or_else(|| format!("Channel filter requested missing channel '{name}'"))?;
            indices.push(idx);
        }

        Ok(indices)
    }
}

py_json_methods!(
    ChannelFilter,
    Dispatcher,
    #[new]
    fn py_new(inner: Box<dyn Dispatcher>, channels: Vec<String>) -> PyResult<Self> {
        Ok(Self::new(inner, channels))
    }
);

#[typetag::serde]
impl Dispatcher for ChannelFilter {
    fn init(
        &mut self,
        ctx: &ControllerCtx,
        channel_names: &[String],
        core_assignment: usize,
    ) -> Result<(), String> {
        let indices = self.build_indices(channel_names)?;
        let filtered_names = indices
            .iter()
            .map(|idx| channel_names[*idx].clone())
            .collect::<Vec<_>>();

        self.indices = indices;
        self.initialized = true;
        self.buffer_pool = Some(ctx.dispatcher_buffer_pool.clone());

        self.inner.init(ctx, &filtered_names, core_assignment)
    }

    fn consume(
        &mut self,
        time: SystemTime,
        timestamp: i64,
        channel_values: BufferLease<Vec<f64>>,
    ) -> Result<(), String> {
        if !self.initialized {
            return Err("ChannelFilter must be initialized before consuming data".to_string());
        }

        let pool = self
            .buffer_pool
            .as_ref()
            .ok_or_else(|| "ChannelFilter must be initialized before consuming data".to_string())?;
        let mut filtered = pool.lease_or_create(Vec::new);
        let buf = filtered.as_mut();
        buf.clear();
        buf.reserve(self.indices.len());
        for &idx in &self.indices {
            let value = channel_values.as_ref().get(idx).copied().ok_or_else(|| {
                format!(
                    "Channel filter index {idx} out of bounds for {} values",
                    channel_values.as_ref().len()
                )
            })?;
            buf.push(value);
        }

        self.inner.consume(time, timestamp, filtered)
    }

    fn terminate(&mut self) -> Result<(), String> {
        let result = self.inner.terminate();
        self.indices.clear();
        self.initialized = false;
        self.buffer_pool = None;
        result
    }
}
