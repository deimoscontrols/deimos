---
description: Signal processing pipeline conventions for Deimos firmware and host-side calc
globs: ["firmware/deimos_daq_rev7/**", "software/deimos/src/calc/**"]
---

# Signal Processing Pipeline Conventions

## Pipeline stage ordering (firmware)

- Raw ADC integer → voltage scaling → FIR fractional delay (`f1.update`) → Butterworth IIR (`f2.update`).
  The per-channel compose is literally `f2.update(f1.update(b[i] as f32 * s))`.
  Source: `firmware/deimos_daq_rev7/src/board/subsystems/sampling.rs` line 469.
- "Stage 6 decimation" is implicit: the SysTick ISR in `operating.rs` reads the latest value from the
  `ADC_SAMPLES` atomic array, which the sampling ISR overwrites at the internal ADC rate. There is no
  explicit accumulator; samples between SysTick firings are silently discarded.

## FIR fractional delay

- Always `SisoFirFilter<3, f32>` (3 taps, polynomial order 2) per channel, hardcoded in `Sampler::new()`.
  Source: `sampling.rs` lines 144, 258.
- Coefficients are computed by `flaw::polynomial_fractional_delay` (Lagrange interpolation) at init time
  based on each channel's scan group delay relative to group 0.
- The flaw crate implementation is in
  `flaw-0.6.1/src/fractional_delay.rs` (in cargo registry).

## Butterworth IIR — two orders, not one

- Firmware uses second-order (`butter2`, `SisoIirFilter<2>`) for reporting rates above
  `butter2::MIN_CUTOFF_RATIO` and falls back to first-order (`butter1`, `SisoIirFilter<1>`) below
  that threshold (roughly ~40 Hz). The host-side `Butter2` calc is always second-order.
  Source: `sampling.rs` lines 310, 461–480.
- State-space canonical form: `SisoIirFilter` stores the nontrivial row of A, C vector, and scalar D.
  Per-sample update: `Y = C·X + D·U`, then `X(k) = A·X(k-1) + U`. No biquad or direct-form II.
  Source: `flaw-0.6.1/src/iir.rs`.

## Steady-state initialization ("no step on run start")

- Both host and firmware initialize the IIR to steady state from the last known value before the first
  operating sample. Method: `SisoIirFilter::set_steady_state(u)` sets every state entry to
  `u / (1 - sum(A))` — closed-form, no matrix inversion.
- Firmware path: `update_cutoff()` in `sampling.rs` calls `set_steady_state(init_val)` where
  `init_val = ADC_SAMPLES[i].load(...)`. The `NEW_ADC_CUTOFF` flag is always set during
  `configuring.rs:92` before the first operating cycle.
- Edge case: if `init_val` is non-finite at configure time (power-on before first ADC sample),
  the firmware resets to zero → a step from zero IS possible. See `sampling.rs` line 328 guard.
- Host path: `Butter2.eval()` in `butter.rs` line 116 calls `set_steady_state(x)` on first sample.

## IIR summation order (float roundoff)

- Summation is smallest-to-largest by design. `C` and `A` arrays are reversed at `new_interpolated()`
  time (`iir.rs` lines 147–148) so that `c[0]` is the smallest coefficient.
- The `update()` dot product seeds with `d * u` (the smallest term) before summing the C·X terms.
  Source: `flaw-0.6.1/src/iir.rs` lines 39, comment block.
- No `-ffast-math` in firmware build (`Cargo.toml` uses `default-features = false` for flaw; no
  `[profile.*] rustflags` overrides found that would enable fast-math).

## ADC cutoff ratio computation

- Cutoff ratio = `reporting_rate_hz / ADC_SAMPLE_FREQ_HZ` (dimensionless).
  This equals `1 / (dt_ns * 1e-9 * ADC_SAMPLE_FREQ_HZ)`.
  Set at configure time in `configuring.rs` lines 89–91.
- The same ratio drives both the IIR cutoff and the filter-order switch.

## flaw crate

- Version 0.6.0 in firmware (pinned); 0.6.1 available in cargo registry for source inspection.
  Firmware uses `default-features = false` (no_std). Source inspection path:
  `~/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/flaw-0.6.1/src/`.
