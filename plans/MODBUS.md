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
time, publication time, and operating metrics.

`sample_time_ns` identifies the start of the contributing ADC group, while
`metrics.sent_time_ns` identifies publication.
`metrics.id` provides snapshot ordering.

Every read within a publishing cycle sees the same immutable snapshot.

Calibration and engineering conversions run in firmware in both modes.

Reusable no-std calculations live in `deimos_shared`.

Devices report their calibration state during configuration and reject
Modbus operation when a required calibration is absent.

Calibration is not writable except by flashing the firmware.

Firmware latches one engineering snapshot per publishing cycle before network processing.

Protocol handling never advances acquisition or filters.

Each snapshot begins with a device-specific magic value.

Receive paths validate lengths, identity, enums, and safety-relevant ranges
before changing state or outputs.

## Module boundaries

Each `deimos_shared::peripherals` device module is the no-std source of truth
for packets, register maps, codecs, and validation shared between firmware and
software.

## Modbus/TCP behavior

### Transport and framing

Firmware listens on TCP port 502 while in `Binding`.
It supports one client and accepts every Unit Identifier, including 0 and 255.

Device register maps use zero-based protocol addresses.

Firmware stages the six-byte MBAP prefix, validates its declared length, and
reads through exactly one ADU.

Fragmented ADUs remain buffered across cycles; pipelined ADUs stay aligned.

Invalid protocol IDs or impossible lengths close only that connection.

Request and response storage is fixed at 256 bytes.

`rmodbus` handles FC03, FC04, and FC16. FC23 is local until `rmodbus` implements it.

### Functions and errors

- FC03 reads holding registers (config, outputs, and diagnostics).
- FC04 reads input registers (measurements).
- FC16 atomically writes complete holding fields.
- FC23 atomically writes one holding block and reads another in one ADU.

Unsupported functions return `Illegal Function`.
Read-only, split-field, cross-gap, and out-of-range accesses return `Illegal Data Address`.
Invalid values return `Illegal Data Value`.

Writes are validated as a complete candidate before application.

### Retained state and loss of contact

Each device defines its default publishing rate, timeout, and safe outputs.

Successful writes retain output values until replaced.

Any accepted modbus request resets the loss-of-contact counter.

Invalid requests do not reset loss of contact.

Loss-of-contact timeout returns the device to Connecting and safe outputs.

Period correction persists and phase correction applies once.
Firmware clamps applied corrections to preserve cycle timing margin.

## Synchronized control

FC23 is the preferred cyclic transaction. This combines:

- bulk-read the device's complete coherent snapshot mirror (sensor readings)
- bulk-write the complete set of output values (PWM, etc.) and timing adjustment

Configuration registers (cycle dt, timeout, etc.) should be written and read
only as needed, not on every cycle.

If two ADUs are handled in one cycle, both read the same snapshot.

If the FC23 reads and writes the same block, the read will reflect the previous write.

## Realtime bounds

Each Binding or Modbus operating cycle permits at most:

- two complete ADU parses;
- four TCP receive-buffer calls;
- four TCP transmit-buffer calls;
- two Ethernet-frame receives; and
- two Ethernet-frame transmits.

Each ADU uses at most two receive and two transmit calls.

TCP backpressure blocks consumption of the following request
until the response enters the TX ring.

Two ADUs per cycle allow a short backlog to drain.
Cyclic clients should keep one FC23 request outstanding.

## Operational constraints

- Use Modbus/TCP only on a trusted control network; it has no authentication or
  encryption.
- Connect one client and normally keep one FC23 request outstanding.
- Read the complete device snapshot block for synchronized measurements.

## References

1. Modbus Organization, *MODBUS Application Protocol Specification V1.1b3*, 2012.
2. Modbus Organization, *MODBUS Messaging on TCP/IP Implementation Guide V1.0b*, 2006.
