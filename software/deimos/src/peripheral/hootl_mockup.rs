//! Peripheral wrapper that provides software-defined behavior with an internal state machine.

use std::collections::BTreeMap;
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime};

use serde::{Deserialize, Serialize};

use deimos_shared::OperatingMetrics;

#[cfg(feature = "python")]
use pyo3::prelude::*;

use crate::calc::Calc;
use crate::py_json_methods;

use super::Peripheral;

/// Peripheral wrapper that emits mock outputs using the ipc_mockup-style state machine.
#[derive(Serialize, Deserialize, Debug)]
#[cfg_attr(feature = "python", pyclass)]
pub struct HootlMockupPeripheral {
    inner: Box<dyn Peripheral>,
    configuring_timeout: Duration,
    end: Option<SystemTime>,
    #[serde(skip)]
    state: Mutex<MockState>,
}

impl HootlMockupPeripheral {
    pub fn new(
        inner: Box<dyn Peripheral>,
        configuring_timeout: Duration,
        end: Option<SystemTime>,
    ) -> Self {
        Self {
            inner,
            configuring_timeout,
            end,
            state: Mutex::new(MockState::default()),
        }
    }
}

py_json_methods!(
    HootlMockupPeripheral,
    Peripheral,
    #[new]
    #[pyo3(signature=(inner, configuring_timeout_ms=250, end_epoch_ns=None))]
    fn py_new(
        inner: Box<dyn Peripheral>,
        configuring_timeout_ms: u64,
        end_epoch_ns: Option<u64>,
    ) -> PyResult<Self> {
        let configuring_timeout = Duration::from_millis(configuring_timeout_ms);
        let end = match end_epoch_ns {
            Some(ns) => Some(
                SystemTime::UNIX_EPOCH
                    .checked_add(Duration::from_nanos(ns))
                    .ok_or_else(|| pyo3::exceptions::PyValueError::new_err("Invalid end_epoch_ns"))?,
            ),
            None => None,
        };
        Ok(Self::new(inner, configuring_timeout, end))
    },
    #[getter]
    fn serial_number(&self) -> u64 {
        self.inner.id().serial_number
    }
);

#[derive(Debug)]
struct MockState {
    mode: MockMode,
    counter: u64,
}

impl Default for MockState {
    fn default() -> Self {
        Self {
            mode: MockMode::Binding,
            counter: 0,
        }
    }
}

#[derive(Debug)]
enum MockMode {
    Binding,
    Configuring { start: Instant },
    Operating,
    Terminated,
}

impl MockState {
    fn advance(&mut self, now: SystemTime, timeout: Duration, end: Option<SystemTime>) {
        if end.map_or(false, |end| now > end) {
            self.mode = MockMode::Terminated;
            return;
        }

        let next = match std::mem::replace(&mut self.mode, MockMode::Terminated) {
            MockMode::Binding => MockMode::Configuring {
                start: Instant::now(),
            },
            MockMode::Configuring { start } => {
                if start.elapsed() > timeout {
                    MockMode::Binding
                } else {
                    self.counter = 0;
                    MockMode::Operating
                }
            }
            MockMode::Operating => MockMode::Operating,
            MockMode::Terminated => MockMode::Terminated,
        };

        self.mode = next;
    }
}

#[typetag::serde]
impl Peripheral for HootlMockupPeripheral {
    fn id(&self) -> deimos_shared::peripherals::PeripheralId {
        self.inner.id()
    }

    fn input_names(&self) -> Vec<String> {
        self.inner.input_names()
    }

    fn output_names(&self) -> Vec<String> {
        self.inner.output_names()
    }

    fn operating_roundtrip_input_size(&self) -> usize {
        self.inner.operating_roundtrip_input_size()
    }

    fn operating_roundtrip_output_size(&self) -> usize {
        self.inner.operating_roundtrip_output_size()
    }

    fn emit_operating_roundtrip(
        &self,
        id: u64,
        period_delta_ns: i64,
        phase_delta_ns: i64,
        inputs: &[f64],
        bytes: &mut [u8],
    ) {
        self.inner.emit_operating_roundtrip(id, period_delta_ns, phase_delta_ns, inputs, bytes);
    }

    fn parse_operating_roundtrip(&self, bytes: &[u8], outputs: &mut [f64]) -> OperatingMetrics {
        let mut metrics = self.inner.parse_operating_roundtrip(bytes, outputs);

        let mut state = match self.state.lock() {
            Ok(state) => state,
            Err(_) => return metrics,
        };
        state.advance(SystemTime::now(), self.configuring_timeout, self.end);

        match state.mode {
            MockMode::Operating => {
                for (idx, value) in outputs.iter_mut().enumerate() {
                    *value = state.counter as f64 + (idx as f64) * 0.01;
                }
                metrics.id = state.counter;
                state.counter = state.counter.wrapping_add(1);
            }
            MockMode::Binding | MockMode::Configuring { .. } | MockMode::Terminated => {
                for value in outputs.iter_mut() {
                    *value = 0.0;
                }
                metrics = OperatingMetrics::default();
            }
        }

        metrics
    }

    fn standard_calcs(&self, name: String) -> BTreeMap<String, Box<dyn Calc>> {
        self.inner.standard_calcs(name)
    }
}
