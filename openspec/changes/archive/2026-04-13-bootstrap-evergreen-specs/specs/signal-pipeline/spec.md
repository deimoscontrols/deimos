<!-- REVIEW-COMPLETE (task 3.3): approved-with-open-REVIEW-comments — stage 6 "decimation" mechanism (SysTick readout vs explicit downsample) and order-1 Butterworth fallback for low rates need clarification; all other claims code-verified including FIR order, state-space form, steady-state init, and float precision ordering -->

## ADDED Requirements

### Requirement: A measurement passes through a fixed six-stage pipeline before reaching a dispatcher

Each raw measurement SHALL traverse the following stages in order before it becomes a row value delivered to dispatchers:

1. **RF filter** (hardware, at the sensor connector)
2. **Active low-pass** (hardware, Sallen-Key after the instrumentation amplifier)
3. **ADC input filter** (hardware, at the ADC inputs)
4. **FIR fractional delay** (firmware, cross-channel time alignment)
5. **Butterworth IIR** (firmware, state-space canonical form)
6. **Decimation** (firmware, to the user-requested output rate)

The stages MUST be composed in this order so that each stage's input bandwidth assumption is honored by the previous stage.

<!-- REVIEW: Stage ordering confirmed for firmware stages 4–5. In `firmware/deimos_daq_rev7/src/board/subsystems/sampling.rs` line ~469, the per-sample path is: raw ADC value → scaling → FIR fractional delay (f1.update) → Butterworth IIR (f2.update), matching the spec's stage 4-then-5 ordering. Stages 1–3 are hardware-only and cannot be verified in firmware/software code. -->

<!-- REVIEW: Stage 6 "Decimation" mechanism differs from a typical downsample-and-skip design. The firmware does not implement an explicit decimation counter or accumulator. Instead, the operating-cycle SysTick fires at `dt_ns` (the user output rate), and the interrupt handler reads the latest filtered value from `ADC_SAMPLES` atomic storage. The IIR cutoff ratio is set equal to `reporting_rate / ADC_SAMPLE_FREQ_HZ` at configure time (`configuring.rs` lines 89–91), so the IIR itself acts as the anti-alias filter. Every SysTick fires one output sample — there is no subsampling of accumulated internal samples. Whether this constitutes "decimation" per this spec is a judgment call for maintainers: the output rate is user-controlled and filtering precedes readout, but internal ADC samples between SysTick firings are silently discarded, not averaged or downsampled. -->

Reference: `docs/guide.md` "The signal processing pipeline"; firmware realization in `firmware/deimos_daq_rev7/` (and prior-rev siblings). The host-side `Butter2` calc in `software/deimos/src/calc/butter.rs` is a user-facing echo of stage 5, used in control loops rather than in the per-channel measurement pipeline.

#### Scenario: User configures a 100 Hz output rate

- **WHEN** a user requests a 100 Hz per-channel output rate
- **THEN** the internal ADC SHALL still run at its burst-scan rate (~33 kHz per channel), and decimation (stage 6) SHALL reduce to 100 Hz only after stages 1–5 have filtered the higher-rate data

### Requirement: The analog stages do the heavy anti-alias work

Stages 1–3 (RF, Sallen-Key, ADC input filter) SHALL together provide the dominant anti-aliasing attenuation before digitization. The active low-pass (stage 2) MUST be placed *after* the instrumentation amplifier so that amplifier offset errors are not amplified by the filter.

Reference: `docs/guide.md` "Voltage measurement frontend" ("The filter is placed *after* amplification to avoid amplifying offset errors").

#### Scenario: Amplifier offset does not saturate the filter

- **WHEN** an INAMP develops a DC offset
- **THEN** the Sallen-Key filter MUST receive the already-amplified signal and the offset MUST NOT be further amplified by the filter stage

### Requirement: FIR fractional delay aligns channels in time

Stage 4 SHALL apply a FIR fractional-delay filter per channel, implemented as a dot product over a ring buffer of recent samples using Lagrange-polynomial interpolation, to produce "synthetic simultaneous sampling" across channels that were physically scanned sequentially by the ADC.

Reference: `docs/guide.md` "FIR fractional delay"; firmware implementation under `firmware/deimos_daq_rev7/`.

<!-- REVIEW: FIR default-order claim confirmed. `firmware/deimos_daq_rev7/src/board/subsystems/sampling.rs` line 144 declares `adc_filters_fractional_delay: [SisoFirFilter<3, f32>; 18]` and line 258 constructs with `SisoFirFilter::<3, f32>::new(...)`. The `ORDER=3` const generic means 3 taps (polynomial order 2 = ORDER-1), matching the spec's "3-tap (order-2) FIR" claim. The Lagrange coefficients are computed by `flaw::polynomial_fractional_delay` in `flaw-0.6.1/src/fractional_delay.rs`; the order is hardcoded at the call site in firmware, not user-configurable. -->

#### Scenario: Three-tap FIR is the default order

- **WHEN** a channel is processed through stage 4
- **THEN** a 3-tap (order-2) FIR MUST be the default, because higher orders add overshoot and precision problems without meaningful accuracy gains

### Requirement: Butterworth IIR is implemented in state-space canonical form

Stage 5 SHALL be a Butterworth IIR filter in state-space canonical form, so that each per-sample update reduces to vector dot products with the minimum number of nonzero coefficients. The initial state at run start SHALL be the filter's steady-state response to the first observed input (no matrix inversion required at startup).

<!-- REVIEW: State-space canonical form confirmed. `flaw-0.6.1/src/iir.rs` `SisoIirFilter` stores the nontrivial row of `A`, the `C` vector, and scalar `D`, and the `update()` method is literally `Y = C·X + D·U` then `X(k) = A·X(k-1) + B·U`. No biquad or direct-form II. The `set_steady_state()` method (iir.rs line 156) sets every state entry to `u / (1 - sum(A))`, which is the closed-form steady-state without matrix inversion. Host-side `Butter2.eval()` (butter.rs line 116) calls `filt.set_steady_state(x)` on the first sample. Firmware calls `set_steady_state(init_val)` inside `update_cutoff()` (sampling.rs lines 333, 360), which fires via the `NEW_ADC_CUTOFF` flag set by `configuring.rs:91–92` before the first operating cycle — so steady-state init does happen before the first output sample under normal startup flow. -->

<!-- REVIEW: Undocumented split: the spec says stage 5 is "Butterworth IIR" without qualification, but firmware switches between a second-order filter (`butter2`, `SisoIirFilter<2>`) and a first-order fallback (`butter1`, `SisoIirFilter<1>`) depending on whether the reporting rate falls below `butter2::MIN_CUTOFF_RATIO` (sampling.rs lines 310, 461–480). For low reporting rates (roughly below ~40 Hz based on the guard condition) a first-order Butterworth is used instead of second-order. The spec should clarify whether both orders are permitted or whether the second-order form is always required. -->

Reference: `software/deimos/src/calc/butter.rs` (host-side `Butter2`); firmware implementation under `firmware/deimos_daq_rev7/`; `docs/guide.md` "IIR filter details".

<!-- REVIEW: "No step on run start" confirmed for both host and firmware. Host: `Butter2.eval()` (butter.rs line 113–118) calls `set_steady_state(x)` on the very first sample. Firmware: `update_cutoff()` (sampling.rs lines 327–336) reads the last known ADC sample from atomic storage and calls `set_steady_state(init_val)` before swapping in the new filter; this fires via `NEW_ADC_CUTOFF` which is always set during `configuring.rs:92`. Edge case: if `init_val` is non-finite (line 328 guard), the firmware resets to zero — meaning a step from zero IS possible if the last stored ADC value was NaN or Inf at configure time (e.g., immediately after power-on). Maintainers should decide whether this edge case is within the spec's "no step" guarantee. -->

#### Scenario: No step on run start

- **WHEN** a channel begins stage-5 processing at the start of an operating run
- **THEN** the filter MUST NOT produce an artificial step from zero to the input value; the first output sample MUST reflect the steady-state response to the first input

### Requirement: Decimation reduces to the user-requested output rate

Stage 6 SHALL reduce the sample rate from the internal ADC/filter rate to the user-configured output rate (5 Hz – 5 kHz). Decimation MUST occur only after stages 4–5 have filtered the higher-rate data.

Reference: `docs/guide.md` "Decimation"; host-side `software/deimos/src/dispatcher/decimation.rs` provides dispatcher-side decimation that MAY be composed on top of stage-6 decimation but is not a substitute for it.

#### Scenario: Decimation does not add aliasing

- **WHEN** a channel is decimated by stage 6
- **THEN** the output rate MUST be below the effective bandwidth delivered by stages 1–5 so that decimation does not introduce additional aliasing

### Requirement: The pipeline deliberately trades a small amount of aliasing for phase margin

The combined pipeline design SHALL prioritize preserving phase margin for real-time control over eliminating every trace of aliasing. Some aliasing is intentionally allowed; the pipeline is engineered for closed-loop control, not offline-only analysis.

Reference: `docs/guide.md` "Why so many filters?" ("some aliasing is intentionally allowed in order to preserve phase margin").

#### Scenario: Control loop tuning depends on phase margin

- **WHEN** a user closes a control loop around a Deimos-measured channel
- **THEN** the loop's phase budget MUST reflect the designed phase-margin-preserving behavior of the pipeline, not an assumption of zero aliasing

### Requirement: Internal ADC rate is not user-configurable

The per-channel internal ADC burst-scan rate SHALL be ~33 kHz (aggregate ~660 kS/s across 8 batches with ~330 ns sample-and-hold). The user-requested rate (5 Hz – 5 kHz) affects only stage-6 decimation, not the internal ADC rate.

Reference: `docs/guide.md` "The internal ADC does burst scanning at 33kHz per channel, achieving 660kS/s overall with 330ns sample-hold in 8 batches".

#### Scenario: User requests a low output rate

- **WHEN** a user configures 10 Hz output
- **THEN** the internal ADC and stages 1–5 MUST still operate at their fixed internal rates; only stage 6 MUST decimate to 10 Hz

### Requirement: Floating-point correctness is preserved through the firmware stages

Firmware implementations of stages 4–6 SHALL be built without `-ffast-math` or equivalent flags that break IEEE-754 reassociation guarantees. Summation order and FMA usage SHALL be treated as correctness concerns, not optimization concerns.

Reference: `docs/guide.md` "Floating-point precision"; see also the InterpN deck's discussion of `mul_add` and sum ordering reused across the Deimos math stack.

<!-- REVIEW: Summation order confirmed. `flaw-0.6.1/src/iir.rs` `update()` (line 39) starts the `C·X` dot product with `d * u` as the accumulator seed, commenting "Sum starting with d*u because this term is the smallest, and `c` terms are ordered from smallest to largest." The `C` and `A` arrays are reversed during `new_interpolated()` (iir.rs lines 147–148) so that `c[0]` is the smallest coefficient. This is an intentional roundoff-reduction ordering, consistent with the spec requirement. No `-ffast-math` flag found in firmware `Cargo.toml` or build scripts (only `default-features = false` for the flaw crate dependency). -->

#### Scenario: Roundoff error in the IIR

- **WHEN** the Butterworth IIR accumulates a dot product of coefficients with historical states
- **THEN** the implementation MUST order the summation (smallest-to-largest or an equivalent roundoff-controlled ordering) rather than accepting the natural evaluation order
