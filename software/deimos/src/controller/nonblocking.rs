//! Components to support nonblocking operation.
use super::{RunHandle, manual_inputs_default};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;

use std::collections::BTreeSet;
use std::sync::atomic::AtomicBool;

use crate::dispatcher::{Dispatcher, LatestValueDispatcher, LowPassDispatcher};
use crate::peripheral::PluginMap;

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
