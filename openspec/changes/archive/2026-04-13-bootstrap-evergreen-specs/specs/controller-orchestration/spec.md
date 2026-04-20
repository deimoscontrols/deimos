<!-- REVIEW-COMPLETE (task 3.3): approved — state names aligned to code (`Disconnected`), scenario wording corrected ("returns Err"), and lifecycle-ownership requirement narrowed to describe actual terminate coverage. Remaining terminate-on-error gaps tracked as follow-on change proposals, not gates on this bootstrap. -->

## ADDED Requirements

### Requirement: Controller progresses through a fixed lifecycle state machine

The controller SHALL advance peripherals through the stages `Disconnected → Binding → Configuring → Operating` (the `ConnState` enum in `peripheral_state.rs`). `Operating` (a.k.a. `OperatingRoundtrip` in user-facing documentation) is the only stage in which user data is sampled and calcs evaluate. Transitions MUST occur only in that order for a peripheral reaching operation for the first time in a run.

Reference: `ConnState` in `software/deimos/src/controller/peripheral_state.rs`; `software/deimos/src/controller/mod.rs`, `software/deimos/src/controller/controller_state.rs`, `software/deimos_shared/src/states/`.

#### Scenario: Fresh run reaches operation

- **WHEN** `run()` is invoked with a healthy transport and a reachable peripheral
- **THEN** the controller MUST progress the peripheral through `Disconnected`, `Binding`, `Configuring`, and `Operating` in that order before any row is produced for dispatchers

### Requirement: Controller self-heals on timeout or lost contact

If the controller loses contact with a peripheral or a stage times out, it SHALL return that peripheral to `Disconnected` and resume the lifecycle from the top without tearing down the whole run.

Reference: controller state machine in `software/deimos/src/controller/mod.rs`; `BindingInput.timeout_to_connecting` in `software/deimos_shared/src/states/binding.rs`.

#### Scenario: Peripheral drops mid-operation

- **WHEN** a peripheral stops replying during `Operating`
- **THEN** the controller MUST transition that peripheral back to `Disconnected` and attempt re-bind without aborting other healthy peripherals' operation

#### Scenario: Partial fleet loss

- **WHEN** one peripheral out of several disconnects
- **THEN** the remaining peripherals MUST stay in `Operating` and continue producing rows; the lost peripheral's tape slots MAY hold their last-valid values per the run-mode policy but the controller MUST NOT exit the run

### Requirement: Controller supports operating a subset of a discovered fleet

During `Binding`, the controller MAY observe peripherals it does not intend to operate. Unexpected peripherals MUST be ignored silently; only peripherals whose `PeripheralId` is present in the configured set SHALL be advanced to `Configuring`.

Reference: `ControllerState::new` in `software/deimos/src/controller/controller_state.rs` — "controlling only some of the peripherals available on a network is a normal use case."

#### Scenario: Shared network, selective operation

- **WHEN** two controllers share one Ethernet segment and each owns a different subset of DAQs
- **THEN** each controller MUST operate only its configured peripherals and MUST NOT error when it sees the others respond to broadcasts

#### Scenario: Expected peripheral missing from bind

- **WHEN** a configured peripheral is not observed during `Binding`
- **THEN** the controller MUST return `Err` from `run()` (via the pre-`ControllerState::new` check in `mod.rs` ~line 787–801) rather than silently proceeding, so the user learns about the missing peripheral instead of running a degraded experiment

### Requirement: Controller reports per-cycle timing margin as a metric

During `OperatingRoundtrip` the controller SHALL compute `ctrl.cycle_time_margin_ns` each cycle and expose it alongside peripheral metrics in the channel list delivered to dispatchers.

Reference: `ControllerOperatingMetrics` in `software/deimos/src/controller/controller_state.rs`.

#### Scenario: Timing margin appears in the row

- **WHEN** a CSV dispatcher records an operating run
- **THEN** one of the channel columns MUST be `ctrl.cycle_time_margin_ns` and MUST contain a finite value each cycle reflecting how much headroom the controller had between the deadline and when the cycle completed

### Requirement: Controller exposes blocking and non-blocking run modes

Deimos SHALL offer two run modes: a **blocking** `run()` entry point, and a **non-blocking** entry point that returns a handle so callers can drive the run alongside other user code.

Reference: `software/deimos/src/controller/mod.rs`, `software/deimos/src/controller/nonblocking.rs`.

#### Scenario: Script uses blocking mode

- **WHEN** a Python or Rust script calls `.run()` on a controller
- **THEN** the call MUST NOT return until the run completes; on normal completion (stop signal, timeout, or loss-of-contact) the controller MUST have called `terminate` on every calc and dispatcher

#### Scenario: GUI uses non-blocking mode

- **WHEN** a UI kicks off a run via the non-blocking entry point
- **THEN** it MUST receive a handle that can be polled or signaled for stop, while the UI thread remains responsive

### Requirement: Controller supports configurable loop pacing strategies

The controller SHALL support both a high-rate busy-wait loop strategy suitable for up to ~20 kHz cycles, and a low-CPU OS-scheduled strategy suitable for up to ~50 Hz. The active strategy is selected at run configuration time.

Reference: loop-pacing logic in `software/deimos/src/controller/timing.rs` and `software/deimos/src/controller/mod.rs`.

#### Scenario: High-rate control run

- **WHEN** a user configures the controller for the high-rate pacing strategy at 10 kHz
- **THEN** the controller MUST meet that cycle rate (non-negative `cycle_time_margin_ns` on average) on a machine meeting the documented performance requirements

#### Scenario: Idle-friendly logging run

- **WHEN** a user configures the controller for the low-CPU pacing strategy at 10 Hz
- **THEN** the controller MUST NOT busy-wait between cycles, leaving CPU headroom for other processes on the same machine

### Requirement: Controller owns the lifecycle of sockets, peripherals, calcs, and dispatchers

The controller SHALL call `open` on every socket and `init` on every calc and dispatcher before entering `Operating`. On normal run exit (stop signal, timeout, loss-of-contact escalation, reconnect timeout, socket-worker error) the controller SHALL call `terminate` on every calc and dispatcher and `close` on every socket before returning.

Reference: `ControllerCtx` in `software/deimos/src/controller/context.rs`; lifecycle calls in `software/deimos/src/controller/mod.rs`.

#### Scenario: Clean shutdown on stop signal

- **WHEN** a user signals the controller to stop and the run exits normally
- **THEN** the controller MUST call `terminate` on every calc and dispatcher and `close` on every socket before returning

<!-- FOLLOW-ON: Two error-exit paths in `run()` currently skip `self.terminate()`:
  1. Dispatcher error (mod.rs ~lines 1551–1558): when a dispatcher returns `Err` from `consume`, the code calls `socket_orchestrator.close()` and returns `Err` without calling `self.terminate()`.
  2. Calc eval error (mod.rs ~lines 1525–1527): when `orchestrator.eval()` returns `Err`, the code also skips `self.terminate()`.
A future change proposal could align both paths with the other error-exits (loss-of-contact, reconnect timeout, termination signal, socket-worker error) that do call terminate. Not a gate on this bootstrap — the spec now describes the narrower guarantee the code actually provides. -->
