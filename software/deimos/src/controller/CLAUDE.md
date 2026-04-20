# Controller subsystem — non-obvious patterns

## Timing / time-sync

- **`timestamp` is session-relative, not UNIX epoch.** Each cycle, `timestamp = target_time.as_nanos() as i64` where `target_time` starts at `dt_ns` (one cycle after `start_of_operating`). A 100 Hz session running for 1 hour produces timestamps in the 3.6e12 ns range, which lands near epoch 1970-01-01T01:00 in UTC. Downstream consumers (e.g. `TimescaleDbDispatcher`) must handle session-relative values, not wall-clock epoch offsets. The wall-clock string (`system_time: String`) is passed separately. See `mod.rs:1090-1095`.

- **Timing error target is the cycle midpoint, not the cycle start.** `tmean = (target_time - cycle_duration / 2).as_nanos() as i64` — packet arrivals are driven toward the midpoint of each cycle window. See `mod.rs:1094`.

- **`TimingPID` splits its output into period and phase corrections.** `(out_period, out_phase) = c.update(error)`: `out_phase` = PD term (immediate phase nudge sent each cycle), `out_period` = I term (steady-state rate offset). The docstring explains the split: the peripheral can hold the rate correction across missed packets. See `timing.rs:28-39`.

- **On a missed packet, phase delta is zeroed but period delta is preserved.** `mod.rs:1490-1494`: if `loss_of_contact_counter > 0`, `requested_phase_delta_ns` is set to 0.0 and the PID is not updated — but `requested_period_delta_ns` retains its last value. The peripheral is expected to continue applying the stale period correction, keeping rate aligned even without a fresh phase nudge.

- **Two `cycle_time_margin_ns` channels are dispatched per cycle (controller + peripheral).** `ctrl.cycle_time_margin_ns` is the controller-side loop headroom (`controller_state.rs:19`). `<name>.metrics.cycle_time_margin_ns` is the peripheral-side headroom from `OperatingMetrics` returned by `parse_operating_roundtrip` (`peripheral_state.rs:134`). Both are dispatched to every registered dispatcher each cycle.

- **PID gains are hand-tuned and scale with `dt_ns`.** Gains at `mod.rs:1067-1076` are normalized to a 100 Hz reference period (10,000,000 ns). The comment tags this area as `FUTURE: could use more scrutiny`.

## Error conventions

- The operating loop returns `Err(String)` on socket worker failure or dispatcher errors; it does not panic. See `mod.rs:1474-1482`, `mod.rs:1551-1558`.
- `ControllerState::new` uses `assert!` (panics) to enforce that all expected peripherals were found in the bind scan — this is an invariant, not a recoverable error. See `controller_state.rs:63-67`.
