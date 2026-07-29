# Rev7 Modbus plan Phase 3 implementation report

Date: 2026-07-29 (America/New_York)
Phase 1 hardware-test checkpoint: `e60e8e5` (`use dtcm for tc tables`)
Implementation base: `6e1ee90` (`zero sampling time at start of operating`)
Phase 3 handoff checkpoint: `aac4013` (`stack analysis script`)
Hardware: rev7 SN3, MAC `8C-1F-64-84-40-00`
Benchmark adapter: CDC-NCM `A0:CE:C8:69:F4:6E`

## Implemented scope

- Added `rmodbus` without default features and a fixed 256-byte request/
  response implementation. No Modbus request processing allocates.
- Added the zero-based FC04 engineering-snapshot map and FC03/FC16 holding map
  in `deimos_shared`, with most-significant 16-bit register first for all
  multi-register values.
- Added complete-field holding-write validation. A candidate configuration is
  constructed and validated before application; rejected and omitted fields do
  not alter the retained output state.
- Made the first accepted FC03, FC04, or FC16 request in calibrated `Binding`
  select `OperatingModbus`. The complete first ADU survives that state
  transition and is answered from the first operating snapshot.
- Accepted and echoed every one-byte Unit Identifier, including 0 and 255.
- Implemented standard exceptions for unsupported functions, illegal
  addresses/field splits, and illegal values.
- Implemented response backpressure, partial TCP delivery, orderly-close/RST
  detection, connection-local abort for malformed MBAP framing, and relisten.
- Implemented rate-change re-entry only after its FC16 response has been copied
  into the socket TX queue. Timeout and complete retained output settings
  survive re-entry.
- Kept all service work bounded per Modbus-capable board cycle: at most two TCP
  receive calls, two TCP transmit calls, two Ethernet-frame receives, two
  Ethernet-frame transmits, and one complete ADU parse. Register loops are
  bounded by 125 read registers and 21 writable registers.
- Aborted the unused TCP listener when UDP wins protocol selection, before the
  normal Deimos Configuring path resumes ordinary network polling.
- Added the register-map guide and an `rmodbus` reference client which reads and
  decodes the full 75-register snapshot.

The implemented maps and operating instructions are in
`plans/MODBUS_REGISTER_MAP.md`.

## Hardware protocol validation

For protocol testing only, SN3 was flashed with identity coefficients and a
temporary calibrated flag. That temporary flag and all diagnostic markers were
removed afterward; the final image is again explicitly uncalibrated.

The calibrated test image passed:

- a complete 75-register FC04 read decoded into `OperatingSnapshot`;
- FC03 of all 27 holding registers;
- Unit Identifiers 0, 1, 37, 254, and 255 with exact response echo;
- a request delivered in three TCP fragments across multiple 10 Hz cycles;
- unsupported-function, out-of-range-read, split-field-write, and invalid-value
  exception checks;
- safe output-state rewrite and retention across repeated reads;
- 10 to 20 Hz rate re-entry with timeout and output retention;
- 20 pipelined one-register reads at 5 kHz as a packet-storm diagnostic, with
  one ADU serviced per board cycle;
- restoration from 5 kHz to the 10 Hz/600-cycle defaults;
- connection-local rejection of a nonzero MBAP protocol identifier followed by
  a clean reconnect and valid request; and
- orderly client close followed by a new connection.

The production uncalibrated image was then flashed. SN3 remained pingable and
TCP port 502 returned `Connection refused`, confirming that calibration state
gates the listener rather than general network operation.

The first default holding-register read reported 10 Hz, a 100,000,000 ns
period, and 600 timeout cycles. The shared operating timeout path was already
exercised synthetically in Phase 2; an additional uninterrupted 60-second idle
hardware wait was not performed in this pass.

## Verification

- `cargo test --workspace`: passed (46 Deimos, 320 numerics, 14 shared, all
  enabled console/integration tests, and the Deimos doctest; one pre-existing
  desktop smoke test ignored).
- `cargo check -p deimos --examples`: passed, including the reference client.
- rev7 firmware `cargo build --release`: passed without warnings.
- rev7 firmware and shared-library Clippy checks produced no warning in a
  Phase 3 file; remaining warnings are pre-existing in unrelated code/tests.
- `python firmware/flash.py`: flashed the final identity/uncalibrated image to
  SN3.
- No temporary calibrated flag, panic loop, debug marker, or debug symbol
  remains in the final source or ELF.

## Release footprint

The final release image was measured with `llvm-size -A` against the Phase 2
image.

| Section | Phase 2 bytes | Phase 3 bytes | Delta |
|---|---:|---:|---:|
| `.text` | 74,176 | 83,904 | +9,728 |
| `.rodata` | 14,120 | 14,440 | +320 |
| `.itcm` | 20,516 | 19,920 | -596 |
| `.dtcm` | 2,408 | 2,408 | 0 |
| `.bss` | 7,008 | 7,008 | 0 |
| all reported sections | 131,524 | 140,976 | +9,452 |

The single `Interface::poll` call site remains in ITCM. Both ordinary Deimos
polling and bounded Modbus polling route through it, avoiding a second large
monomorphized smoltcp poll implementation.

## 5 kHz nominal-path regression

All Phase 3 runs used an uncalibrated production image, Deimos UDP, SN3, the
selected CDC-NCM adapter, a 200,000 ns period, `Performant` controller mode, and
the canonical 10-second window. Run 3 is the exact final handoff image after the
setup-only TCP-listener shutdown was added; runs 1 and 2 preceded that 176-byte
change. The saved post-fix Phase 2 run is the closest baseline because it also
clears the sampling accumulator on operating entry.

| Measurement | Phase 2 post-fix | Phase 3 run 1 | Phase 3 run 2 | Phase 3 final run 3 |
|---|---:|---:|---:|---:|
| Rows / expected | 49,989 / 50,000 | 49,987 / 50,000 | 49,991 / 50,000 | 49,991 / 50,000 |
| Whole-run drop rate | 0.02080458 | 0.01402365 | 0.03820688 | 0.03472625 |
| Final-five-second drop rate | 0.01532000 | 0.02668000 | 0.03484000 | 0.02412000 |
| Maximum loss burst | 1 | 8 | 1 | 3 |
| Minimum controller margin | 131,552 ns | 57,193 ns | 163,412 ns | -73,462 ns |
| Minimum DAQ margin | 12,835 ns | 9,635 ns | 10,445 ns | 2,570 ns |
| Whole-run DAQ margin p01 | 16,335 ns | 14,875 ns | 14,685 ns | 14,975 ns |
| Final-five-second DAQ margin p01 | 16,275 ns | 14,815 ns | 14,675 ns | 14,965 ns |

The stable DAQ-margin percentile estimates a Phase 3 nominal-path cost of about
1.3--1.7 microseconds per 200-microsecond cycle. All runs retained at least
14.675 microseconds at the final-window first percentile and had no missed DAQ
deadline. Run 3 contained one controller-host scheduling overrun and one low
but still positive DAQ-margin outlier; neither changed the repeatable DAQ first
percentile. The loss counter remains dominated by host/adapter packet bursts:
the whole-run rates straddled the Phase 2 value while the board-margin
percentiles were repeatable. Nearly all 50,000 snapshots were delivered in
every run.

The second run CSV is archived as
`target/rev7_rate_benchmark/phase3_run2_20260729.csv`; the exact-final-image CSV
is `target/rev7_rate_benchmark/phase3_final_run3_20260729.csv`. The first run was
overwritten by the repeat before archival, so its printed summary above is the
retained record.

## Phase 3 review

A post-implementation review of commit `af2f02b` tightened the Phase 3 code
without changing the register map or protocol behavior:

- Factored the duplicated MBAP-prefix parser, response-header construction,
  Modbus-listener recovery, and fixed-array register encoding/decoding.
- Added Google-style no-types docstrings and field comments to the new framing,
  socket-budget, register-map, and network-storage code. Units and fixed array
  shapes are stated at the relevant boundaries.
- Replaced the two state-transition `unwrap` calls with safe reconnect fallbacks,
  made the loss-of-contact increment saturating, and removed a fallible unwrap
  from realtime exception construction.
- Added shared decoder coverage for wrong register count, wrong snapshot magic,
  and an out-of-range GPIO value.
- Inspected `rmodbus::ModbusFrame::parse`; it contains no loop. Firmware request
  loops are bounded by two socket calls, the 256-byte ADU, the 21-register
  writable map, or fixed compile-time array lengths.
- Put every `smoltcp::Interface::poll` behind a finite device-token allowance.
  Modbus retains one shared two-RX/two-TX frame budget across both polls in a
  cycle. An ordinary poll permits at most eight RX and eight TX frames, so the
  Deimos loop's two polls permit at most 16 in each direction per cycle. The
  only explicit unbounded loops in the reviewed paths are the interrupt-driven
  `wfi` state-transition waits.
- Confirmed that the reviewed paths add no allocation, realtime FMA, or unsafe
  block. The existing `link_section` attribute is the only unsafe annotation in
  the network path.

The ordinary-poll bound was selected with a hardware A/B. A four-frame limit
spread queued traffic over enough cycles to reduce the DAQ-margin percentile;
the final eight-frame limit drained the normal backlog while remaining strictly
bounded. The unlimited run below was diagnostic only and was not left flashed.

| Measurement | bounded 4 run 1 | bounded 4 run 2 | unlimited A/B | final bounded 8 run 1 | final bounded 8 run 2 |
|---|---:|---:|---:|---:|---:|
| Rows / expected | 49,989 / 50,000 | 49,991 / 50,000 | 49,990 / 50,000 | 49,988 / 50,000 | 49,989 / 50,000 |
| Whole-run drop rate | 0.01638360 | 0.02840511 | 0.04318864 | 0.04813155 | 0.04382964 |
| Final-five-second drop rate | 0.01156000 | 0.03272000 | 0.03788000 | 0.04560000 | 0.04684000 |
| Maximum loss burst | 1 | 1 | 1 | 1 | 1 |
| Minimum controller margin | 152,563 ns | 158,663 ns | 134,644 ns | 160,351 ns | 104,327 ns |
| Minimum DAQ margin | 11,825 ns | 7,475 ns | 11,825 ns | 11,295 ns | 11,345 ns |
| Whole-run DAQ margin p01 | 12,595 ns | 12,575 ns | 15,385 ns | 15,655 ns | 15,485 ns |
| Final-five-second DAQ margin p01 | 12,705 ns | 12,525 ns | 15,355 ns | 15,625 ns | 15,475 ns |

The final bounded runs have no DAQ deadline miss and recover roughly 0.5--1.0
microseconds of first-percentile margin relative to the pre-review Phase 3
runs. Their loss rates remain within the established adapter/host variation and
all loss bursts were isolated single cycles. The final reviewed release image
is 140,608 bytes: 368 bytes smaller than the `af2f02b` image. Its principal
sections are `.text` 83,600, `.rodata` 14,376, `.itcm` 19,920, `.dtcm` 2,408,
and `.bss` 7,008 bytes.

Review CSVs are archived as
`target/rev7_rate_benchmark/phase3_review_run1_20260729.csv`,
`phase3_review_run2_20260729.csv`,
`phase3_review_unbounded_ab_20260729.csv`,
`phase3_review_bounded8_run1_20260729.csv`, and
`phase3_review_bounded8_run2_20260729.csv`. The final bounded-eight,
identity-calibration image is flashed on SN3.

## Stack-frame baseline at handoff

The pinned `firmware/stack-sizes.sh` diagnostic at handoff produced 161
demangled fixed-frame entries from a separate optimized fat-LTO object. The
largest frames relevant to tracking firmware growth were:

| Function | Fixed frame bytes |
|---|---:|
| `deimos_bare_metal::__cortex_m_rt_main` | 12,664 |
| `Board::new` | 2,424 |
| `Board::operate` | 1,616 |
| `Sampler::sample` | 1,216 |
| `libm::math::rem_pio2::rem_pio2` | 744 |
| `Net::poll_bounded` | 736 |

These are individual fixed frames rather than cumulative call-chain bounds and
exclude hardware exception frames and unresolved indirect callees. Phase 4
therefore pairs this reproducible static regression report with an on-target MSP
high-water measurement during its endpoint stress matrix.

## Deferred Phase 4 work

Phase 3 included targeted malformed-frame, partial-delivery, reconnect, and
high-rate request tests needed to validate the implementation. Broader Phase 4
hardening remains: DHCP/fallback changes during a session, deliberately stalled
clients with TX backpressure, the full 60-second default timeout, and sustained
complete reads and writes at the 4 Hz and 5 kHz endpoints. The
equipment-assisted identity-first calibration and full-range calibrated
engineering checks remain deferred until the end of Phase 5, after all other
baseline firmware machinery is complete. The physical pre-rev7 compatibility
run also remains a final Phase 5 verification item, with `e60e8e5` retained as
the attribution checkpoint.
