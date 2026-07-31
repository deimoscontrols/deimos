# Rev7 Phase 6 cycle-owned sampling report

Date: 2026-07-30

> **Intermediate characterization:** The 33 kHz/3x/1x topology described below
> was used to establish the timing feasibility of synchronous sampling. It has
> since been superseded by the current Phase 6 plan and implementation:
> rounded-N synchronous oversampling targeting 9 kHz below 3 kHz, and direct 1x
> synchronous sampling at and above 3 kHz. The measurements remain useful as
> historical timing evidence but are not a characterization of the final
> rounded-N implementation.

## Superseding implementation verification

The rounded-N implementation builds and passes target-independent scheduling
tests, including the 4 Hz case (`2,250` samples/cycle), nearest-integer boundary
selection, exact quotient/remainder tick sums, and a primed steady-state ADC
filter at the 4 Hz cutoff ratio. Firmware and filter analysis now use the same
shared cycle-rate policy. Because sampling and publication share one SysTick
owner, the obsolete ADC double buffer and ADC/counter/frequency atomics were
removed; counters now accumulate directly into sampler-owned `i64` values.

The release stack report shows 120-byte frames for both sampling entrypoints,
64-byte and 56-byte frames for the oversampled and direct SysTick closures, a
272-byte common `operating_cycle` frame, a 1,472-byte Operating-entry frame, and
a 12,720-byte initialization-only main frame. The production ELF is 148,660
bytes across all sections, including 24,760 bytes of ITCM, 2,568 bytes of DTCM,
and 6,832 bytes of BSS.

SN3 was not reflashed during this structural update so its calibration workflow
would not be disturbed. At that point, the final rounded-N timing sweep and all
dynamic signal-response measurements remained outstanding. Dynamic magnitude,
phase, folded-alias, and noise testing is intentionally deferred until a
programmable signal generator is available.

## Calibrated upper-rate characterization

On 2026-07-31, calibrated SN3 ran the final rounded-N/direct release firmware
through 10-second Deimos UDP benchmarks above the original 5 kHz limit. The
benchmark used the normal calibrated operating path, Performant controller
mode, and the A0:CE:C8:69:F4:6E Ethernet adapter.

| Rate | Minimum board margin | Steady p01 board margin | Final-5-s drop rate | Timestamp result |
|---:|---:|---:|---:|---|
| 5.0 kHz | 106.860 us | 115.970 us | 0.00028000 | monotonic |
| 6.0 kHz | 67.110 us | 76.380 us | 0.05006667 | monotonic |
| 7.0 kHz | 42.325 us | 45.875 us | 0.05925714 | monotonic |
| 8.0 kHz | 18.085 us | 23.135 us | 0.01762500 | monotonic |
| 8.5 kHz | 6.465 us | 10.375 us | 0.05962353 | monotonic |
| 8.75 kHz | 0.955 us | 11.095 us | 0.01602286 | monotonic |
| 8.8 kHz | 1.395 us | 6.885 us | 0.01552273 | monotonic |
| 8.85 kHz | 0.615 us | 5.775 us | 0.05882486 | monotonic |
| 8.9 kHz | -0.555 us | 5.295 us | 0.01968539 | monotonic |
| 9.0 kHz | -4.655 us | 2.475 us | 0.05775556 | 18 regressions |

The strict measured deadline crosses between 8.85 and 8.9 kHz. The controller
margin remained positive through 9 kHz, making firmware execution the limiting
side. The supported Deimos maximum is therefore 8 kHz, which retains 18.085 us
of observed worst-case board margin instead of treating the barely positive
8.85 kHz result as usable headroom. The supported Modbus maximum is separately
set to 500 Hz to reserve substantially more time for TCP and request processing.
The common supported minimum is 5 Hz. The 4 Hz measurements below are retained
as historical characterization outside the supported operating envelope.

Loss-of-contact rates remain bursty and nonmonotonic with board margin, matching
the previously observed host/Ethernet behavior. The timing decision therefore
uses board margin and timestamp continuity rather than packet loss alone. CSV
artifacts are stored under `target/rev7_rate_benchmark/`.

## Result

The firmware now has three operating-entry sampling topologies which share one
ADC acquisition/handoff implementation and one engineering/network publishing
cycle:

- free-running TIM2 at 33 kHz below 2.5 kHz publication rate;
- cycle-owned SysTick at three samples per cycle from 2.5 kHz through 3.799 kHz;
- cycle-owned SysTick at one sample per cycle from 3.8 kHz upward.

The cutovers are compiled constants in `deimos_shared`; neither protocol gained
configuration fields. Integer cross multiplication selects a topology once at
operating entry. Rate changes continue to use operating re-entry, which releases
the old IRQ scope before lending the sampler to its new owner.

The 3x path is timing-safe over its checked-in 2.5--3.799 kHz interval. It also
ran correctly down to 4 Hz in a forced-cutover characterization image, but that
does not establish acceptable folded-alias behavior at low sample rates. The
33 kHz path therefore remains below 2.5 kHz. The 3x communication tick becomes
marginal near 3.9 kHz and overruns by 4 kHz, so the 1x path begins at 3.8 kHz.
The 1x path retained at least 103.6 us of measured board margin through 5 kHz.

## Implementation

- TIM2 registration moved from the entire board lifetime into state-local IRQ
  scopes. The sampler is never mutably shared between registered handlers.
- Both cycle-owned handlers sample before variable-duration engineering and
  network work. The 3x handler uses a countdown; only every third tick publishes
  and performs communications.
- Three SysTick reloads are calculated once per publication cycle. Their sum is
  the corrected publication interval and they differ by at most one timer tick.
- The 3x ADC IIR cutoff ratio is exactly one third. The 1x path applies the
  fractional-delay FIR but no ADC IIR, without an IIR branch in the channel hot
  loop.
- Cycle-owned timestamps are captured directly by the SysTick owner after the
  acquisition clock advances. The lower-priority TIM2 path retains its bounded
  rollover retry.
- Test-only watermarks separately record sample-only and
  sample-plus-communication margins. The packet margin is the time to the next
  sampling deadline for cycle-owned modes.
- The 16-bit counter unroller now uses the exact `2^16` modulus and records the
  correct direction at an `i32` accumulator wrap. Compile-time assertions prove
  the 50 MHz timer maximum advances by less than half a modulus during the
  longest corrected subcycle/cycle implied by both checked-in cutovers.

## 3x characterization from 4 Hz to 5 kHz

SN3 was flashed with identity calibrations and a timing-watermark image whose
compiled 3x cutover was temporarily set to 4 Hz and whose 1x cutover was above
5 kHz. Runs used Deimos UDP, the A0:CE:C8:69:F4:6E Ethernet adapter, Performant
controller mode, and 10-second windows. `loss_of_contact_counter > 0` defines a
dropped cycle.

| Rate | Whole drop rate | Final-5-s drop rate | Min board margin | Steady min board margin | Timestamp result |
|---:|---:|---:|---:|---:|---|
| 4 Hz | 0 | 0 | 82.171 ms | 83.275 ms | monotonic |
| 10 Hz | 0 | 0 | 33.277 ms | 33.277 ms | monotonic |
| 100 Hz | 0 | 0 | 3.275 ms | 3.283 ms | monotonic |
| 500 Hz | 0 | 0 | 607.380 us | 607.380 us | monotonic |
| 1,000 Hz | 0 | 0 | 259.650 us | 268.555 us | monotonic |
| 1,500 Hz | 0.00006668 | 0.00013333 | 156.495 us | 157.420 us | monotonic |
| 2,000 Hz | 0.02660665 | 0.04310000 | 86.540 us | 86.540 us | monotonic |
| 2,500 Hz | 0.03536990 | 0.02704000 | 53.965 us | 53.965 us | monotonic |
| 3,000 Hz | 0 | 0 | 30.425 us | 30.425 us | monotonic |
| 3,500 Hz | 0.00014289 | 0.00017143 | 14.320 us | 14.470 us | monotonic |
| 3,750 Hz | 0.00125360 | 0 | 11.540 us | 11.640 us | monotonic |
| 3,800 Hz, repeat | 0.00168457 | 0.00042105 | 9.410 us | 9.410 us | monotonic |
| 3,900 Hz | 0.00500103 | 0.00666667 | -0.450 us | 7.610 us | monotonic |
| 4,000 Hz | 0.00155027 | 0.00045000 | -51.760 us | -1.540 us | monotonic; 2.75 ms max gap |
| 4,500 Hz | 0.04967660 | 0.05066667 | -16.890 us | -11.960 us | monotonic |
| 5,000 Hz | 0.02106506 | 0.00244000 | -24.460 us | -24.440 us | monotonic |

The fresh 3.8 kHz debug watermarks measured a 65.150 us minimum sample-only
margin and a 9.410 us minimum sample-plus-communication margin. Across the
forced sweep through 5 kHz, the sample-only minimum remained positive at
42.400 us. The communication tick is therefore the 3x timing limiter.

Loss rate is not monotonic with firmware margin: for example, the 3 kHz run had
zero loss while the 2.5 kHz run lost 3.54%, despite the latter having more board
margin. This matches the previously characterized host/USB-Ethernet sensitivity
of `loss_of_contact_counter`; it must not be interpreted as an IRQ-overrun
measurement. Negative board margin and enlarged timestamp gaps identify the
actual 3x deadline failure above the safe range.

## Checked-in cutover verification

Boundary and upper-range runs used the final 2.5/3.8 kHz constants:

| Rate | Selected topology | Min board margin | Steady min margin | Timestamp result |
|---:|---|---:|---:|---|
| 1,000 Hz | 33 kHz free-running | 256.025 us | 256.025 us | monotonic |
| 2,499 Hz | 33 kHz free-running | 74.800 us | 77.935 us | monotonic |
| 2,500 Hz | 3x | 53.600 us steady | 53.600 us | monotonic |
| 3,799 Hz | 3x | 10.340 us | 10.390 us | monotonic |
| 3,800 Hz | 1x | 184.970 us | 185.250 us | monotonic |
| 4,000 Hz | 1x | 169.350 us | 172.270 us | monotonic |
| 4,500 Hz | 1x | 130.200 us | 130.200 us | monotonic |
| 5,000 Hz | 1x | 103.640 us | 106.020 us | monotonic |

The first transmitted snapshot can still contain the packet-default zero margin
because margin describes the preceding completed cycle. One 2.5 kHz capture
included that zero after dispatch synchronization; the steady watermark and
subsequent packets remained positive.

A final non-instrumented identity-firmware smoke run at 5 kHz produced 49,989
rows, a 1.4243% whole-run and 1.0120% final-five-second loss rate, 106.270 us
minimum board margin, and no timestamp regression. SN3 was left on this final
production image.

## Stack and footprint

The pinned optimized stack-size report contains a 120-byte frame for each
sampling entrypoint, a 304-byte common `operating_cycle` frame, an 856-byte
`Board::operate` frame, and a 12,656-byte initialization-only main frame. A
combined timing/stack-watermark 5 kHz 1x run measured:

- reserved MSP: 88,656 bytes;
- high-water use: 15,264 bytes;
- untouched stack: 73,392 bytes.

The final production ELF reports 152,496 bytes across all sections: `.text`
85,848 bytes, `.rodata` 14,628, `.itcm` 29,088, `.dtcm` 2,568, `.bss` 7,040,
and Ethernet `.sram3` 12,424. ITCM remains below its 64 KiB region.

## Verification and remaining work

- `cargo test --workspace`: passed 47 Deimos, 320 numerics, 20 shared, all
  console/integration tests, and doctests; the desktop smoke test remains
  ignored. The two existing numerics test-import warnings remain.
- Rev7 release builds passed with and without timing instrumentation.
- The rate benchmark now accepts `DEIMOS_BENCH_RATE_HZ` and
  `DEIMOS_BENCH_SECONDS`, making cutover sweeps reproducible without source
  edits.
- Shared arithmetic tests cover cutover boundaries, three-way tick splitting,
  timestamp reload arithmetic, and both counter-wrap directions.

This timing report does not close the folded-alias, analog response, GPIO-marker
timestamp, maximum-rate counter-input, worst-case Modbus packet, or active
phase-correction checks. Modbus operation was unavailable because SN3 still has
identity calibrations, as intended before its calibration run. Those checks
remained deferred at that checkpoint. The later cycle-owned sampling work and
calibrated production sweep supersede that checkpoint's provisional warning
about retaining the free-running path.

## Calibrated production rate sweep

After SN3 was calibrated and flashed with the production image, the
`rev7_rate_sweep` example ran 20 logarithmically spaced 20-second points from
5 Hz through 8 kHz in Performant mode. It repeated the 12 grid points below
500 Hz in Efficient mode. The plotted loss rate, board margin, and host-process
CPU values use only each run's final five seconds so startup synchronization
does not dominate the comparison.

Efficient mode completed every requested comparison through its highest tested
rate of 358.086 Hz. Its final-five-second loss rate was zero at every point and
its host-process CPU use ranged from 1.10% to 2.67% of one CPU. Performant mode
used approximately 100% of one CPU, as designed.

| Performant rate | Final-5-s loss rate | Final-5-s minimum board margin |
|---:|---:|---:|
| 5.000 Hz | 0 | 68.365 us |
| 358.086 Hz | 0 | 55.060 us |
| 778.507 Hz | 0 | 49.880 us |
| 1,147.891 Hz | 0 | 43.550 us |
| 1,692.537 Hz | 0 | 54.700 us |
| 2,495.608 Hz | 0.026605 | 21.115 us |
| 3,679.717 Hz | 0.035817 | 188.840 us |
| 5,425.642 Hz | 0.002433 | 95.360 us |
| 8,000.000 Hz | 0.060423 | 17.715 us |

The margin discontinuity above 3 kHz is the expected transition to the
sample-per-cycle topology. Every run retained positive settled board margin,
including 8 kHz. No point produced a sample-time regression or a stale sample
time on a fresh snapshot.

The interactive results are published as
`software/deimos_website/docs/assets/rev7_rate_sweep_light.html` and
`rev7_rate_sweep_dark.html`. The example checkpoints both themes and its
summary table after every completed point, waits for the board's bounded
Operating-to-Binding timeout between points, and bounds discovery retries. If
an Efficient run above 50 Hz fails, it stops probing upward and confirms
previously completed candidates in descending order to find a reliable
maximum.

## Final calibrated Modbus matrix

On 2026-07-31, calibrated SN3 ran the bounded `rev7_modbus_phase4` hardware
harness against the final rounded-N/direct firmware. The first attempt found
that the harness's malformed-range probe still used the old 75-register
snapshot boundary. The firmware correctly treated that request as valid after
the acquisition timestamp expanded the snapshot to 79 registers. The harness
now derives the final-valid address from `SNAPSHOT_INPUT_REGISTER_COUNT`, so
future snapshot-layout changes cannot silently invalidate that test.

The complete production-image `all` suite passed:

- fragmented MBAP/PDU delivery, standard exception responses, connection-local
  rejection of invalid MBAP fields, partial-ADU close, and arbitrary Unit
  Identifiers;
- four consecutive close/reconnect cycles followed by a fresh valid session;
- complete timing and safe-output FC16 writes, complete holding-register reads,
  and sustained synchronized 79-register FC04 reads at 5 and 500 Hz;
- a finite 8,192-request pipelined backpressure attempt with every complete
  request accepted by the host subsequently drained; and
- the 62-second default application timeout, replacement connection, and
  rejection of the stale session.

| Production-image workload | Result | Minimum returned board margin | Loss counter |
|---|---:|---:|---:|
| 5 Hz, 4.000 s | 20 reads, 5.0 reads/s | 62.260 us | 1 |
| 500 Hz, 5.002 s | 2,501 reads, 500.0 reads/s | 56.695 us | 1 |
| Backpressure, 2,688 responses | 0 deadline misses | 34.140 us; p01 40.640 us | 1 |

The same calibration was then flashed with only the test-only stack and timing
watermarks enabled. The compliant endpoint run measured:

- minimum sample-only margin: 89.345 us;
- minimum sample-plus-communication margin: 47.530 us;
- MSP reservation: 88,888 bytes;
- MSP high-water use: 15,956 bytes; and
- minimum untouched MSP stack: 72,932 bytes.

After the adversarial backpressure run, the global sample-only and
sample-plus-communication minima were 89.215 us and 34.270 us respectively.
MSP high-water use did not increase. All 2,688 responses drained on the same
connection with zero returned deadline misses and a 40.575 us returned-margin
first percentile.

Finally, `uv run python firmware/flash.py` restored the ordinary production
image. The embedded calibration and archived SN3 `calibration.bin` both had
SHA-256 `15ac3fd3c65df02e89ca0e690b6eb5ee040becb7a50e95b7c5b786f34920a07b`.
SN3 responded at `169.254.101.34`, port 502 accepted a new connection, and a
complete synchronized snapshot decoded successfully after the restore.

## Modbus period and phase corrections

The Modbus holding map now appends a signed `i64` persistent period delta at
registers 27--30 and a signed `i64` one-shot phase delta at registers 31--34.
Existing register addresses are unchanged. Both values use
most-significant-register-first order. The firmware saturating-adds them and
reuses the common internal `+/-10%` nominal-period clamp before constructing
the next synchronous sample schedule. Consuming a phase correction clears its
holding-register value; the period value remains until replaced.

Calibrated SN3 passed a focused production-image correction test at 100 Hz:

- an `i64::MAX` period request produced a persistent 11.000 ms interval;
- an `i64::MIN` phase request produced one shortened interval and then returned
  to the nominal cadence; and
- holding-register reads returned the raw persistent period request and zero
  after phase consumption.

At the 500 Hz supported endpoint, an `i64::MIN` persistent period request was
clamped to a 1.800 ms interval. The board returned 1,667 complete synchronized
snapshots in 3.001 seconds with an exact 1.800 ms acquisition-time step and a
45.010 us minimum returned margin. A test-instrumented repeat measured 42.475 us
minimum sample-plus-communication margin, 78.030 us minimum sample-only margin,
and 16,244 bytes of MSP high-water use out of 88,888 bytes.

The complete expanded-map matrix then passed protocol, lifecycle, 5/500 Hz
endpoint, correction, backpressure, and 62-second timeout suites. The finite
2,688-response adversarial run drained every accepted response with zero
reported deadline misses and a 33.100 us minimum returned board margin. The
ordinary calibrated production image was reflashed after instrumentation.
