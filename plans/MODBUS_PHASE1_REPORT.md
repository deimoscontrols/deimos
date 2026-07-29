# Rev7 Modbus plan Phase 1 implementation report

Date: 2026-07-29 (America/New_York)  
Base revision: `8ea56a0` (`add assignment for SN3`)  
Hardware: rev7 SN3, MAC `8C-1F-64-84-40-00`

## Implemented scope

- Added direction-specific magic values and validated decoding for both sides of
  every rev7 Binding, Configuring, and Operating exchange. Pre-rev7 framing is
  unchanged, and discovery emits both binding request formats.
- Added the rev7 configuring calibration flag. Normal operation requires a
  calibrated image; calibration collection requires an identity image. Rev7 no
  longer requires a duplicate host calibration artifact.
- Added `LinearCalibration` and `Rev7Calibration` `ByteStruct` records without a
  calibration magic. An absent firmware `static/calibration.in` produces the
  documented identity/uncalibrated build; calibration processing emits the
  final binary artifact.
- Added allocation-free shared `f32` Pt100 and K-type functions, thin host `f64`
  adapters, generated coefficients, a reproducible fitter, and a fit report.
- Replaced the raw rev7 operating output with the common `OperatingSnapshot`
  engineering packet. Only the three external RTD resistance-to-temperature
  calculations remain in the host standard calc graph.
- Added the two-slot relaxed-atomic ADC publication buffer and single-load
  coherent reader.
- Enabled smoltcp TCP, reserved one 512-byte RX and one 512-byte TX buffer, and
  added one dormant socket plus lifecycle helpers. Phase 1 does not listen on
  port 502 or parse TCP data.

## Packet and calibration sizes

| Record | Bytes |
|---|---:|
| Rev7 binding input / output | 6 / 20 |
| Rev7 configuring input / output | 15 / 6 |
| Rev7 operating input / snapshot | 69 / 149 |
| Rev7 calibration image | 145 |

## Numerical validation

The checked-in generator performs interval-local optimization for both fitted
and emitted-`f32` evaluators. Detailed RMS, derivative, monotonicity, and
round-trip results are in
`software/deimos_shared/scripts/FIT_REPORT.md`.

| Conversion | Runtime form | Coefficients/degree | Maximum error |
|---|---|---:|---:|
| K-type temperature to voltage | `interpn` regular cubic B-spline | 89 coefficients | 0.00939837 K-equivalent |
| K-type voltage to temperature | `interpn` regular cubic B-spline | 513 coefficients | 0.00902328 K |
| Pt100 resistance to temperature | one global Horner polynomial | degree 10 | 0.00687762 K |

All are monotonic over their supported domains. Finite out-of-range inputs use
linear endpoint-tangent extrapolation.

## Release footprint

Both images were built from the same base toolchain and measured with
`llvm-size -A`.

| Section | Baseline bytes | Phase 1 bytes | Delta |
|---|---:|---:|---:|
| `.text` | 64,840 | 76,176 | +11,336 |
| `.rodata` | 12,492 | 16,528 | +4,036 |
| `.itcm` | 13,852 | 20,516 | +6,664 |
| `.bss` | 4,824 | 7,008 | +2,184 |
| `.sram3` | 12,424 | 12,424 | 0 |
| all reported sections | 109,304 | 133,524 | +24,220 |

## 5 kHz hardware regression

Canonical conditions: Deimos UDP roundtrip, 200,000 ns period, 10 seconds,
identity calibration image, loss-of-contact counter as the drop indicator.

| Measurement | Baseline | Custom spline | `interpn` run 1 | `interpn` run 2 |
|---|---:|---:|---:|---:|
| Rows / expected | 49,996 / 50,000 | 49,989 / 50,000 | 49,991 / 50,000 | 49,990 / 50,000 |
| Whole-run drop rate | 0.40101208 | 0.00912201 | 0.01280230 | 0.03896779 |
| Final-five-second drop rate | 0.40080000 | 0.00540000 | 0.01240000 | 0.03528000 |
| Maximum loss burst | 2 | 3 | 3 | 1 |
| Minimum controller margin | 111,457 ns | 170,909 ns | 115,796 ns | 155,426 ns |

The two `interpn` runs remain within the 0.00540 to 0.04384 final-five-second
spread observed across the earlier custom-spline runs. They do not demonstrate
a loss-of-contact-rate regression, but the run-to-run variance is too large to
claim a performance improvement from this benchmark alone. The board cycle
time and board margin minima were nonsensical in all runs because of the
pre-existing board-time wrap behavior; they are intentionally not used for this
comparison and remain in scope for the later timing phase.

Three runs of the preceding custom-spline image were attempted. The first,
immediately after that flash, stopped receiving fresh operating packets after
roughly one second and is a failed run under the benchmark rules. Two
subsequent runs completed; their final-five-second rates were 0.04384 and
0.00540, with maximum bursts of 3. This did not reproduce as a deterministic
firmware-layout failure, but the one operating-session exit remains a
reliability observation to carry into the high-rate timing investigation
rather than averaging it away.

The completed post-change hardware runs also verify that SN3 reports
`firmware_calibrated = 0`, the calibration-mode controller accepts it, every
received engineering snapshot passes magic/semantic validation, and UDP
operation remains stable with the dormant TCP socket present.

## Verification completed

- `cargo test --workspace`: passed (including 46 Deimos, 320 numerics, and 9
  shared unit tests; one pre-existing desktop-session smoke test remains
  ignored).
- `cargo check -p deimos --examples`: passed.
- rev7 firmware `cargo build --release`: passed.
- Shared fit generator reproduced the checked-in constants and report.
- `python firmware/flash.py`: flashed the Phase 1 identity image to SN3.
- Canonical SN3 5 kHz/10-second hardware run: two completed `interpn` repeats;
  the earlier custom-spline runs and one failed immediate-post-flash session
  are detailed above.

An equipment-assisted full calibration run and final calibrated-image
verification were not performed during this implementation pass. They require
the external current, RTD, thermocouple, and voltage references plus the normal
operator-entered hold sequence; SN3 remains deliberately flashed with the
identity/uncalibrated image ready for that procedure.
