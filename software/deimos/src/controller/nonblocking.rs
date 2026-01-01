//! Components to support nonblocking operation.
use super::{ManualInputMap, manual_inputs_default};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;

use std::collections::{BTreeSet, HashMap};
use std::sync::atomic::AtomicBool;

use crate::dispatcher::{
    Dispatcher, LatestValueDispatcher, LatestValueHandle, LowPassDispatcher, Row,
};
use crate::peripheral::PluginMap;

#[cfg(feature = "python")]
use pyo3::prelude::*;

#[cfg(feature = "python")]
use crate::python::BackendErr;

use tracing::{info, warn};

impl super::Controller {
    /// Run the control program on a separate thread and return a handle for coordination.
    ///
    /// `latest_value_cutoff_freq` enables an
    /// optional second-order Butterworth low-pass filter
    /// cutoff frequency to apply to latest-value data.
    /// If the selected frequency is outside the viable
    /// range for the filter, the cutoff frequency will
    /// be clamped to the viable bounds and a warning
    /// will be emitted.
    ///
    /// If `wait_for_ready` is true, block until the controller completes its first cycle.
    ///
    /// `plugins` provides a mechanism to register user-defined Peripheral objects.
    pub fn run_nonblocking(
        self,
        plugins: Option<PluginMap<'static>>,
        latest_value_cutoff_freq: Option<f64>,
        wait_for_ready: bool,
    ) -> Result<RunHandle, String> {
        let mut controller = self;
        controller.ready.reset();
        info!("Building nonblocking controller.");

        // Attach a latest-value dispatcher to expose live data.
        let existing_names = BTreeSet::from_iter(controller.dispatcher_names().clone());
        //   Make sure we don't overwrite an existing dispatcher name.
        let mut latest_name = "latest".to_string();
        if existing_names.contains(&latest_name) {
            let mut suffix = 1;
            loop {
                let candidate = format!("latest_{suffix}");
                if !existing_names.contains(&candidate) {
                    latest_name = candidate;
                    break;
                }
                suffix += 1;
            }
        }

        // Add the handle to get the latest values, possibly with a filter applied.
        let (latest_dispatcher, latest_handle) = LatestValueDispatcher::new();
        let latest_dispatcher: Box<dyn Dispatcher> = match latest_value_cutoff_freq {
            Some(cutoff_hz) => LowPassDispatcher::new(latest_dispatcher, cutoff_hz),
            None => latest_dispatcher,
        };
        controller.add_dispatcher(&latest_name, latest_dispatcher);

        // Set up machinery for interacting with the controller during operation.
        let termination_signal = Arc::new(AtomicBool::new(false));
        let manual_input_values = manual_inputs_default();
        controller.ctx.manual_inputs = manual_input_values.clone();
        let manual_input_names = controller.manual_input_names();

        // Make a handle to the termination signal.
        let term_for_thread = termination_signal.clone();
        let ready_for_thread = controller.ready.clone();
        let ready_for_handle = controller.ready.clone();

        // Run the controller on a separate thread.
        let handle = thread::Builder::new()
            .name("controller-run".to_string())
            .spawn(move || {
                let _ready_guard = ReadyFinishGuard {
                    ready: ready_for_thread,
                };
                // Run to completion.
                let result = controller.run(&plugins, Some(&*term_for_thread));

                // Remove the temporary latest-value handle when we're done.
                controller.remove_dispatcher(&latest_name);

                result
            })
            .expect("Failed to spawn controller thread");

        info!("Spawned nonblocking controller thread.");

        let mut run_handle = RunHandle {
            termination: termination_signal,
            latest: latest_handle,
            join: Some(handle),
            manual_input_values,
            manual_input_names,
            ready: ready_for_handle,
        };

        if !wait_for_ready {
            return Ok(run_handle);
        }

        info!("Waiting for controller thread to indicate readiness.");
        let ready_state = run_handle.ready.wait_ready_or_finished();
        if ready_state.ready {
            return Ok(run_handle);
        }

        warn!("Controller run did not indicate readiness.");
        // If we waited for readiness but the wait ended without readiness being indicated,
        // then the control program already ended.
        let err = match run_handle.join.take() {
            Some(handle) => match handle.join() {
                Ok(Ok(msg)) => format!("Controller exited before ready: {msg}"),
                Ok(Err(e)) => e,
                Err(e) => format!("Controller thread panicked: {e:?}"),
            },
            None => "Controller thread already joined or not started".to_string(),
        };

        Err(err)
    }
}

/// A snapshot of the values from all peripherals and calcs
/// at the end of a cycle.
#[cfg_attr(feature = "python", pyclass)]
#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    /// Cycle-end UTC system time in RFC3339 format with nanoseconds.
    pub system_time: String,

    /// [ns] Cycle-end time in nanoseconds since the start of run.
    pub timestamp: i64,

    /// Map of the latest readings from all peripherals and calcs.
    pub values: HashMap<String, f64>,
}

/// A handle to a [Controller] running via [Controller::run_nonblocking] that allows
/// reading and writing values from outside the control program during operation
/// and signaling the controller to shut down.
///
/// Signals the controller to shut down when dropped.
#[cfg_attr(feature = "python", pyclass)]
pub struct RunHandle {
    /// Signal to stop the controller.
    termination: Arc<AtomicBool>,

    /// Link to the running controller to get a snapshot of the output.
    latest: LatestValueHandle,

    /// Thread handle for the running controller.
    join: Option<std::thread::JoinHandle<Result<String, String>>>,

    /// Manual input values for peripherals to set.
    manual_input_values: ManualInputMap,

    /// Names of all peripheral inputs that can be set manually.
    manual_input_names: Vec<String>,

    /// Shared readiness signal from the controller.
    ready: Arc<ReadySignal>,
}

impl RunHandle {
    /// Signal the controller to stop.
    pub fn stop(&self) {
        self.termination
            .store(true, std::sync::atomic::Ordering::Relaxed);
    }

    /// Check if the controller thread is still running.
    pub fn is_running(&self) -> bool {
        self.join
            .as_ref()
            .map(|h| !h.is_finished())
            .unwrap_or(false)
    }

    /// Check if the controller has completed its first cycle.
    pub fn is_ready(&self) -> bool {
        self.ready.is_ready()
    }

    /// Wait for the controller thread to finish.
    pub fn join(&mut self) -> Result<String, String> {
        match self.join.take() {
            Some(h) => match h.join() {
                Ok(Ok(msg)) => Ok(msg),
                Ok(Err(e)) => Err(e),
                Err(e) => Err(format!("Controller thread panicked: {e:?}")),
            },
            None => Err("Controller thread already joined or not started".to_string()),
        }
    }

    /// Get the latest row: (system_time, timestamp, channel_values).
    pub fn latest_row(&self) -> (String, i64, Vec<f64>) {
        let row: Arc<Row> = self.latest.latest_row();
        (
            row.system_time.clone(),
            row.timestamp,
            row.channel_values.clone(),
        )
    }

    /// Column headers including timestamp/time.
    pub fn headers(&self) -> Vec<String> {
        self.latest.headers()
    }

    /// Read the latest row mapped to header names.
    pub fn read(&self) -> Snapshot {
        let headers = self.latest.headers();
        let row = self.latest.latest_row();
        let mut map = HashMap::new();
        // First two headers are timestamp, time.
        if headers.len() >= 2 {
            for (name, val) in headers.iter().skip(2).zip(row.channel_values.iter()) {
                map.insert(name.clone(), *val);
            }
        }
        Snapshot {
            system_time: row.system_time.clone(),
            timestamp: row.timestamp,
            values: map,
        }
    }

    /// List peripheral inputs that can be written manually.
    pub fn available_inputs(&self) -> Vec<String> {
        self.manual_input_names.clone()
    }

    /// Write values to peripheral inputs not driven by calcs.
    pub fn write(&self, values: HashMap<String, f64>) -> Result<(), String> {
        if !self.is_running() {
            return Err("Controller is not running".to_string());
        }

        if values.is_empty() {
            return Ok(());
        }

        let allowed = &self.manual_input_names;
        for name in values.keys() {
            if !allowed.contains(name) {
                return Err(format!(
                    "Manual input `{name}` is not available for manual writes"
                ));
            }
        }

        let mut manual_guard = self
            .manual_input_values
            .write()
            .map_err(|_| "Manual input map lock poisoned".to_string())?;
        for (name, value) in values {
            manual_guard.insert(name, value);
        }
        Ok(())
    }
}

impl Drop for RunHandle {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(feature = "python")]
#[pymethods]
impl RunHandle {
    #[pyo3(name = "stop")]
    fn py_stop(&self) {
        self.stop();
    }

    #[pyo3(name = "is_running")]
    fn py_is_running(&self) -> bool {
        self.is_running()
    }

    #[pyo3(name = "is_ready")]
    fn py_is_ready(&self) -> bool {
        self.is_ready()
    }

    #[pyo3(name = "join")]
    fn py_join(&mut self) -> PyResult<String> {
        self.join()
            .map_err(|e| PyErr::from(BackendErr::RunErr { msg: e }))
    }

    #[pyo3(name = "latest_row")]
    fn py_latest_row(&self) -> (String, i64, Vec<f64>) {
        self.latest_row()
    }

    #[pyo3(name = "headers")]
    fn py_headers(&self) -> Vec<String> {
        self.headers()
    }

    #[pyo3(name = "read")]
    fn py_read(&self) -> Snapshot {
        self.read()
    }

    #[pyo3(name = "available_inputs")]
    fn py_available_inputs(&self) -> Vec<String> {
        self.available_inputs()
    }

    #[pyo3(name = "write")]
    fn py_write(&self, values: HashMap<String, f64>) -> PyResult<()> {
        self.write(values)
            .map_err(|e| PyErr::from(BackendErr::RunErr { msg: e }))
    }

    fn __enter__(slf: PyRefMut<'_, Self>) -> PyResult<PyRefMut<'_, Self>> {
        Ok(slf)
    }

    fn __exit__(
        &mut self,
        _exc_type: Option<Py<PyAny>>,
        _exc: Option<Py<PyAny>>,
        _traceback: Option<Py<PyAny>>,
    ) -> PyResult<bool> {
        self.stop();
        Ok(false)
    }
}

#[cfg(feature = "python")]
#[pymethods]
impl Snapshot {
    #[getter]
    #[pyo3(name = "system_time")]
    fn py_system_time(&self) -> String {
        self.system_time.clone()
    }

    #[getter]
    #[pyo3(name = "timestamp")]
    fn py_timestamp(&self) -> i64 {
        self.timestamp
    }

    #[getter]
    #[pyo3(name = "values")]
    fn py_values(&self) -> HashMap<String, f64> {
        self.values.clone()
    }
}

/// Boolean predicates to support the ReadySignal condvar
/// because condvars can generate spurious wake signals.
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct ReadyState {
    pub(super) ready: bool,
    pub(super) finished: bool,
}

/// Classic predicate-signal pair for inter-thread signaling
/// using OS-scheduled condition variable.
#[derive(Debug, Default)]
pub(super) struct ReadySignal {
    pub(super) state: Mutex<ReadyState>,
    pub(super) cvar: Condvar,
}

impl ReadySignal {
    /// Clear predicates.
    pub(super) fn reset(&self) {
        let mut state = self.state.lock().unwrap();
        state.ready = false;
        state.finished = false;
    }

    /// Set ready predicate and signal condvar.
    pub(super) fn mark_ready(&self) {
        let mut state = self.state.lock().unwrap();
        if !state.ready {
            state.ready = true;
            self.cvar.notify_all();
        }
    }

    /// Set finished predicate and signal condvar.
    pub(super) fn mark_finished(&self) {
        let mut state = self.state.lock().unwrap();
        state.finished = true;
        self.cvar.notify_all();
    }

    /// Check ready predicate.
    pub(super) fn is_ready(&self) -> bool {
        self.state.lock().unwrap().ready
    }

    /// Wait for signal indicating the thread is either ready or already finished.
    /// Uses efficient-but-imprecise OS-scheduled waiting.
    pub(super) fn wait_ready_or_finished(&self) -> ReadyState {
        let mut state = self.state.lock().unwrap();
        while !state.ready && !state.finished {
            state = self.cvar.wait(state).unwrap();
        }
        *state
    }
}

/// Drop-guard to guarantee that the ready signal is marked
/// finished if the controller exits the loop for any reason.
pub(super) struct ReadyFinishGuard {
    pub(super) ready: Arc<ReadySignal>,
}

impl Drop for ReadyFinishGuard {
    fn drop(&mut self) {
        self.ready.mark_finished();
    }
}

pub(super) fn default_ready_signal() -> Arc<ReadySignal> {
    Arc::new(ReadySignal::default())
}
