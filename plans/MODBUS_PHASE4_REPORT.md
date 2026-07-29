# Rev7 Modbus plan Phase 4 implementation report

Date: 2026-07-29 (America/New_York)
Implementation base: `053cb82` (`update plan`)
Phase 4 tooling checkpoint: `e88a1dc` (`add rev7 modbus phase 4 stress tooling`)
Phase 4 handoff checkpoint: `362f295` (`harden rev7 modbus timing under load`)
Hardware: rev7 SN3, MAC `8C-1F-64-84-40-00`
Benchmark adapter: CDC-NCM `A0:CE:C8:69:F4:6E`

## Implemented scope

- Added `rev7_modbus_phase4`, a finite host-side hardware harness for malformed
  framing, fragmented delivery, lifecycle, endpoint, backpressure, address-
  transition, and timeout tests. Ordinary suites keep exactly one request
  outstanding. Only the explicitly adversarial backpressure suite pipelines a
  finite request burst.
- Added optional startup-only MSP painting and probe-side reporting through
  `stack-watermark` and `firmware/stack-watermark.sh`. Production firmware has
  no stack-painting work.
- Added optional relaxed minimum-cycle-margin recording and
  `firmware/cycle-margin-watermark.sh`. The recorder has one IRQ writer, uses no
  exclusive-update loop, and is absent from production firmware.
- Added a shared direct Modbus network-byte encoder for a complete engineering
  snapshot. The register-valued API derives from that byte encoder, so the
  optimized firmware path and host register layout do not duplicate field
  ordering.
- Made a complete 75-register FC04 read write its 150 payload bytes directly
  into the fixed response. Partial reads retain the generic fixed-register
  path.
- Put the bounded Modbus request dispatcher in ITCM. This moved existing code
  rather than adding a second dispatcher and leaves more than 40 KiB of ITCM
  available.

The production request policy is unchanged: no more than two TCP receives, two
TCP transmits, two Ethernet-frame receives, two Ethernet-frame transmits, and
one complete request parse occur in a Modbus-capable cycle. Request and response
storage remains fixed at 256 bytes; no firmware loop introduced in Phase 4 is
unbounded.

## Hardware matrix

SN3 used identity coefficients with a temporary calibrated flag only for the
Modbus transport tests. All output writes used the shared safe default state.
The temporary flag and test instrumentation were removed before the final
nominal-path runs.

### Protocol and lifecycle

The protocol suite passed:

- a valid FC04 request fragmented within both the MBAP prefix and PDU;
- unsupported-function, zero-count, out-of-range, and inconsistent FC16 byte-
  count exceptions without losing the connection;
- connection-local rejection of a nonzero protocol identifier, MBAP length 1,
  and MBAP length 251, each followed by a fresh valid full-snapshot connection;
- close during a partial ADU followed by a clean full-snapshot reconnect; and
- response echo for arbitrary Unit Identifiers, including 0 and 255.

Four consecutive orderly close/reconnect iterations each entered a fresh
session and returned snapshot ID 1. The first snapshot's margin is the packet
default zero because no predecessor operating cycle has completed yet; endpoint
watermarks below measure completed cycles directly.

### Supported rate endpoints

The final optimized image passed complete three-register timing FC16 writes,
complete 21-register safe-output FC16 writes, complete 27-register holding
reads, and sustained complete 75-register FC04 reads at both endpoints.

| Measurement | 4 Hz | 5 kHz |
|---|---:|---:|
| Sustained interval | 4.000 s | 5.001 s |
| Complete snapshot reads | 16 | 3,555 |
| Complete reads/s | 4.0 | 710.8 |
| First / last snapshot ID | 4 / 19 | 23 / 25,022 |
| Minimum margin returned in a snapshot | 71,985,310 ns | 30,420 ns |
| Holding loss-of-contact counter after the run | 1 | 7 |

The test-only probe watermark observed a minimum of **15,590 ns** over every
cycle in the complete endpoint run. Thus the supported one-request-outstanding
matrix had no DAQ deadline miss, including cycles not selected for a Modbus
response. Direct byte encoding increased the 5 kHz complete-read rate from the
initial Phase 4 result of 684.4/s to 710.8/s without reducing low-rate response
cadence.

### Stalled client and backpressure

The host reduced its receive buffer, submitted a finite 8,192-request burst,
stopped reading for two seconds, and then drained every complete request which
its kernel had accepted. The board:

- accepted 32,256 request bytes, or 2,688 complete full-snapshot reads;
- retained complete 159-byte responses through transmit backpressure;
- drained all 2,688 responses on the same connection;
- reported loss-of-contact count 13 after recovery; and
- re-entered cleanly for a fresh full-snapshot connection after close.

This test intentionally violates the documented one-request-outstanding rule.
Its returned-margin first percentile was 7,600 ns, but 24 of 2,688 responses
reported an isolated negative margin; the test-only global watermark was
-5,250 ns. This does not represent the supported endpoint behavior above, and
the finite per-cycle budgets prevented the peer from creating unbounded work or
blocking reconnect. Further latency staging was rejected because the compliant
endpoint already retained 15,590 ns of margin and staging reduced 4 Hz full-
read throughput from 4.0/s to 1.3/s.

### Default timeout

A first full read selected the documented 10 Hz, 600-cycle defaults. With no
further application request for 62 seconds, a replacement connection completed
and returned snapshot ID 1 while the original host descriptor remained open.
Because the board owns only one TCP socket, this proves the old operating state
exited and the listener was re-established. A stale peer cannot resume after it
transmits again. The test does not require a passive peer to observe the first
RST because TCP reset segments are not retransmitted.

### DHCP/fallback transition

A NetworkManager DHCP service was introduced three seconds into a live Modbus
session on fallback address `169.254.101.34`. The server acknowledged lease
`169.254.254.142` four seconds later. All 197 sequential complete-snapshot reads
continued on the fallback connection for the full 20-second hold, ending at
snapshot ID 200. After that client closed, fallback stopped responding and the
leased address began answering both ICMP and TCP port 502. This verifies that a
lease is deferred during operation and applied through connection setup rather
than changing the address beneath an active session.

The temporary shared-network profile was deleted afterward. The host adapter is
again `Wired connection 1` at `169.254.254.1/16`.

## Stack and footprint

The combined timing/stack-instrumented backpressure run measured:

- actual flip-link MSP reservation: 88,880 bytes;
- MSP high-water use: 16,936 bytes; and
- minimum untouched stack: 71,944 bytes.

The static optimized stack report still contains 161 entries. Its largest
frames remain unchanged from the Phase 3 handoff: main 12,664 bytes,
`Board::new` 2,424, `Board::operate` 1,616, `Sampler::sample` 1,216,
`rem_pio2` 744, and `Net::poll_bounded` 736. The uncommon partial-snapshot path
has a 344-byte frame; the ITCM request dispatcher has a 216-byte frame.

The final production release image is 141,404 bytes, 796 bytes larger than the
Phase 3 handoff. Relevant sections are:

| Section | Phase 3 bytes | Phase 4 bytes | Delta |
|---|---:|---:|---:|
| `.text` | 83,600 | 81,792 | -1,808 |
| `.rodata` | 14,376 | 14,376 | 0 |
| `.itcm` | 19,920 | 22,524 | +2,604 |
| `.dtcm` | 2,408 | 2,408 | 0 |
| `.bss` | 7,008 | 7,008 | 0 |
| all reported sections | 140,608 | 141,404 | +796 |

## Final Deimos regression

All runs used the uncalibrated production image, Deimos UDP, 5 kHz, Performant
controller mode, and a 10-second window. Runs 1 and 2 predate the final direct
Modbus encoder/ITCM placement; run 3 is the exact Phase 4 handoff image. Those
changes are not executed by the Deimos operating path.

| Measurement | Phase 4 run 1 | Phase 4 run 2 | Final run 3 |
|---|---:|---:|---:|
| Rows / expected | 49,988 / 50,000 | 49,988 / 50,000 | 49,989 / 50,000 |
| Whole-run drop rate | 0.02660639 | 0.01190286 | 0.01980436 |
| Final-five-second drop rate | 0.03780000 | 0.02336000 | 0.02772000 |
| Maximum loss burst | 2 | 1 | 1 |
| Minimum controller margin | 159,922 ns | 162,606 ns | 146,947 ns |
| Minimum DAQ margin | 11,355 ns | 11,775 ns | 11,685 ns |
| Whole-run DAQ margin p01 | 15,685 ns | 15,995 ns | 15,125 ns |
| Final-five-second DAQ margin p01 | 15,675 ns | 15,915 ns | 15,095 ns |

All runs had positive DAQ margin and remain inside the established host/adapter
loss variation from Phase 3. CSVs are archived as
`target/rev7_rate_benchmark/phase4_final_run1_20260729.csv`,
`phase4_final_run2_20260729.csv`, and `phase4_final_run3_20260729.csv`.

## Verification and final state

- `cargo test --workspace`: passed 46 Deimos, 320 numerics, 14 shared, all
  enabled console/integration tests, and doctests; the pre-existing desktop
  smoke test remains ignored. Two pre-existing unused test-import warnings were
  emitted by `deimos_numerics`.
- `cargo check -p deimos --examples`: passed, including both rev7 Modbus tools.
- Rev7 `cargo build --release`: passed without a warning.
- The pinned static stack report, both probe watermark scripts, formatting,
  shell syntax, and diff checks passed.
- Source review found no new firmware allocation, realtime FMA, duplicated
  snapshot schema, or unbounded loop. New unsafe attributes only place the
  bounded dispatcher in ITCM and export the test-only probe watermark symbol;
  no new unsafe block was added.
- `python firmware/flash.py` restored the final identity/uncalibrated production
  image. SN3 is pingable at fallback address `169.254.101.34`, and TCP port 502
  is closed as required for an uncalibrated unit.

## Deferred Phase 5 work

As agreed at the Phase 3 handoff, full-range calibrated engineering checks and
the equipment-assisted identity-first calibration run remain deferred until
all firmware machinery is complete. The physical pre-rev7 compatibility run is
also deferred to Phase 5. Phase 5 additionally implements acquisition
timestamps and then performs the final release matrix.
