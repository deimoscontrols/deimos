# time-sync Specification

## Purpose

Defines three composable time-accuracy tiers — LOCAL (controller/peripheral phase alignment via period/phase deltas), GLOBAL (controller wall-clock to NTP), and TOTAL (sub-µs peripheral sample time to UTC, hardware-only) — plus the packet-arrival-timestamp dependency and observable metrics that make convergence verifiable.

## Requirements
### Requirement: Time synchronization is structured in three composable levels

Deimos SHALL provide time alignment between the controller and each peripheral via three composable levels:

1. **LOCAL** — each peripheral runs a closed-loop software adjustment that phase-aligns its sampling clock with the controller's cycle cadence.
2. **GLOBAL** — the controller references the host OS monotonic clock, disciplined over longer time scales by NTP.
3. **TOTAL** — the composition of LOCAL and GLOBAL yields the end-to-end alignment reported to users.

#### Scenario: All three levels are present in a running controller

- **WHEN** a controller drives a peripheral in steady-state operation
- **THEN** every operating-cycle packet from controller to peripheral MUST carry a period delta and phase delta (LOCAL), and every row emitted to dispatchers MUST carry a timestamp sourced from the controller's monotonic clock (GLOBAL)

<!-- REVIEW: The per-cycle dispatched `timestamp` is nanoseconds since the controller entered
     OperatingRoundtrip — a session-relative monotonic value, not a UNIX-epoch nanosecond
     timestamp. The spec says "sourced from the controller's monotonic clock", which is
     technically correct (it derives from `Instant::now()` via `start_of_operating.elapsed()`),
     but readers may expect a UNIX-epoch ns value (like what TimescaleDB's `timestamp` column
     normally receives). Maintainers should confirm whether dispatchers downstream
     (e.g. TimescaleDbDispatcher) correctly handle a session-relative ns value, or whether the
     intent is for `timestamp` to be a UNIX-epoch ns offset. The wall-clock `SystemTime::now()`
     is passed separately as `system_time`; which one downstream consumers use as the
     authoritative time axis is not currently specified here. -->

### Requirement: LOCAL level phase-aligns each peripheral via period/phase deltas

On every operating-cycle packet, the controller SHALL emit a per-peripheral period delta and phase delta that together steer the peripheral's sampling clock to align with the controller's cycle. The peripheral MUST consume those deltas to adjust its next-cycle timing. On a missed cycle (loss-of-contact), the requested phase delta SHALL be reset to zero and the period correction cleared — the loop freezes rather than extrapolating.

#### Scenario: Peripheral drift is corrected convergently

- **WHEN** the controller detects the peripheral's cycle is arriving off the nominal period
- **THEN** the next operating packet MUST carry a phase delta whose sign pulls the peripheral's phase toward alignment, such that the loop converges rather than diverges

<!-- REVIEW: The LOCAL loop computation is fully present and confirmed. The spec's sign
     convergence scenario cannot be verified from the Rust host side alone — whether a positive
     phase delta actually pulls the peripheral's phase *forward* (vs backward) depends on
     firmware-side interpretation of the delta. The spec correctly places this as a SHALL on
     the controller's output direction; the firmware's handling is a separate contract. -->

### Requirement: GLOBAL level uses host monotonic clock disciplined by NTP

The controller SHALL timestamp every row delivered to dispatchers using a clock derived from the host OS monotonic clock, not wall-clock system time, for ordering and spacing guarantees. Timestamps MUST be strictly monotonically non-decreasing and advance by exactly one cycle period per cycle, immune to NTP jumps. System wall-clock time MAY be recorded alongside (for human-readable logs) but MUST NOT be the source of truth for inter-row spacing.

#### Scenario: Wall-clock jumps mid-run

- **WHEN** the host's wall clock is adjusted by NTP mid-run
- **THEN** the monotonic timestamps delivered to dispatchers MUST remain monotonically non-decreasing and MUST NOT exhibit the jump

<!-- REVIEW: GLOBAL requirement confirmed with nuance. The monotonic `timestamp` is strictly
     monotonically increasing (it advances by exactly `dt_ns` per cycle), immune to NTP jumps.
     An ISO-8601 wall-clock string is recorded alongside for human readability. Worth clarifying
     that the monotonic value is nanoseconds since the start of OperatingRoundtrip, not since
     UNIX epoch. If TimescaleDB downstream expects a UNIX-epoch timestamp, the session-relative
     value will be interpreted as a time very close to 1970-01-01 (e.g., at 100 Hz for 1 hour
     = 3.6e12 ns ≈ epoch +1h) rather than the wall-clock time, which would silently produce
     incorrect time axes in dashboards. Recommend maintainers confirm downstream handling of
     session-relative vs epoch timestamps. -->

### Requirement: TOTAL alignment converges to sub-µs between modules without specialized hardware

The composition of LOCAL and GLOBAL SHALL converge to sub-microsecond alignment between modules on the same network within a few seconds of entering steady-state operation. Deimos MUST NOT require PTP hardware, a PTP root clock, a specialized switch, or any hardware timestamping to achieve this.

**Verification note:** This guarantee is a real-hardware contract and CANNOT be validated in HOOTL. HOOTL lacks an independent peripheral oscillator, the simulated peripheral discards period/phase deltas, and in-process channel arrival timestamps trivially track the target cycle midpoint — so the PID sees near-zero error regardless of convergence. Validation requires a real DAQ node with its own oscillator and a real network transport.

#### Scenario: Commodity switch deployment

- **WHEN** two DAQ nodes are connected to the controller via a commodity Ethernet switch with no PTP support
- **THEN** after a few seconds of steady-state operation the controller SHALL report a per-peripheral phase alignment tighter than 1 µs relative to its own cycle

<!-- REVIEW: TOTAL sub-µs phase-convergence cannot be exercised meaningfully in HOOTL —
     requires real hardware with separate clock sources (task 2.5, verified 2026-04-13).

     Three reasons HOOTL cannot exercise this claim:

     1. **No independent peripheral clock.** The HOOTL driver does not model a real MCU
        oscillator running at a slightly different frequency. Real peripheral hardware has its
        own crystal oscillator whose rate diverges from the host clock by 10–100 ppm; the LOCAL
        PID loop exists to correct that steady-state frequency offset. In HOOTL, the driver
        replies immediately over an in-process crossbeam channel — there is no autonomous clock
        to drift.

     2. **Period/phase deltas are silently discarded.** The HOOTL driver's operating branch
        checks only the packet size and extracts the cycle id; it does NOT parse period/phase
        deltas from the received bytes, and it does NOT introduce any timing adjustment in
        response. Even if the PID computed a nonzero correction, the simulated peripheral
        ignores it entirely.

     3. **Arrival timestamp is trivially accurate.** The in-process socket stamps
        `Instant::now()` immediately after channel dequeue. Because both the controller and
        driver share the same process clock with sub-microsecond in-process channel latency,
        the measured arrival time naturally tracks the controller's target cycle midpoint
        without any PID intervention. The PID sees near-zero error not because it has
        converged, but because there was never any real clock-domain error to correct.

     To exercise the TOTAL requirement, a test would need: (a) real hardware peripheral with
     its own oscillator, (b) a real network transport (UDP or Unix socket) introducing actual
     round-trip jitter, and (c) a clock-rate offset between the peripheral and the host that
     the PID must actively reduce. These properties are only present in a real-hardware or
     hardware-in-the-loop (HIL) test setup, not HOOTL.

     The spec claim ("sub-microsecond alignment … without specialized hardware") is plausible
     for the real system, but must be validated with a real DAQ node rather than HOOTL
     instrumentation. -->

<!-- REVIEW: Verifiable HOOTL proxy for TOTAL — the per-peripheral timing-delta channels
     dispatched each cycle (raw/filtered timing delta, requested period/phase delta) will show
     PID activity, but in HOOTL these values represent OS-scheduler noise rather than real
     clock-domain error. Typical HOOTL raw timing deltas are a few microseconds of
     thread-scheduling jitter rather than the ~100 ppm frequency error a real MCU would
     present. A HOOTL-based "test" that observes these deltas staying small would only confirm
     that in-process channel latency is low — it would not validate the TOTAL requirement as
     stated. -->

### Requirement: Packet arrival time drives the LOCAL loop

The LOCAL time-sync loop SHALL consume the arrival timestamp reported by the socket for each operating-cycle reply. The accuracy of that arrival timestamp is a requirement of the socket contract, not a best-effort — the sub-µs TOTAL guarantee is contingent on it. See the `transport-abstraction` capability's arrival-timestamp requirement.

#### Scenario: Inaccurate arrival times break LOCAL convergence

- **WHEN** a transport implementation reports arrival timestamps with millisecond-scale jitter
- **THEN** the LOCAL loop MUST NOT be expected to converge to sub-µs

<!-- REVIEW: Arrival-timestamp dependency confirmed. UDP and Unix sockets both stamp
     `Instant::now()` immediately after the `recv_from` syscall returns — post-syscall latency
     is the only source of noise, which is typically sub-µs on a lightly loaded host. The
     in-process HOOTL channel also stamps `Instant::now()` immediately after dequeue, so HOOTL
     timing accuracy is limited by OS scheduler granularity rather than network jitter —
     adequate for HOOTL testing, but not representative of real-hardware convergence behavior.
     The transport-abstraction dependency the spec claims is correctly mirrored by the Socket
     trait's "should be as accurate as possible" contract. No gaps found for this
     requirement. -->

### Requirement: Time-sync metrics are observable per cycle

The controller SHALL expose per-cycle timing state as part of the operating metrics written to dispatchers — at minimum a controller-side cycle-time margin and the peripheral-side cycle-time margin reported by each peripheral. Additional per-peripheral timing channels (raw and filtered timing delta, requested period and phase delta, loss-of-contact counter, cycle lag count) SHALL also be dispatched every cycle so that PID activity and link health are observable live.

#### Scenario: Operator spots a drifting peripheral

- **WHEN** a user streams the running controller's row output to a live dashboard
- **THEN** at least one channel MUST reflect the controller's cycle-time margin each cycle, so drift or saturation is visible without offline analysis

<!-- REVIEW: Observable metrics requirement confirmed. Two independent cycle-time margin
     channels are dispatched every cycle — a controller-side margin and a peripheral-side
     margin. Additional per-peripheral timing channels (raw/filtered timing delta, requested
     period/phase delta, loss-of-contact counter, cycle lag count) are also dispatched every
     cycle. The scenario is satisfied: a live dashboard will see the controller-side margin go
     to zero or negative when the controller is saturated, and the per-peripheral margin when
     the peripheral is saturated. No gaps found. -->
