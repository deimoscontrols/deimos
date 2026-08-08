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

## Shared measurement path

`OperatingSnapshot` is the source for both the Deimos response and Modbus
register image. It contains final engineering values, device I/O, acquisition
time, publication time, and operating metrics. Host adapters expose those
values through the software peripheral API.

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

Each snapshot begins with a device-specific magic value. Receive paths validate
lengths, identity, enums, and safety-relevant ranges before changing state or
outputs. Invalid requests do not reset loss of contact.

## Module boundaries

Each `deimos_shared::peripherals` device module is the no-std source of truth
for packets, register maps, codecs, and validation. Firmware adds transport and
hardware behavior; software uses the shared definitions and documents each
device map.

## Modbus/TCP behavior

### Transport and framing

Firmware listens on TCP port 502 while in `Binding`. It supports one client and
accepts every Unit Identifier, including 0 and 255. Device register maps use
zero-based protocol addresses.

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

### Retained state and loss of contact

Each device defines its default publishing rate, timeout, and safe outputs.
Successful writes retain output values until replaced; reads do not change
them. Any accepted FC03, FC04, FC16, or FC23 request renews authority and resets
the loss-of-contact counter. Timeout or connection loss returns the device to
connection setup and safe outputs.

Where timing corrections are exposed, period correction persists and phase
correction applies once. Firmware bounds applied corrections to preserve cycle
timing margin.

## Synchronized control

FC23 is the preferred cyclic transaction:

- read the device's complete coherent snapshot mirror;
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
ring. Register loops are bounded by protocol and device-map constants. There
are no resynchronization scans or unbounded packet-draining loops.

Two ADUs per cycle allow a short backlog to drain. Cyclic clients should still
keep one FC23 request outstanding.

## Operational constraints

- Use Modbus/TCP only on a trusted control network; it has no authentication or
  encryption.
- Connect one client and normally keep one FC23 request outstanding.
- Read the complete device snapshot block for synchronized measurements.
- Modbus exposes only the latest snapshot, not history or events.

## References

1. Modbus Organization, *MODBUS Application Protocol Specification V1.1b3*,
   2012.
2. Modbus Organization, *MODBUS Messaging on TCP/IP Implementation Guide
   V1.0b*, 2006.
