# controller-orchestration Specification

## Purpose

Defines the controller's lifecycle state machine, self-healing semantics, run modes, loop pacing, and per-cycle ownership of sockets, peripherals, calcs, and dispatchers — the contract that ties the other capabilities together into a running Deimos session.

## Requirements
### Requirement: Controller progresses through a fixed lifecycle state machine

The controller SHALL advance each peripheral through the stages `Disconnected → Binding → Configuring → Operating` in that order on first reach. `Operating` is the only stage in which user data is sampled, calcs evaluate, and rows are produced for dispatchers.

#### Scenario: Fresh run reaches operation

- **WHEN** a run is started with a healthy transport and a reachable peripheral
- **THEN** the peripheral MUST traverse `Disconnected → Binding → Configuring → Operating` in that order before any row is produced for dispatchers

### Requirement: Controller self-heals on timeout or lost contact

If the controller loses contact with a peripheral or a stage times out, it SHALL return that peripheral to `Disconnected` and resume the lifecycle from the top without tearing down the whole run. Loss of one peripheral MUST NOT evict the remaining peripherals from `Operating`; the run continues and the lost peripheral's tape slots MAY hold last-valid values per the run-mode policy until it rejoins or the run exits for another reason.

#### Scenario: Peripheral drops mid-operation

- **WHEN** a peripheral stops replying during `Operating`
- **THEN** the controller MUST transition that peripheral back to `Disconnected` and attempt re-bind without aborting other healthy peripherals' operation

### Requirement: Controller supports operating a subset of a discovered fleet

During `Binding`, the controller MAY observe peripherals it does not intend to operate. Peripherals outside the configured set MUST be ignored silently (not an error); only configured peripherals SHALL be advanced to `Configuring`. Sharing one network segment between multiple controllers that each own a disjoint subset is a supported configuration.

#### Scenario: Shared network, selective operation

- **WHEN** two controllers share one Ethernet segment and each owns a different subset of peripherals
- **THEN** each controller MUST operate only its configured peripherals and MUST NOT error when it sees the others respond to broadcasts

#### Scenario: Expected peripheral missing from bind

- **WHEN** a configured peripheral is not observed during `Binding`
- **THEN** the run MUST fail with an error rather than silently proceeding, so the user learns about the missing peripheral instead of running a degraded experiment

### Requirement: Controller reports per-cycle timing margin as a metric

Each `Operating` cycle the controller SHALL compute cycle-time margin (nanoseconds of headroom between the cycle deadline and actual completion) and expose it as a channel named `ctrl.cycle_time_margin_ns` alongside peripheral metrics in the row delivered to dispatchers. The value MUST be finite every cycle; a negative value indicates the controller missed its deadline.

#### Scenario: Timing margin appears in every operating row

- **WHEN** a dispatcher records an operating run
- **THEN** every row MUST include a `ctrl.cycle_time_margin_ns` column with a finite value

### Requirement: Controller exposes blocking and non-blocking run modes

Deimos SHALL offer two run modes: a **blocking** mode that does not return until the run completes, and a **non-blocking** mode that returns a handle the caller can poll or signal for stop while other user code runs concurrently. Both modes MUST provide the termination guarantee described under "Controller owns the lifecycle" on normal exit.

#### Scenario: Non-blocking mode yields a drivable handle

- **WHEN** a caller starts a run via the non-blocking entry point
- **THEN** control MUST return immediately with a handle that can be polled for completion and signaled to stop, while the run executes concurrently

### Requirement: Controller supports configurable loop pacing strategies

The controller SHALL support both a high-rate busy-wait pacing strategy suitable for cycles up to ~20 kHz, and a low-CPU OS-scheduled pacing strategy suitable for cycles up to ~50 Hz. The active strategy is selected at run configuration time. The low-CPU strategy MUST NOT busy-wait between cycles, leaving CPU headroom for other processes on the same machine.

#### Scenario: High-rate control run meets cycle rate

- **WHEN** the high-rate pacing strategy is configured at 10 kHz on a machine meeting the documented performance requirements
- **THEN** the controller MUST achieve non-negative average cycle-time margin

### Requirement: Controller owns the lifecycle of sockets, peripherals, calcs, and dispatchers

The controller SHALL open every socket and initialize every calc and dispatcher before entering `Operating`. On normal run exit (stop signal, timeout, loss-of-contact escalation, reconnect timeout, or socket-worker error) the controller SHALL terminate every calc and dispatcher and close every socket before returning. Callers MUST NOT need to run cleanup themselves for the normal-exit paths.

#### Scenario: Clean shutdown on stop signal

- **WHEN** a user signals the controller to stop and the run exits normally
- **THEN** every calc and dispatcher MUST be terminated and every socket MUST be closed before the run returns

<!-- FOLLOW-ON: Two error-exit paths in `run()` currently skip `self.terminate()`:
  1. Dispatcher error (mod.rs ~lines 1551–1558): when a dispatcher returns `Err` from `consume`, the code calls `socket_orchestrator.close()` and returns `Err` without calling `self.terminate()`.
  2. Calc eval error (mod.rs ~lines 1525–1527): when `orchestrator.eval()` returns `Err`, the code also skips `self.terminate()`.
A future change proposal could align both paths with the other error-exits (loss-of-contact, reconnect timeout, termination signal, socket-worker error) that do call terminate. Not a gate on this bootstrap — the spec now describes the narrower guarantee the code actually provides. -->

