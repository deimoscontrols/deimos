# Rev7 Modbus plan Phase 2 implementation report

Date: 2026-07-29 (America/New_York)
Phase 1 hardware-test checkpoint: `e60e8e5` (`use dtcm for tc tables`)
Hardware: rev7 SN3, MAC `8C-1F-64-84-40-00`
Benchmark adapter: CDC-NCM `A0:CE:C8:69:F4:6E`

## Implemented scope

- Renamed the persistent nominal operating state to `OperatingDeimos` and added
  the data-carrying `OperatingModbus(ModbusInitialConfig)` state.
- Added the private `OperatingMode` invocation selector and routed both states
  through one `Board::operate` implementation.
- Added the documented 10 Hz/600-cycle Modbus defaults. Modbus entry applies
  its period, timeout count, ADC cutoff request, and complete retained output
  settings before enabling the cycle interrupt.
- Added `OperatingOutputSettings` as one copyable, validated output value and
  embedded it in the unchanged Deimos operating-input wire layout.
- Split the common snapshot publisher and output/address/timing work from the
  mode-specific I/O branch. Deimos retains its two-poll UDP behavior and timing
  corrections. The intentionally unreachable Phase 2 Modbus branch retains
  snapshots, polls the interface, applies no Deimos timing correction, and
  reaches the existing loss-of-contact transition when no protocol request can
  reset the counter.
- Added a rate-reentry constructor which changes only `dt_ns`, preserving the
  current loss-of-contact count and full output settings.
- Extended the canonical benchmark with whole-run and final-five-second DAQ
  cycle-margin minima and first percentiles.

No Modbus listener, parser, request acceptance, register map, or production
transition into `OperatingModbus` was added; those remain Phase 3 work.

## Compatibility and tests

`OperatingRoundtripInput` remains 69 bytes. Tests pin the nested output field at
its previous byte offsets: PWM duty starts at byte 28, PWM frequency at byte 44,
DAC voltage at byte 60, and GPIO at byte 68. The controller and HOOTL adapters
use the nested value without changing packet bytes.

The shared tests verify that:

- output settings serialize, validate, copy, and round-trip as one value;
- Modbus defaults are `dt_ns = 100_000_000` and
  `loss_of_contact_limit = 600`;
- a 200,000 ns rate re-entry preserves timeout count and every output field;
- the Deimos packet length and output offsets remain unchanged.

Verification completed:

- `cargo test --workspace`: passed (46 Deimos, 320 numerics, 10 shared, and all
  other enabled workspace tests; one pre-existing desktop smoke test ignored).
- `cargo test -p deimos --example rev7_rate_benchmark`: passed.
- `cargo check -p deimos --examples`: passed.
- rev7 firmware `cargo build --release`: passed without warnings.
- `python firmware/flash.py`: flashed the Phase 2 identity image to SN3.

## Release footprint

The final output-retention optimization removed a redundant 41-byte settings
copy from each accepted Deimos input and reduced the IRQ closure from 2,968 to
2,896 bytes. Relative to the Phase 1 checkpoint:

| Section | Phase 1 bytes | Phase 2 bytes | Delta |
|---|---:|---:|---:|
| `.text` | 73,936 | 74,176 | +240 |
| `.rodata` | 14,120 | 14,120 | 0 |
| `.itcm` | 20,516 | 20,516 | 0 |
| `.dtcm` | 2,408 | 2,408 | 0 |
| `.bss` | 7,008 | 7,008 | 0 |
| all reported sections | 131,284 | 131,524 | +240 |

## 5 kHz hardware regression

The Phase 1 comparison values are the two final runs made with the selected
CDC-NCM adapter and the checkpoint firmware. The Phase 2 values are two runs of
the final optimized image. All used the same SN3, controller host, direct
100-Mbit/s link, 200,000 ns period, controller configuration and control-loop
implementation, and identity calibration. The Phase 2 benchmark executable
adds only the post-run DAQ-margin statistics.

| Measurement | Phase 1 run 1 | Phase 1 run 2 | Phase 2 run 1 | Phase 2 run 2 |
|---|---:|---:|---:|---:|
| Rows / expected | 49,990 / 50,000 | 49,991 / 50,000 | 49,990 / 50,000 | 49,991 / 50,000 |
| Whole-run drop rate | 0.02884577 | 0.03340601 | 0.04708942 | 0.04420796 |
| Final-five-second drop rate | 0.04324000 | 0.04012000 | 0.05144000 | 0.06628000 |
| Maximum loss burst | 1 | 1 | 1 | 1 |
| Minimum controller margin | 163,166 ns | 112,924 ns | 163,909 ns | 104,312 ns |
| Whole-run DAQ margin p01 | 16,885 ns | 16,865 ns | 16,525 ns | 16,405 ns |
| Final-five-second DAQ margin p01 | 16,975 ns | 16,865 ns | 16,495 ns | 16,385 ns |

The DAQ first percentile is substantially more repeatable than the adapter-
dominated loss rate. Its two-run median decreased from 16,875 to 16,465 ns for
the whole run and from 16,920 to 16,440 ns for the steady window. The estimated
Phase 2 firmware cost is therefore approximately 0.4--0.5 microseconds at 5
kHz, leaving a steady first-percentile margin of at least 16.385 microseconds in
these runs. Sparse board-time wrap artifacts still invalidate the raw minimum;
Phase 5 retains the strict minimum-margin fix and gate.

Network misses remained individually isolated and all runs delivered
essentially 50,000 snapshots. Their per-second clustering and run-to-run rate
continue to follow the selected adapter/host scheduling behavior rather than
the stable DAQ margin. The first 100 ms discovery scan immediately after each
flash missed SN3; an unchanged retry found it and completed normally, matching
the adapter warm-up behavior observed before Phase 2.

Raw final CSVs are archived as
`target/rev7_rate_benchmark/phase2_optimized_original_adapter_run1_20260729.csv`
and
`target/rev7_rate_benchmark/phase2_optimized_original_adapter_run2_20260729.csv`.

## Deferred hardware verification

The equipment-assisted identity-first calibration run and physical pre-rev7
compatibility run remain deferred. Return to commit `e60e8e5` to perform those
tests against the exact Phase 1 checkpoint before attributing any result to the
Phase 2 state refactor.
