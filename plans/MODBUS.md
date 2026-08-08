# Modbus/TCP design

## Scope

The DAQ has two operating states:

- `OperatingDeimos` exchanges synchronous UDP command/snapshot roundtrips with
  the Deimos controller.
- `OperatingModbus` exposes the same measurements and retained outputs through
  one Modbus/TCP connection.

Both call the same `Board::operate` implementation. Acquisition, filtering,
calibration, conversion, timing, and output handling therefore remain
protocol-independent.

```text
Connecting -> Binding -> Configuring -> OperatingDeimos
                    \
                     +-- valid Modbus request -> OperatingModbus

OperatingDeimos -- loss of contact --> Connecting
OperatingModbus -- loss of contact --> Connecting
OperatingModbus -- cycle-rate change --> OperatingModbus(new configuration)
```

A valid FC03, FC04, FC16, or FC23 request in `Binding` selects Modbus operation.
The request is retained and answered from the first operating snapshot, so a
read-only client can start Modbus mode. If UDP binding wins, the TCP listener is
closed before Deimos configuration.

Modbus publishing rates are limited to 5 Hz through 500 Hz to preserve time for
bounded TCP processing.

## Shared measurement path

`OperatingSnapshot` is the source for both the Deimos response and Modbus
register image. It contains final `f32` engineering values, counter and digital
inputs, acquisition time, publication time, and operating metrics. Host
software upcasts values to `f64`. External RTD channels publish resistance;
software performs the final resistance-to-temperature conversion.

`sample_time_ns` identifies the start of the contributing ADC group, while
`metrics.sent_time_ns` identifies publication. Neither includes filter-delay
correction. `metrics.id` provides snapshot ordering. Every read within a
publishing cycle sees the same immutable snapshot.

Calibration and engineering conversions run in firmware in both modes.
Reusable no-std calculations live in `deimos_shared`. Devices report their
calibration state during configuration and reject Modbus operation when a
required calibration is absent. Calibration is not writable through Modbus.

Firmware latches one engineering snapshot per publishing cycle before network
processing. Protocol handling never advances acquisition or filters.

Cycle-rate writes preserve outputs, timeout, and timing corrections, then
re-enter `OperatingModbus` and rebuild rate-dependent state. Sampled counters
reset. Clients must receive the write response before sending another request.

Each snapshot begins with a device-specific magic value. Receive paths validate
lengths, identity, enums, and safety-relevant ranges before changing state or
outputs. Invalid requests do not reset loss of contact.

## Modbus/TCP behavior

### Transport and framing

Firmware listens on TCP port 502 while in `Binding`. It supports one client and
accepts every Unit Identifier, including 0 and 255. Register addresses below
are zero-based protocol addresses.

Firmware stages the six-byte MBAP prefix, validates its declared length, and
reads through exactly one ADU. Fragmented ADUs remain buffered across cycles;
pipelined ADUs stay aligned. Invalid protocol IDs or impossible lengths close
only that connection. Request and response storage is fixed at 256 bytes.

`rmodbus` handles FC03, FC04, and FC16. FC23 is local until `rmodbus` provides a
bounded implementation with compatible behavior.

### Functions and errors

- FC03 reads holding registers.
- FC04 reads input registers.
- FC16 atomically writes complete holding fields.
- FC23 atomically writes one holding block and reads another in one ADU.

Unsupported functions return `Illegal Function`. Read-only, split-field,
cross-gap, and out-of-range accesses return `Illegal Data Address`. Invalid
values return `Illegal Data Value`. Writes are validated as a complete
candidate before application.

### Defaults and retained state

Initial Modbus configuration is:

- 10 Hz publishing;
- 600 loss-of-contact cycles, or one minute;
- safe outputs; and
- zero period and phase corrections.

A first write overlays its complete included fields. Successful writes retain
PWM, DAC, and GPIO outputs until replaced. Reads do not change outputs. Any
accepted FC03, FC04, FC16, or FC23 request renews authority and resets the one
loss-of-contact counter. Timeout or connection loss returns the board to
connection setup and safe outputs.

The signed period correction persists. The signed phase correction applies to
the next publication interval and then reads as zero. Applied corrections are
clamped to `+/-10%` of the nominal period; register values themselves are not
write-clamped.

## Register encoding

Registers use network byte order. Multi-register values place the most
significant register first. `f32` fields contain IEEE-754 bits; signed integers
use two's complement.

### Input registers (FC04)

Read address 0, count 75 for one coherent snapshot. Partial in-range reads are
valid, but the full block is the synchronized-sample contract.

| Address | Count | Type | Field | Units / shape |
| ---: | ---: | --- | --- | --- |
| 0 | 2 | `u32` | `magic` | `0xD7000002` |
| 2 | 4 | `u64` | `metrics.id` | snapshot count |
| 6 | 4 | `i64` | `metrics.sent_time_ns` | ns |
| 10 | 4 | `u64` | `metrics.last_input_id` | last accepted transaction ID |
| 14 | 4 | `i64` | `metrics.last_input_received_time_ns` | ns |
| 18 | 4 | `i64` | `metrics.cycle_time_margin_ns` | ns |
| 22 | 4 | `i64` | `sample_time_ns` | acquisition start, ns |
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

Other FC04 ranges return `Illegal Data Address`.

### Holding registers (FC03, FC16, and FC23)

Read address 0, count 35 for the complete configuration and diagnostics.
Writes must contain complete fields wholly within the base configuration
(0..2), outputs (6..26), or timing corrections (27..34).

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
| 27 | 4 | R/W | `i64` | requested period delta | ns; persistent |
| 31 | 4 | R/W | `i64` | requested phase delta | ns; one cycle |

The coherent snapshot is mirrored for FC03 and FC23:

| Address | Count | Access | Type | Field |
| ---: | ---: | --- | --- | --- |
| 256 (`0x0100`) | 75 | R | snapshot layout above | coherent snapshot |

Holding addresses 256..330 correspond to input addresses 0..74. Writes to the
mirror or addresses 35..255 return `Illegal Data Address`.

## Synchronized control

FC23 is the preferred cyclic transaction:

- read address 256, count 75;
- write one complete writable block; and
- include the next output or timing command in the write data.

Firmware latches the snapshot before processing Modbus traffic. FC23 retains
its validated write while forming the response, then applies the resulting
outputs after request processing. This matches the Deimos sense/respond/act
contract.

If two ADUs are handled in one cycle, both read the same snapshot. Their writes
compose in stream order, and the final state is applied after processing. An
FC23 configuration read reflects its preceding write; the snapshot mirror
remains the already-latched measurement.

## Realtime bounds

Each Binding or Modbus operating cycle permits at most:

- two complete ADU parses;
- four TCP receive-buffer calls;
- four TCP transmit-buffer calls;
- two Ethernet-frame receives; and
- two Ethernet-frame transmits.

Each ADU uses at most two receive and two transmit calls. TCP backpressure
blocks consumption of the following request until the response enters the TX
ring. Register loops are bounded to 125 read and 21 writable registers. There
are no resynchronization scans or unbounded packet-draining loops.

Two ADUs per cycle allow a short backlog to drain. Cyclic clients should still
keep one FC23 request outstanding.

## Operational constraints

- Use Modbus/TCP only on a trusted control network; it has no authentication or
  encryption.
- Connect one client and normally keep one FC23 request outstanding.
- Read all 75 snapshot registers for synchronized measurements.
- Treat cycle-rate changes as maintenance operations because they reprime
  rate-dependent state and reset sampled counters.
- Modbus exposes only the latest snapshot, not history or events.

## References

1. Modbus Organization, *MODBUS Application Protocol Specification V1.1b3*,
   2012.
2. Modbus Organization, *MODBUS Messaging on TCP/IP Implementation Guide
   V1.0b*, 2006.
