//! Components to support nonblocking operation.
use std::sync::{Arc, Mutex, Condvar};



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
