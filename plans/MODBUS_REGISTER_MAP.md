# Rev7 Modbus/TCP Register Map

The calibrated rev7 firmware listens on TCP port 502 while it is in `Binding`.
The first accepted FC03, FC04, or FC16 request selects Modbus operation. An
uncalibrated firmware image does not listen on port 502.

All addresses below are zero-based protocol addresses. Each 16-bit register is
transmitted in network byte order. `f32` values use their IEEE-754 bit pattern;
32- and 64-bit values place the most-significant 16-bit register first. Signed
64-bit values use two's-complement representation.

The server accepts and echoes every Unit Identifier, including 0 and 255. It
supports one TCP client and one outstanding request. A client must receive the
complete response before sending another request.

## Input registers (FC04)

Read address 0, count 79 to obtain the complete synchronized engineering
snapshot. Every value in that block comes from one firmware publication cycle.

| Address | Count | Type | Field | Units / shape |
| ---: | ---: | --- | --- | --- |
| 0 | 2 | `u32` | `magic` | `0xD7000002` |
| 2 | 4 | `u64` | `metrics.id` | snapshot count |
| 6 | 4 | `i64` | `metrics.cycle_time_ns` | ns |
| 10 | 4 | `i64` | `metrics.sent_time_ns` | ns |
| 14 | 4 | `u64` | `metrics.last_input_id` | last accepted transaction ID |
| 18 | 4 | `i64` | `metrics.last_input_received_time_ns` | ns |
| 22 | 4 | `i64` | `metrics.cycle_time_margin_ns` | ns |
| 26 | 4 | `i64` | `sample_time_ns` | ADC acquisition-start time, ns |
| 30 | 2 | `f32` | `module_bus_current_a` | A |
| 32 | 2 | `f32` | `module_bus_voltage_v` | V |
| 34 | 2 | `f32` | `board_temperature_k` | K |
| 36 | 8 | `f32[4]` | `current_4_20_a` | A, channels 0..3 |
| 44 | 6 | `f32[3]` | `rtd_resistance_ohm` | ohm, channels 0..2 |
| 50 | 4 | `f32[2]` | `thermocouple_temperature_k` | K, channels 0..1 |
| 54 | 12 | `f32[6]` | `voltage_v` | V, channels 0..5 |
| 66 | 4 | `i64` | `encoder` | counts |
| 70 | 4 | `i64` | `pulse_counter` | counts |
| 74 | 4 | `f32[2]` | `frequency_meas` | Hz, channels 0..1 |
| 78 | 1 | `u16` | `gpio` | input bits 0..1 |

`sample_time_ns` is captured immediately before the first ADC conversion group
which contributes to the published filtered values. It is not corrected for
fractional-delay or low-pass-filter group delay.

Other FC04 ranges receive `Illegal Data Address` (exception 02). A partial
in-range read is supported, but a full-block read is the synchronization
contract.

## Holding registers (FC03 / FC16)

FC03 reads any in-range block. FC16 is the only supported write function.
Writes must begin and end on complete scalar-field boundaries and must stay
entirely within either the configuration block (0..2) or output block (6..26).
This makes every accepted multi-output write atomic.

| Address | Count | Access | Type | Field | Valid values |
| ---: | ---: | --- | --- | --- | --- |
| 0 | 2 | R/W | `f32` | cycle rate | finite, 4..5000 Hz |
| 2 | 1 | R/W | `u16` | loss-of-contact limit | 1..65535 cycles |
| 3 | 2 | R | `u32` | current cycle period | ns |
| 5 | 1 | R | `u16` | current loss counter | cycles |
| 6 | 8 | R/W | `f32[4]` | PWM duty fractions | finite, 0..1 |
| 14 | 8 | R/W | `u32[4]` | PWM frequencies | nonzero Hz |
| 22 | 4 | R/W | `f32[2]` | DAC voltages | finite, 0..2.5 V |
| 26 | 1 | R/W | `u16` | GPIO outputs | bits 0..3 only |

Omitted writable fields retain their last accepted values. Reads never alter
outputs. A rejected write leaves the complete configuration and output state
unchanged. Read-only, unsupported, split-field, and cross-gap writes receive
`Illegal Data Address` (exception 02); invalid field values receive `Illegal
Data Value` (exception 03).

The first read uses defaults of 10 Hz, 600 loss-of-contact cycles (one minute),
and safe outputs. A first FC16 write overlays only its included fields on those
defaults. Each accepted FC03, FC04, or FC16 request resets loss of contact.

Changing the cycle rate is a rare maintenance operation. The board enqueues the
FC16 response, preserves the complete output and timeout state, and re-enters
the shared operating implementation at the new period. The existing filter
cutoff update then skips one sampler iteration and resets encoder/pulse-counter
sampling state. Wait for the write response before issuing another request.

## Framing and bounded service

The MBAP length field delimits each ADU in the TCP byte stream. The firmware
stages the six-byte prefix first and then reads no farther than the declared ADU
end, so a partial TCP read is retained without consuming a following request.
Invalid protocol IDs or impossible/oversized MBAP lengths abort that connection;
the listening socket then starts the next connection at known byte alignment.

Each binding or Modbus operating cycle performs at most:

- two TCP receive-buffer calls;
- two TCP transmit-buffer calls;
- two Ethernet frame receives; and
- two Ethernet frame transmits.

Only one complete ADU is parsed per cycle. Register-copy loops have protocol
constant bounds (at most 125 read registers and 21 writable registers); there
are no resynchronization scans or unbounded packet-draining loops in the
Modbus-capable IRQ paths.
