<!-- REVIEW-COMPLETE (task 3.3): approved-with-open-REVIEW-comments — TOTAL sub-µs claim is hardware-only (HOOTL cannot exercise it, honestly documented), and the session-relative vs UNIX-epoch timestamp ambiguity for TimescaleDB needs clarification; LOCAL and GLOBAL requirements are fully code-verified with no substantive gaps -->

## ADDED Requirements

<!-- REVIEW: Verification summary (task 1.6). All four cross-checks below are noted against the
     code as of this writing. No SHALL statement in this spec was found to be outright violated;
     the comments below document spec imprecision and one implementation nuance worth confirming
     with maintainers before this spec is marked evergreen. -->

### Requirement: Time synchronization is structured in three composable levels

Deimos SHALL provide time alignment between the controller and each peripheral via three composable levels:

1. **LOCAL** — each peripheral runs a closed-loop software adjustment that phase-aligns its sampling clock with the controller's cycle cadence.
2. **GLOBAL** — the controller references the host OS monotonic clock, disciplined over longer time scales by NTP.
3. **TOTAL** — the composition of LOCAL and GLOBAL yields the end-to-end alignment reported to users.

Reference: `docs/guide.md` (three-level sync description); `software/deimos/src/controller/timing.rs`; `software/deimos_shared/src/states/` (cycle id, period/phase deltas); firmware-side adjustment lives under `firmware/deimos_daq_rev7/` and siblings.

#### Scenario: All three levels are present in a running controller

- **WHEN** a controller drives a peripheral in `OperatingRoundtrip`
- **THEN** every operating-cycle packet sent from controller to peripheral MUST carry a `period_delta_ns` and a `phase_delta_ns` (LOCAL), and every row emitted to dispatchers MUST carry a timestamp sourced from the controller's monotonic clock (GLOBAL)

<!-- REVIEW: The `timestamp` value dispatched each cycle
     (`controller/mod.rs:1095`: `target_time.as_nanos() as i64`) is nanoseconds since the
     controller entered OperatingRoundtrip — a session-relative monotonic value, not a
     UNIX-epoch nanosecond timestamp. The spec says "sourced from the controller's monotonic
     clock", which is technically correct (it derives from `Instant::now()` via
     `start_of_operating.elapsed()`), but readers may expect a UNIX-epoch ns value (like what
     TimescaleDB's `timestamp` column normally receives). Maintainers should confirm whether
     dispatchers downstream (e.g. TimescaleDbDispatcher) correctly handle a session-relative
     ns value, or whether the intent is for `timestamp` to be a UNIX-epoch ns offset.
     The wall-clock `time: SystemTime::now()` is passed separately as `system_time` in
     `dispatcher/mod.rs`; which one downstream consumers use as the authoritative time axis
     is not currently specified here. -->

### Requirement: LOCAL level phase-aligns each peripheral via period/phase deltas

On every operating-cycle packet, the controller SHALL emit a per-peripheral `period_delta_ns` and `phase_delta_ns` that together steer the peripheral's sampling clock to align with the controller's cycle. The peripheral MUST consume those deltas to adjust its next-cycle timing.

Reference: `Peripheral::emit_operating_roundtrip(id, period_delta_ns, phase_delta_ns, inputs, bytes)` in `software/deimos/src/peripheral/mod.rs`.

#### Scenario: Peripheral drifts slow and is pulled forward

- **WHEN** the controller detects the peripheral's cycle is arriving later than the nominal period
- **THEN** the next operating packet MUST carry a `phase_delta_ns` whose sign pulls the peripheral's phase forward, such that the loop converges rather than diverges

<!-- REVIEW: The LOCAL loop computation is fully present and confirmed. `TimingPID` in
     `controller/timing.rs` splits its output into `(out_period, out_phase)` which become
     `requested_period_delta_ns` and `requested_phase_delta_ns` in `peripheral_state.rs`.
     These are cast to `i64` and passed directly to `emit_operating_roundtrip` each cycle
     (`controller/mod.rs:1378-1396`). The spec's sign convergence scenario cannot be verified
     from the Rust host side alone — whether `phase_delta_ns > 0` actually pulls the
     peripheral's phase *forward* (vs backward) depends on firmware-side interpretation of the
     delta. The spec correctly places this as a SHALL on the controller's output direction; the
     firmware's handling is a separate contract. One nuance: if a packet is missed
     (`loss_of_contact_counter > 0`), `requested_phase_delta_ns` is reset to 0 and the period
     correction is also cleared (`mod.rs:1493`). The spec is silent on this missed-packet
     behavior; maintainers may want to add a note that the loop freezes on a missed cycle
     rather than extrapolating. -->

### Requirement: GLOBAL level uses host monotonic clock disciplined by NTP

The controller SHALL timestamp every row delivered to dispatchers using a clock derived from the host OS monotonic clock, not wall-clock `SystemTime`, for ordering and spacing guarantees. System wall-clock time MAY be recorded in addition (for human-readable logs) but MUST NOT be the source of truth for inter-row spacing.

Reference: `Row { system_time, timestamp, channel_values }` in `software/deimos/src/dispatcher/mod.rs` (where `timestamp: i64` is monotonic-ns and `system_time: String` is wall-clock ISO-8601).

#### Scenario: Wall-clock jumps mid-run

- **WHEN** the host's wall clock is adjusted by NTP mid-run
- **THEN** the `timestamp` values delivered to dispatchers MUST remain monotonically non-decreasing and MUST NOT exhibit the jump

<!-- REVIEW: GLOBAL requirement confirmed with nuance. Each cycle, `time = SystemTime::now()`
     (wall clock) and `timestamp = target_time.as_nanos() as i64` (monotonic, session-relative)
     are computed at the top of the control loop (`controller/mod.rs:1090-1095`). The
     `timestamp` is strictly monotonically increasing (it advances by exactly `dt_ns` per
     cycle via `target_time += cycle_duration`), immune to NTP jumps. The `system_time` string
     (ISO-8601 UTC) is recorded alongside for human readability. This matches the spec.
     The spec says `timestamp: i64` is "monotonic-ns" — accurate but worth clarifying it is
     nanoseconds since the start of OperatingRoundtrip, not since UNIX epoch. If TimescaleDB
     downstream expects a UNIX-epoch timestamp, the session-relative value will be interpreted
     as a time very close to 1970-01-01 (e.g., at 100 Hz for 1 hour = 3.6e12 ns ≈ epoch +1h)
     rather than the wall-clock time, which would silently produce incorrect time axes in
     dashboards. Recommend maintainers confirm TimescaleDbDispatcher's handling of
     session-relative vs epoch timestamps. -->

### Requirement: TOTAL alignment converges to sub-µs between modules without specialized hardware

The composition of LOCAL and GLOBAL SHALL converge to sub-microsecond alignment between modules on the same network within a few seconds of entering `OperatingRoundtrip`. Deimos MUST NOT require PTP hardware, a PTP root clock, a specialized switch, or any hardware timestamping to achieve this.

Reference: `docs/guide.md` ("TOTAL: Together, these achieve sub-microsecond accuracy between modules in just a few seconds, without specialized hardware").

#### Scenario: Commodity switch deployment

- **WHEN** two DAQ nodes are connected to the controller via a commodity Ethernet switch with no PTP support
- **THEN** after a few seconds of steady-state operation the controller SHALL report a per-peripheral phase alignment tighter than 1 µs relative to its own cycle

<!-- REVIEW: TOTAL sub-µs phase-convergence cannot be exercised meaningfully in HOOTL —
     requires real hardware with separate clock sources (task 2.5, verified 2026-04-13).

     Three reasons HOOTL cannot exercise this claim:

     1. **No independent peripheral clock.** The HOOTL driver (`hootl.rs` `HootlRunner::run_loop`)
        does not model a real MCU oscillator running at a slightly different frequency. Real
        peripheral hardware has its own crystal oscillator whose rate diverges from the host clock
        by 10–100 ppm; the LOCAL PID loop exists to correct that steady-state frequency offset.
        In HOOTL, the driver replies immediately over an in-process crossbeam channel — there is
        no autonomous clock to drift.

     2. **Period/phase deltas are silently discarded.** The operating-state branch of
        `HootlRunner::run_loop` (`hootl.rs:567-594`) checks only the packet size and extracts the
        cycle id; it does NOT parse `period_delta_ns` or `phase_delta_ns` from the received bytes,
        and it does NOT introduce any timing adjustment in response. Even if the PID computed a
        nonzero correction, the simulated peripheral ignores it entirely.

     3. **Arrival timestamp is trivially accurate.** `ThreadChannelSocket::recv` stamps
        `Instant::now()` immediately after `try_recv()` returns (`thread_channel.rs:115`). Because
        both the controller and driver share the same process clock with sub-microsecond in-process
        channel latency, the measured arrival time naturally tracks the controller's target cycle
        midpoint (`tmean`) without any PID intervention. The PID sees near-zero error not because
        it has converged, but because there was never any real clock-domain error to correct.

     To exercise the TOTAL requirement, a test would need: (a) real hardware peripheral with its
     own oscillator, (b) a real network transport (UDP or Unix socket) introducing actual round-trip
     jitter, and (c) a clock-rate offset between the peripheral and the host that the PID must
     actively reduce. These properties are only present in a real-hardware or hardware-in-the-loop
     (HIL) test setup, not HOOTL.

     The spec claim ("sub-microsecond alignment … without specialized hardware") originates from
     `docs/guide.md` and is plausible for the real system, but must be validated with a real DAQ
     node rather than HOOTL instrumentation. -->

<!-- REVIEW: Verifiable HOOTL proxy for TOTAL — the per-peripheral channels dispatched each
     cycle (`raw_timing_delta_ns`, `filtered_timing_delta_ns`, `requested_period_delta_ns`,
     `requested_phase_delta_ns` — see `peripheral_state.rs:96-138`) will show PID activity, but
     in HOOTL these values represent OS-scheduler noise rather than real clock-domain error.
     Typical HOOTL `raw_timing_delta_ns` values are a few microseconds of thread-scheduling jitter
     rather than the ~100 ppm frequency error a real MCU would present. A HOOTL-based "test" that
     observes these deltas staying small would only confirm that in-process channel latency is
     low — it would not validate the TOTAL requirement as stated. -->


### Requirement: Packet arrival time drives the LOCAL loop

The LOCAL time-sync loop SHALL consume the arrival `Instant` reported by the socket for each operating-cycle reply. The accuracy of that `Instant` is a requirement of the socket contract, not a best-effort.

Reference: `SocketPacketMeta { time: Instant, ... }` in `software/deimos/src/socket/mod.rs`; see also the `transport-abstraction` capability's "Received packets carry arrival timestamps" requirement.

#### Scenario: Inaccurate arrival times break LOCAL convergence

- **WHEN** a transport implementation reports arrival `Instant` values with millisecond-scale jitter
- **THEN** the LOCAL loop MUST NOT be expected to converge to sub-µs; the sub-µs TOTAL guarantee is contingent on the socket's timestamp accuracy contract

<!-- REVIEW: Arrival-timestamp dependency confirmed. `SocketPacketMeta { time: Instant, ... }`
     (`socket/mod.rs:29-43`) carries the arrival time with doc comment "should be as accurate as
     possible". UDP (`udp.rs:260`) and Unix (`unix.rs:184`) both stamp `Instant::now()`
     immediately after the `recv_from` syscall returns — post-syscall latency is the only
     source of noise, which is typically sub-µs on a lightly loaded host. `thread_channel.rs:115`
     also stamps `Instant::now()` immediately after the in-process channel dequeue, so HOOTL
     timing accuracy is limited by OS scheduler granularity rather than network jitter — adequate
     for HOOTL testing, but not representative of real-hardware convergence behavior.
     The controller consumes `meta.time` at `mod.rs:521`:
     `ps.metrics.last_received_time_ns = (meta.time - start_of_operating).as_nanos() as i64`
     and feeds it into the PID at `mod.rs:1498`: `dt_err_i64_ns = tmean - last_received_time_ns`.
     The transport-abstraction dependency the spec claims is correctly mirrored by the Socket
     trait's "should be as accurate as possible" contract. No gaps found for this requirement. -->

### Requirement: Time-sync metrics are observable per cycle

The controller SHALL expose per-cycle timing state as part of the operating metrics written to dispatchers — at minimum a controller-side cycle-time margin and any peripheral-side metrics surfaced by `parse_operating_roundtrip` via `OperatingMetrics`.

Reference: `ControllerOperatingMetrics` with `cycle_time_margin_ns` in `software/deimos/src/controller/controller_state.rs`; `Peripheral::parse_operating_roundtrip` returning `OperatingMetrics` in `software/deimos/src/peripheral/mod.rs`.

#### Scenario: Operator spots a drifting peripheral

- **WHEN** a user streams the running controller's row output to a live dashboard
- **THEN** at least one channel MUST reflect the controller's cycle-time margin each cycle, so drift or saturation is visible without offline analysis

<!-- REVIEW: Observable metrics requirement confirmed. Two independent cycle-time margin
     channels are dispatched every cycle:
     1. `ctrl.cycle_time_margin_ns` — controller-side margin from `ControllerOperatingMetrics`
        (`controller_state.rs:19,31`); computed as `(target_time - elapsed).as_secs_f64() * 1e9`
        at `mod.rs:1098-1099`.
     2. `<name>.metrics.cycle_time_margin_ns` — peripheral-side margin from the `OperatingMetrics`
        struct returned by `parse_operating_roundtrip` (`deimos_shared/src/states/operating.rs:26`);
        written to dispatchers via `peripheral_state.rs:134`.
     Additional per-peripheral timing channels also dispatched every cycle: `raw_timing_delta_ns`,
     `filtered_timing_delta_ns`, `requested_period_delta_ns`, `requested_phase_delta_ns`,
     `loss_of_contact_counter`, `cycle_lag_count` (`peripheral_state.rs:96-138`).
     The spec's scenario is satisfied: a live dashboard will see at minimum `ctrl.cycle_time_margin_ns`
     go to zero or negative when the controller is saturated, and the per-peripheral
     `cycle_time_margin_ns` when the peripheral is saturated. No gaps found. -->
