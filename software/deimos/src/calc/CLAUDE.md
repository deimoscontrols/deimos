# Calc subsystem — non-obvious patterns

## Error conventions: assert! vs Result

- `Butter2::init()` and `Pid::init()` use `assert!(dt_ns > 0)` rather than returning `Err` for
  the precondition check. A zero or negative dt_ns panics the controller process.
  Source: `butter.rs` (init), `pid.rs` (init).
- This is a fail-fast pattern, consistent with `controller_state.rs:63-67` (which uses `assert!`
  for "all expected peripherals found"). It diverges from the `Result<(), String>` contract that
  the rest of `init()` uses. Do not assume all `init()` failures surface as `Err`.

## Butter2 uses f64::NAN as a sentinel, not Result

- `Butter2::eval()` returns `f64::NAN` when the input is `NAN` or when internal state is
  uninitialized (before the first sample). There is no `Result` wrapping.
  Source: `butter.rs`.
- Downstream calcs that consume `Butter2` output must handle NAN propagation explicitly.
  The calc graph does not short-circuit on NAN.

## State resets across restarts (terminate → init → eval)

- `Butter2::terminate()` resets both `filt` and `initialized` to their zero/false defaults.
  `init()` sets `initialized = false` again. Steady-state initialization fires on the first
  `eval()` call of every new run.
  Source: `butter.rs:116` (steady-state init on first eval), task 2.3 verification.
- The HOOTL driver resets its internal counter to 0 each time it enters Operating state, so
  both back-to-back runs see the same input sequence if the filter starts fresh. Bit-for-bit
  identical leading output was confirmed in `examples/hootl_butter_reset.rs`.

## update_input_map has no post-init guard

- `Butter2::update_input_map()` and `Pid::update_input_map()` mutate the stored name field
  unconditionally. If called after `init()`, the stored name and the resolved tape index become
  out of sync with no error returned. There is no enforcement that `update_input_map` is
  called only before `init`.
  Source: `butter.rs`, `pid.rs`. See `<!-- REVIEW: -->` in `specs/realtime-calc/spec.md`.

## Calc trait contract: eval is synchronous, no timeout guard

- `CalcOrchestrator::eval()` (`orchestrator.rs`) calls each calc's `eval()` synchronously in
  the controller loop. A calc that blocks or panics stalls or terminates the entire run.
  The "MUST NOT block" requirement is a convention enforced by documentation, not by a
  runtime deadline. See `<!-- REVIEW: -->` in `specs/realtime-calc/spec.md`.
