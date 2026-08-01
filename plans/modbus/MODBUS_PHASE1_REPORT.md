# Rev7 Modbus plan Phase 1 implementation report

Date: 2026-07-29 (America/New_York)  
Base revision: `8ea56a0` (`add assignment for SN3`)  
Deferred hardware-verification checkpoint: `e60e8e5` (`use dtcm for tc tables`)
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

| Section | Baseline bytes | Final Phase 1 bytes | Delta |
|---|---:|---:|---:|
| `.text` | 64,840 | 73,936 | +9,096 |
| `.rodata` | 12,492 | 14,120 | +1,628 |
| `.itcm` | 13,852 | 20,516 | +6,664 |
| `.dtcm` | 0 | 2,408 | +2,408 |
| `.bss` | 4,824 | 7,008 | +2,184 |
| `.sram3` | 12,424 | 12,424 | 0 |
| all reported sections | 109,304 | 131,284 | +21,980 |

The final image stores the two K-type coefficient tables in initialized DTCM.
They are copied by the normal `.data` startup path, while `.itcm` contains only
the selected realtime code.

## 5 kHz hardware regression

Canonical conditions: Deimos UDP roundtrip, 200,000 ns period, 10 seconds,
identity calibration image, loss-of-contact counter as the drop indicator.

| Measurement | Baseline | Custom spline | `interpn` run 1 | `interpn` run 2 | DTCM run 1 | DTCM run 2 | DTCM run 3 |
|---|---:|---:|---:|---:|---:|---:|---:|
| Rows / expected | 49,996 / 50,000 | 49,989 / 50,000 | 49,991 / 50,000 | 49,990 / 50,000 | 49,992 / 50,000 | 49,995 / 50,000 | 49,994 / 50,000 |
| Whole-run drop rate | 0.40101208 | 0.00912201 | 0.01280230 | 0.03896779 | 0.40080413 | 0.40072007 | 0.40068808 |
| Final-five-second drop rate | 0.40080000 | 0.00540000 | 0.01240000 | 0.03528000 | 0.40096000 | 0.40076000 | 0.40064000 |
| Maximum loss burst | 2 | 3 | 3 | 1 | 2 | 2 | 3 |
| Minimum controller margin | 111,457 ns | 170,909 ns | 115,796 ns | 155,426 ns | 121,343 ns | 161,682 ns | 179,347 ns |

The final DTCM runs used a replacement USB Ethernet adapter after the original
adapter failed to establish carrier. The replacement was
`00:E0:4C:68:05:25`, configured as `169.254.254.1/16`, with a negotiated
100-Mbit/s full-duplex link. All three runs completed without a reconnect and
received essentially all 50,000 snapshots, but approximately 40.08% of
snapshots reported a nonzero loss-of-contact counter. The loss sequence was
highly regular--predominantly `0, 1, 0, 0, 1`--and each one-second bucket was
between 39.92% and 40.18%.

The third run was performed after restoring laptop wall power. Its minimum
controller margin improved to 179,347 ns while its final-five-second loss rate
remained 0.40064, so host CPU power throttling does not explain the regular
loss cadence.

These results match the original baseline rather than the lower-loss earlier
Phase 1 runs. Because the physical host network path changed, they do not by
themselves establish that moving the tables from ITCM to DTCM caused the
difference. They also do not provide a clean pass of the performance gate.

### Controlled ITCM/DTCM A/B

A subsequent A/B test held SN3, the controller binary, wall-power state,
replacement Ethernet adapter, link configuration, firmware dependency
lockfile, and compiler profile constant. The ITCM image came from commit
`27c1cc5`; the DTCM image was the final working image. Their `.text` sections
were both 73,936 bytes, and symbol inspection confirmed the intended material
layout change: the two coefficient objects moved from ITCM addresses
`0x00004eb0` and `0x00005014` to DTCM addresses `0x20015b34` and `0x20015c98`.

| Measurement | ITCM A | DTCM B |
|---|---:|---:|
| Rows / expected | 49,996 / 50,000 | 49,992 / 50,000 |
| Whole-run drop rate | 0.40075206 | 0.40060410 |
| Final-five-second drop rate | 0.40100000 | 0.40052000 |
| Maximum loss burst | 2 | 2 |
| Minimum controller margin | 174,689 ns | 164,825 ns |

Both images exhibited the same regular loss-counter cadence. The whole-run
rates differ by only 0.00014796, with DTCM slightly lower, so table placement
does not explain the return to the roughly 40% result. Keep the tables in DTCM;
the remaining investigation should focus on the regular controller-input
acceptance and firmware network-poll timing on this host network path.

### Controlled const-in-flash/DTCM A/B

A second A/B tested the remaining table-storage distinction between the earlier
low-loss image and the final firmware. The const image was built from commit
`a8ee620`, where the coefficient arrays were anonymous `const` data materialized
in flash. It used the same current firmware `Cargo.lock` as the DTCM image. Both
images had a 73,936-byte `.text` section and a 131,284-byte total allocation;
the only material layout difference was 2,408 coefficient bytes in `.rodata`
for the const image versus `.dtcm` for the DTCM image. SN3, the release
controller binary, wall-power state, replacement Ethernet adapter, and
100-Mbit/s full-duplex link were held constant. The const image was tested
first and the DTCM image second.

| Measurement | Const in flash A | DTCM B |
|---|---:|---:|
| Rows / expected | 49,992 / 50,000 | 49,995 / 50,000 |
| Whole-run drop rate | 0.40068411 | 0.40102010 |
| Final-five-second drop rate | 0.40064000 | 0.40140000 |
| Maximum loss burst | 2 | 3 |
| Minimum controller margin | 144,119 ns | 176,914 ns |

The const image reproduced the same approximately 40% cadence instead of its
earlier approximately 1% result. The whole-run A/B difference was 0.00033599,
with no direction consistent with a table-placement penalty. Const versus
static storage, flash versus ITCM, and flash versus DTCM are therefore ruled out
as explanations for the earlier low-loss runs. The DTCM image was restored to
SN3 after the test. Raw CSVs are archived as
`target/rev7_rate_benchmark/ab_const_flash_20260729.csv` and
`target/rev7_rate_benchmark/ab_dtcm_after_const_20260729.csv`.

### Controlled USB Ethernet adapter comparison

The original USB Ethernet adapter was subsequently reconnected and tested
without reflashing or rebuilding either the restored DTCM firmware or the
release controller. This adapter uses the `cdc_ncm` driver, appeared as
`enxa0cec869f46e`, and negotiated a 100-Mbit/s link. The host retained the same
`169.254.254.1/16` address and direct DAQ connection. Two completed runs were
compared with the immediately preceding replacement-RTL8153 DTCM run.

| Measurement | Replacement RTL8153 | Original CDC-NCM run 1 | Original CDC-NCM run 2 |
|---|---:|---:|---:|
| Rows / expected | 49,995 / 50,000 | 49,990 / 50,000 | 49,991 / 50,000 |
| Whole-run drop rate | 0.40102010 | 0.02884577 | 0.03340601 |
| Final-five-second drop rate | 0.40140000 | 0.04324000 | 0.04012000 |
| Maximum loss burst | 3 | 1 | 1 |
| Minimum controller margin | 176,914 ns | 163,166 ns | 112,924 ns |

The adapter change eliminates the rigid approximately 40% two-of-five loss
cadence and returns the current firmware to the same broad loss range as the
earlier low-loss runs. Losses on the CDC-NCM adapter are temporally clustered
but individually isolated: one-second rates ranged from zero to 10.64%, while
the loss-of-contact counter never exceeded one. The substantial adapter effect
with an otherwise unchanged firmware/controller pair establishes the host
USB-Ethernet path as the source of the large measurement difference and
strongly implicates its packet pacing, not thermocouple table storage or the
additional Phase 1 calculations. Raw CSVs are archived as
`target/rev7_rate_benchmark/original_adapter_dtcm_run1_20260729.csv` and
`target/rev7_rate_benchmark/original_adapter_dtcm_run2_20260729.csv`.

The two `interpn` runs remain within the 0.00540 to 0.04384 final-five-second
spread observed across the earlier custom-spline runs. They do not demonstrate
a loss-of-contact-rate regression, but the run-to-run variance is too large to
claim a performance improvement from this benchmark alone. The board
`cycle_time_ns` field is absolute uptime rather than a cycle duration. The
whole-run margin minimum was also corrupted by subtracting sampling time
accumulated before Operating from the first completed cycle; final-window
margin statistics were unaffected. The accumulator initialization was corrected
after Phase 2 without changing these archived results.

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
- Canonical SN3 5 kHz/10-second hardware run: two completed `interpn` repeats
  and three completed final DTCM-layout repeats; the changed Ethernet adapter,
  earlier custom-spline runs, and one failed immediate-post-flash session are
  detailed above.
- Controlled same-adapter ITCM/DTCM A/B run: completed with the same dependency
  lockfile and identical `.text` size; table placement did not affect the loss
  cadence.
- Controlled same-adapter const-in-flash/DTCM A/B run: completed with the same
  dependency lockfile, controller binary, `.text` size, and total allocation;
  both images reproduced the approximately 40% loss cadence.
- Controlled same-firmware USB Ethernet adapter comparison: replacing the
  RTL8153 adapter with the original CDC-NCM adapter reduced whole-run loss from
  approximately 40.10% to 2.88--3.34% and removed the rigid loss cadence.

An equipment-assisted full calibration run and final calibrated-image
verification were not performed during this implementation pass. They require
the external current, RTD, thermocouple, and voltage references plus the normal
operator-entered hold sequence; SN3 remains deliberately flashed with the
identity/uncalibrated image ready for that procedure.
