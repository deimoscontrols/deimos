# Rev7 Modbus/TCP design

## Purpose and status

The rev7 DAQ supports two externally distinct operating states which share the
same acquisition, filtering, calibration, engineering-conversion, timing, and
output machinery:

- `OperatingDeimos` exchanges one synchronous UDP command/snapshot roundtrip
  with the Deimos controller.
- `OperatingModbus` exposes the same snapshot and retained output state through
  one Modbus/TCP connection.

The shared implementation is intentional. Keeping one measurement and output
path prevents the two protocols from acquiring different conversion behavior,
filter state, timing semantics, or safety limits. The principal operating-mode
difference is therefore UDP packet handling versus bounded TCP ADU handling.

Both modes support publishing rates from 5 Hz. Deimos mode supports up to
8 kHz; Modbus mode supports up to 500 Hz. The lower Modbus ceiling reserves
time for TCP stream handling and worst-case register requests.

Historical checkpoints retained for comparison are:

| Commit | Purpose |
| --- | --- |
| `e60e8e5` | Shared calculations, engineering snapshot, calibration support, and TCP storage |
| `aac4013` | Initial bounded Modbus/TCP implementation |
| `362f295` | Hardened protocol, backpressure, and timing implementation |
| `f9ac3fa` | Acquisition timestamps |
| `dec7a99` | Cycle-owned synchronous sampling groundwork |

## Major design decisions

### One common operating implementation

The top-level states remain explicit because they have meaning outside the
firmware, but both invoke one `Board::operate` implementation through a private
mode selector:

```text
Connecting -> Binding -> Configuring -> OperatingDeimos
                    \
                     +-- first accepted Modbus request
                         -> OperatingModbus(initial configuration)

OperatingDeimos -- loss of contact --> Connecting
OperatingModbus -- loss of contact --> Connecting
OperatingModbus -- cycle-rate change --> OperatingModbus(updated configuration)
```

There is no separate `NetworkServing` state and no independently stored mode.
The first accepted FC03, FC04, FC16, or FC23 request selects Modbus operation
directly from `Binding`. Its complete ADU is retained across the transition and
answered using the first operating snapshot. This permits a read-only client to
enter Modbus mode without a preliminary write.

The UDP and TCP paths cannot own outputs concurrently. If UDP binding wins, the
unused TCP listener is closed before Deimos configuration continues. A lost TCP
session follows the same loss-of-contact transition as an application timeout.

### One coherent engineering snapshot

`OperatingSnapshot` is the source of truth for both the Deimos UDP response and
the Modbus register image. It contains:

- a packet magic and compact operating metrics;
- acquisition and publication board timestamps;
- module-bus current and voltage;
- filtered board temperature;
- four 4–20 mA currents;
- three external RTD resistances;
- two cold-junction-compensated thermocouple temperatures;
- six final voltage measurements;
- unwrapped encoder and pulse counts;
- two frequency measurements; and
- digital inputs.

Firmware publishes final engineering values as `f32`, and software upcasts them
to `f64`. External RTD channels are the exception: firmware publishes measured
resistance and software performs the final resistance-to-temperature
conversion. Board temperature is calculated and filtered in firmware because
it is also required for thermocouple cold-junction compensation.

`sample_time_ns` labels the start of the ADC group contributing to the
snapshot. `metrics.sent_time_ns` labels snapshot publication. Neither timestamp
is corrected for fractional-delay or low-pass-filter group delay. Snapshot
ordering uses `metrics.id`; the old redundant `cycle_time_ns` field is not part
of this device-specific schema.

Network reads never advance filters or recalculate measurements. Every Modbus
read during one publishing cycle observes the same immutable snapshot.

### Engineering conversions and calibration live in firmware

Both operating modes use the same firmware-side affine ADC calibration and
engineering conversions. This keeps Deimos and Modbus measurements identical
and allows Modbus clients to consume useful values without recreating the host
calculation graph.

Reusable no-std calculations live in `deimos_shared`; host APIs delegate to the
same functions. Runtime calculation choices favor bounded, branch-light `f32`
evaluation:

- K-type voltage/temperature conversions use offline-fitted regular-grid cubic
  B-splines evaluated by `interpn`.
- The Pt100 forward function uses the IEC 60751 Callendar–Van Dusen equation.
- The realtime Pt100 inverse uses one global polynomial rather than a runtime
  root solve or range-partitioned functions.
- Finite values outside fitted ranges use endpoint-tangent linear
  extrapolation.

The generated fits are validated with per-span local error maximization rather
than only a dense grid. The accepted maximum temperature or
temperature-equivalent error is 0.01 K. Recorded worst cases are 0.00939837 K
for K-type temperature-to-voltage, 0.00902328 K for voltage-to-temperature, and
0.00687762 K for the global Pt100 inverse. All accepted fits are monotonic over
their supported domains.

The calibration image is a 145-byte shared `ByteStruct` containing one
`firmware_calibrated` byte and 18 `(slope, offset)` `f32` pairs. It deliberately
has no protocol magic. The firmware build script uses the shared `Calibration`
type for size, identity generation, serialization, deserialization, and
validation, so the binary format has one definition.

An absent installed calibration produces valid identity coefficients with
`firmware_calibrated = 0`. Normal Deimos operation reports this state in the
device-specific configuring response and refuses normal control; calibration
collection requires it. An uncalibrated unit does not listen on TCP port 502.
This prevents both accidental double calibration and silent uncalibrated use.
Calibration is not writable through Modbus.

### Synchronous cycle-owned sampling

One SysTick interrupt owns acquisition, engineering conversion, publication,
and communications while Operating. Ordinary sampler state passes completed
ADC, counter, and frequency values between those steps; no sampled-data atomics
or interrupt-to-interrupt double buffer is required.

For reporting rate `f_report`, firmware chooses the integer number of complete
ADC groups which fit at or below the fixed 9 kHz target:

```text
samples_per_cycle = max(1, floor(9000 Hz / f_report))
sample_rate        = samples_per_cycle * f_report
```

At two or more samples per cycle, the ADC path includes an IIR cutoff at
`0.4 * f_report`. Above the natural boundary near 4.5 kHz, one sample fits and
the direct path omits the IIR. Both paths retain the fractional-delay filter
used to align sequential ADC conversions. The board-temperature channel has a
separate 1 Hz filter evaluated once per published snapshot.

The sample count, actual sample rate, cutoff, and Bode-analysis inputs come from
one policy in `deimos_shared`, preventing firmware and offline analysis from
silently diverging. Counter-rate compile-time assertions protect 16-bit timer
unwrapping at every accepted sample interval.

Operating entry takes a real ADC group and initializes all filter histories to
steady state before the first publication. This avoids long startup transients,
especially in board temperature and thermocouple compensation.

SysTick supplies the scheduled coarse timebase. The Cortex-M7 DWT cycle counter
provides sub-cycle timestamps and true deadline margin, including overruns.
This avoids another hardware timer and gives acquisition and publication times
core-cycle resolution.

### Rate changes reuse operating entry

Cycle-rate changes are expected to be rare, so they use the existing operating
entrypoint rather than adding live filter mutation machinery. After accepting a
rate-changing write, firmware:

1. enqueues the Modbus response;
2. preserves the complete output state, timeout, and timing corrections;
3. exits the operating IRQ scope;
4. re-enters `OperatingModbus` with the new period; and
5. rebuilds and steadily primes the selected sampling filters.

This also resets sampled encoder, pulse-counter, and frequency state. Clients
must wait for the write response before issuing another request. The visible
reset/reprime is accepted in exchange for keeping the common hot path simple.

## Packet identity and validation

Every rev7 Deimos `ByteStruct` packet begins with a packet- and
direction-specific `u32` magic:

| Packet | Magic |
| --- | ---: |
| Binding input | `0xD7B10001` |
| Binding output | `0xD7B10002` |
| Configuring input | `0xD7C00001` |
| Configuring output | `0xD7C00002` |
| Operating input | `0xD7000001` |
| Operating snapshot | `0xD7000002` |

Receive paths require the exact packet length, expected magic, valid enum
encodings, and safety-relevant field ranges before state or outputs can change.
Invalid configuration receives an explicit NACK. Rejected packets do not reset
loss of contact. Exceptional measured floating-point values are allowed to
propagate through snapshots rather than adding hot-path finite checks.

Pre-rev7 packet layouts remain unchanged. Discovery sends both legacy and rev7
binding requests and selects device-specific configuring and operating packets
after the model is known. There is no obsolete-rev7 decoder: existing rev7
hardware was reflashed and recalibrated as a coordinated firmware/software
update.

The snapshot magic is also the first field of the Modbus snapshot register
image, giving clients an identity and layout check without adding a separate
Modbus schema field.

## Modbus/TCP behavior

### Transport and framing

Calibrated firmware listens on TCP port 502 while in `Binding`. It supports one
TCP client and accepts and echoes every Unit Identifier, including 0 and 255.
All Modbus addresses in this document are zero-based protocol addresses.

The six-byte MBAP prefix is staged first. Once its length is valid, firmware
reads only through that ADU's declared end, so TCP fragmentation is retained
across cycles and a following pipelined ADU remains aligned in the socket.
Invalid protocol IDs or impossible/oversized MBAP lengths abort only that TCP
connection; the listener then starts a new connection at known alignment.
Supported requests with semantic errors receive standard Modbus exceptions.

Request and response storage is fixed at 256 bytes and does not allocate.
`rmodbus` handles the supported standard functions other than FC23. FC23 is
implemented locally because the selected `rmodbus` release does not support it;
the local implementation should be replaced with the library implementation
when it becomes available and provides the same bounded behavior.

### Supported function codes

- FC03 reads holding registers.
- FC04 reads input registers.
- FC16 atomically writes complete holding-register fields.
- FC23 atomically writes one holding block and reads one holding block in the
  same ADU.

Unsupported functions receive `Illegal Function`. Read-only, unsupported,
split-field, or cross-gap accesses receive `Illegal Data Address`. Invalid
values receive `Illegal Data Value`. A write is validated into a complete
candidate configuration before application, so rejection cannot partially
alter outputs or timing.

### Defaults, retention, and loss of contact

The first accepted request starts from:

- 10 Hz publishing;
- 600 loss-of-contact cycles, equal to one minute at 10 Hz;
- safe output values; and
- zero period and phase corrections.

A first write overlays only the complete included fields. Reads never change
outputs. Unlike Deimos mode, Modbus retains the last successfully written PWM,
DAC, and GPIO values across later reads and partial writes. This is intentional:
any accepted FC03, FC04, FC16, or FC23 request renews output authority and resets
the single loss-of-contact counter. A read-only client may therefore maintain
the last outputs until it stops communicating or explicitly changes them.

The timeout is expressed only as publishing cycles; there is no separate TCP,
sample, or publication timeout. On timeout or session loss, firmware leaves
Operating, returns to connection setup, and applies the existing safe-output
behavior.

### Timing corrections

The requested signed period correction persists until replaced. The signed
phase correction is consumed by the next scheduled publication interval and
then reads back as zero. Firmware saturating-adds both values and clamps the
applied correction to `+/-10%` of the nominal period. Raw period values remain
readable; the clamp is an internal timing-safety boundary, not a write
validation limit.

## Register encoding

Each 16-bit register is transmitted in network byte order. Multi-register
values place the most-significant 16-bit register first. `f32` fields carry
their IEEE-754 bit patterns; signed integers use two's-complement
representation.

### Input registers (FC04)

Read address 0, count 75 for the complete coherent engineering snapshot.
Partial in-range reads are supported, but only the full block is the
synchronized-sample contract.

| Address | Count | Type | Field | Units / shape |
| ---: | ---: | --- | --- | --- |
| 0 | 2 | `u32` | `magic` | `0xD7000002` |
| 2 | 4 | `u64` | `metrics.id` | snapshot count |
| 6 | 4 | `i64` | `metrics.sent_time_ns` | ns |
| 10 | 4 | `u64` | `metrics.last_input_id` | last accepted transaction ID |
| 14 | 4 | `i64` | `metrics.last_input_received_time_ns` | ns |
| 18 | 4 | `i64` | `metrics.cycle_time_margin_ns` | ns |
| 22 | 4 | `i64` | `sample_time_ns` | ADC acquisition-start time, ns |
| 26 | 2 | `f32` | `module_bus_current_a` | A |
| 28 | 2 | `f32` | `module_bus_voltage_v` | V |
| 30 | 2 | `f32` | `board_temperature_k` | K |
| 32 | 8 | `f32[4]` | `current_4_20_a` | A, channels 0..3 |
| 40 | 6 | `f32[3]` | `rtd_resistance_ohm` | ohm, channels 0..2 |
| 46 | 4 | `f32[2]` | `thermocouple_temperature_k` | K, channels 0..1 |
| 50 | 12 | `f32[6]` | `voltage_v` | V, channels 0..5 |
| 62 | 4 | `i64` | `encoder` | counts |
| 66 | 4 | `i64` | `pulse_counter` | counts |
| 70 | 4 | `f32[2]` | `frequency_meas` | Hz, channels 0..1 |
| 74 | 1 | `u16` | `gpio` | input bits 0..1 |

Other FC04 ranges receive `Illegal Data Address`.

### Holding registers (FC03, FC16, and FC23)

Read address 0, count 35 for the complete configuration and diagnostic block.
Writes must start and end at complete scalar-field boundaries and remain wholly
inside the base-configuration block (0..2), output block (6..26), or
timing-correction block (27..34).

| Address | Count | Access | Type | Field | Valid values |
| ---: | ---: | --- | --- | --- | --- |
| 0 | 2 | R/W | `f32` | cycle rate | finite, 5..500 Hz |
| 2 | 1 | R/W | `u16` | loss-of-contact limit | 1..65535 cycles |
| 3 | 2 | R | `u32` | current cycle period | ns |
| 5 | 1 | R | `u16` | current loss counter | cycles |
| 6 | 8 | R/W | `f32[4]` | PWM duty fractions | finite, 0..1 |
| 14 | 8 | R/W | `u32[4]` | PWM frequencies | nonzero Hz |
| 22 | 4 | R/W | `f32[2]` | DAC voltages | finite, 0..2.5 V |
| 26 | 1 | R/W | `u16` | GPIO outputs | bits 0..3 only |
| 27 | 4 | R/W | `i64` | requested period delta | ns; persistent, internally clamped |
| 31 | 4 | R/W | `i64` | requested phase delta | ns; one cycle, internally clamped |

The coherent snapshot is mirrored into a read-only holding-register window for
FC23:

| Address | Count | Access | Type | Field |
| ---: | ---: | --- | --- | --- |
| 256 (`0x0100`) | 75 | R | snapshot layout above | coherent engineering snapshot |

Holding address 256 corresponds to input address 0 and the mapping continues
through holding address 330. FC03 may read this mirror. Writes into the mirror
or the unsupported gap at addresses 35..255 receive `Illegal Data Address`.

## Recommended synchronized-control transaction

FC23 Read/Write Multiple Registers is the preferred cyclic interface:

- read address 256, count 75;
- write one complete writable block; and
- include the next output or timing command in the write data.

Firmware acquires and constructs the immutable snapshot before processing
Modbus traffic. FC23 validates and retains its write, returns that cycle's
beginning-of-cycle snapshot, and applies the resulting output state afterward.
This matches the Deimos sense/respond/act contract.

If two queued ADUs are serviced in one cycle, both read the same snapshot.
Accepted writes compose in TCP stream order, and only the final retained output
state is applied after request processing. An FC23 read of the configuration
block reflects its accepted write because standard FC23 write processing
precedes the holding-register read; the snapshot mirror remains the already
latched measurement.

## Realtime boundedness

TCP stream service is explicitly budgeted so a malformed peer, request storm,
or stalled receiver cannot monopolize the operating interrupt. Each Binding or
Modbus operating cycle performs at most:

- two complete ADU parses;
- four TCP receive-buffer calls;
- four TCP transmit-buffer calls;
- two Ethernet-frame receives; and
- two Ethernet-frame transmits.

Each ADU uses at most two receive calls and two transmit calls. A response held
under TCP backpressure prevents consumption of its following request until the
response has entered the smoltcp TX ring. Register-copy loops are bounded by
protocol constants: at most 125 read registers and 21 writable registers.
There are no resynchronization scans or unbounded packet-draining loops in the
Modbus-capable interrupt paths.

The two-ADU allowance lets a short backlog drain rather than permanently
remaining one cycle behind, while preserving a fixed worst-case execution
budget. Supported cyclic controllers should nevertheless keep one FC23 request
outstanding.

## Verification evidence

Calibrated SN3 passed the production protocol matrix, including:

- fragmented MBAP and PDU delivery;
- arbitrary Unit Identifiers;
- standard exception responses;
- connection-local malformed-frame rejection and clean reconnect;
- close during a partial ADU;
- repeated close/reconnect cycles;
- retained responses under finite adversarial backpressure;
- 5 Hz and 500 Hz endpoint operation;
- the default 62-second application timeout; and
- persistent period and one-shot phase corrections at the 500 Hz endpoint.

At 500 Hz, sustained synchronized reads completed at 500 responses/s with a
recorded minimum returned board margin of 56.695 us. A maximally shortened
1.8 ms corrected interval retained 45.010 us minimum returned margin. The
finite 2,688-response backpressure run drained every host-accepted response
without a reported deadline miss.

The instrumented compliant endpoint run used 15,956 bytes of the 88,888-byte
MSP reservation and retained 47.530 us minimum sample-plus-communication
margin. The production ELF recorded with the completed synchronous design used
152,496 bytes across all sections, including 29,088 bytes of the 64 KiB ITCM
region and 2,568 bytes of DTCM.

Deimos upper-rate characterization found the strict board deadline between
8.85 and 8.9 kHz. The supported maximum remains 8 kHz, where the recorded
worst-case board margin was 18.085 us, rather than treating the barely positive
8.85 kHz result as usable headroom. Packet loss was host/Ethernet dependent and
nonmonotonic, so supported limits are based primarily on board margin and
timestamp continuity.

## Operational constraints and deferred work

- Deploy Modbus/TCP only on a trusted, isolated control network. The protocol
  has no authentication, encryption, TLS, or Modbus Security layer.
- Connect only one client and normally keep one FC23 request outstanding.
- Read all 75 snapshot registers when a synchronized measurement group is
  required.
- Use one complete-field write when outputs must change atomically.
- Treat cycle-rate changes as maintenance operations and tolerate their
  documented filter reprime and counter reset.
- Modbus exposes only the latest snapshot; it does not provide history or an
  event buffer.
- Sample, publication, filter, and timeout rates are intentionally not
  independent settings.
- Dynamic magnitude, phase, folded-alias, and noise characterization remains
  deferred until an automated programmable signal-generator setup is
  available.
- Acquisition timestamps still merit a direct GPIO-marker comparison under
  nominal and actively corrected timing.
- Any future split of acquisition and communication across interrupts or cores
  requires a new explicit handoff design; the current ordinary-state ownership
  relies on their sequential execution in one SysTick handler.

## References

1. Modbus Organization, *MODBUS Application Protocol Specification V1.1b3*,
   2012.
2. Modbus Organization, *MODBUS Messaging on TCP/IP Implementation Guide
   V1.0b*, 2006.
3. IEC 60751, *Industrial platinum resistance thermometers and platinum
   temperature sensors*.
4. G. W. Burns et al., *Temperature-Electromotive Force Reference Functions
   and Tables for the Letter-Designated Thermocouple Types Based on the ITS-90*,
   NIST Monograph 175, 1993, doi: 10.6028/NIST.MONO.175.
