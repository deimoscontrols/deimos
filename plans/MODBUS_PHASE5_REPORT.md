# Rev7 Modbus plan Phase 5 timing report

Date: 2026-07-29 (America/New_York)
Implementation base: `8a0a417` (`phase 4 cleanup`)
Phase 5 timing checkpoint: `f9ac3fa` (`add rev7 acquisition timestamps`)
Hardware: rev7 SN3, MAC `8C-1F-64-84-40-00`
Benchmark adapter: CDC-NCM `A0:CE:C8:69:F4:6E`

## Implemented scope

- Added a target-independent `AcquisitionClock` which accumulates the actual
  duration of each completed, potentially phase-adjusted SysTick interval.
- Added a TIM2 timestamp capture with exactly two possible attempts. Each
  attempt masks interrupts only while checking `PENDSTSET`, copying the clock,
  reading `SYST_CVR`, and checking `PENDSTSET` again. Two ambiguous attempts
  fall back to the preceding timestamp plus the applied TIM2 sample period.
- Explicitly set SysTick one implemented priority above TIM2. Probe reads found
  that both priority fields had previously retained reset value `0x00`; final
  readback was SysTick `0x00` and TIM2 `0x10` on the STM32H743's four-bit
  priority implementation.
- Enabled the existing `cortex-m` inline-assembly feature so the short TIM2
  critical section uses inline PRIMASK/CPSID/CPSIE instructions rather than
  out-of-line shims.
- Extended each ADC double-buffer slot with the acquisition timestamp. TIM2
  remains the only selector writer, and the higher-priority communication IRQ
  still reads one selector and copies one immutable slot.
- Added `sample_time_ns` to the common `OperatingSnapshot`, Deimos host output,
  and Modbus input-register map. A synchronized Modbus snapshot is now 79
  registers; its timestamp occupies addresses 26 through 29.
- Extended the canonical rate benchmark to reject timestamp regression and a
  non-advancing timestamp on a fresh board snapshot. Repeated timestamps are
  allowed when UDP loss causes the controller to reuse the preceding snapshot.

The timestamp labels the instant immediately before the first ADC conversion
group. It is not corrected for fractional-delay or low-pass-filter group delay.
Firmware engineering values remain `f32`; the host performs the requested
`f64` upcast, including the timestamp output used by the calc graph.

## Boundedness and realtime review

The only Phase 5 loop added to an IRQ path is the fixed
`0..MAX_CAPTURE_ATTEMPTS` loop, where `MAX_CAPTURE_ATTEMPTS` is two. The normal
path uses relaxed atomic ADC fields plus compiler fences; it adds no strongly
ordered atomic operation, allocation, hardware memory barrier, exclusive-
update loop, or FMA. Generated capture assembly contains inline interrupt-mask
instructions and no new DMB, LDREX, or STREX instruction.

The acquisition clock has one writer (SysTick). TIM2 observes it only while all
interrupts are masked, and the state-machine context resets it while interrupts
are masked. The source documents that this topology is single-core-specific;
allowing the sampler to preempt the reader or moving to multiple cores requires
a different ownership protocol, such as a triple buffer.

## 5 kHz nominal-path verification

Both final runs used the exact Phase 5 timing image, identity coefficients,
Deimos UDP, SN3, Performant controller mode, and a 10-second window. The board
published 50,000 nominal cycles per run.

| Measurement | Phase 4 final | Phase 5 run 1 | Phase 5 run 2 |
|---|---:|---:|---:|
| Rows / expected | 49,989 / 50,000 | 49,989 / 50,000 | 49,990 / 50,000 |
| Whole-run drop rate | 0.01980436 | 0.01676369 | 0.00430086 |
| Final-five-second drop rate | 0.02772000 | 0.00252000 | 0.00080000 |
| Maximum loss burst | 1 | 1 | 1 |
| Minimum controller margin | 146,947 ns | 144,167 ns | 132,398 ns |
| Minimum DAQ margin | 11,685 ns | 15,535 ns | 15,775 ns |
| Whole-run DAQ margin p01 | 15,125 ns | 27,540 ns | 27,620 ns |
| Final-five-second minimum DAQ margin | not recorded | 15,565 ns | 16,185 ns |
| Final-five-second DAQ margin p01 | 15,095 ns | 27,530 ns | 27,630 ns |
| Timestamp regressions | not applicable | 0 | 0 |
| Stale timestamps on fresh snapshots | not applicable | 0 | 0 |
| Positive timestamp-step range | not applicable | 181,820--424,280 ns | 181,820--424,280 ns |

Making the documented interrupt ordering explicit improved the repeatable
lower-tail DAQ margin rather than consuming it: SysTick can now preempt an
in-progress sample instead of waiting for TIM2 at the communication boundary.
The loss series remains sensitive to the host/adapter path, but both final runs
are within the established variation and all board deadlines remained positive.

CSVs are archived as
`target/rev7_rate_benchmark/phase5_priority_run1_20260729.csv` and
`phase5_priority_run2_20260729.csv`. Their SHA-256 digests are respectively
`b8a73d7f336fd2977c92ce98eb84f8e9c9f0422f9bbabe763dfecd57f7d81a3e` and
`ce010afc03f5596e04adb041ce96ebda8832b260b1918ff4f1dd77beb531f94f`.

## Stack and footprint

The final static report contains a 1,208-byte `Sampler::sample` frame, down 8
bytes from Phase 4. `Board::operate` remains 1,616 bytes and its communication
handler remains 344 bytes. `Board::new` is 2,920 bytes and the largest
initialization-only main frame remains 12,664 bytes. These are fixed function
frames, not cumulative interrupt-stack paths.

The production image grew by 268 bytes from the Phase 4 handoff:

| Section | Phase 4 bytes | Phase 5 timing bytes | Delta |
|---|---:|---:|---:|
| `.text` | 81,792 | 81,680 | -112 |
| `.rodata` | 14,376 | 14,408 | +32 |
| `.itcm` | 22,524 | 22,812 | +288 |
| `.dtcm` | 2,408 | 2,408 | 0 |
| `.bss` | 7,008 | 7,040 | +32 |
| all reported sections | 141,404 | 141,672 | +268 |

## Software verification

- `cargo test --workspace`: passed 46 Deimos, 320 numerics, 17 shared, all
  enabled console/integration tests, and doctests; the desktop smoke test
  remains ignored. The two known numerics test-import warnings remain.
- `cargo check -p deimos --examples`: passed.
- `cargo test -p deimos --example rev7_modbus_test`: four passed.
- `cargo test -p deimos --example rev7_modbus_client`: passed.
- Rev7 `cargo build --release`: passed without a warning.
- The pinned stack-size report, formatting of changed sources, packet/register
  golden tests, and diff checks passed.

SN3 was left with the identity/uncalibrated production image: no
`static/calibration.in` is present. This is the required starting point for the
identity-first calibration procedure, and Modbus/TCP remains gated while that
flag is clear.

## Remaining Phase 5 hardware matrix

- Compare the acquisition timestamp against a GPIO marker at the first ADC
  conversion with an oscilloscope or logic analyzer, including an actively
  phase-adjusted cycle.
- Run the interactive identity-first calibration on the rev7 units in scope,
  embed each generated `calibration.bin`, and verify calibrated Deimos and
  Modbus operation.
- Perform the deferred full-range RTD and thermocouple engineering checks.
- Perform and archive physical pre-rev7 discovery, binding, configuration, and
  operating compatibility.
- Re-run the supported Modbus matrix with the final calibrated snapshot and
  archive the final release checkpoint.
