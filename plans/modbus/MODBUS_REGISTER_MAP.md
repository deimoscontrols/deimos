# Rev7 Modbus/TCP Register Map

The calibrated rev7 firmware listens on TCP port 502 while it is in `Binding`.
The first accepted FC03, FC04, FC16, or FC23 request selects Modbus operation. An
uncalibrated firmware image does not listen on port 502.

All addresses below are zero-based protocol addresses. Each 16-bit register is
transmitted in network byte order. `f32` values use their IEEE-754 bit pattern;
32- and 64-bit values place the most-significant 16-bit register first. Signed
64-bit values use two's-complement representation.

The server accepts and echoes every Unit Identifier, including 0 and 255. It
supports one TCP client. Cyclic controllers should keep one FC23 request
outstanding; the server can process up to two complete ADUs per publishing
cycle so a short pipeline or backlog can drain without unbounded IRQ work.

## Input registers (FC04)

Read address 0, count 75 to obtain the complete synchronized engineering
snapshot. Every value in that block comes from one firmware publication cycle.

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

`sample_time_ns` is captured immediately before the first ADC conversion group
which contributes to the published filtered values. It is not corrected for
fractional-delay or low-pass-filter group delay.

Other FC04 ranges receive `Illegal Data Address` (exception 02). A partial
in-range read is supported, but a full-block read is the synchronization
contract.

## Holding registers (FC03 / FC16 / FC23)

Read address 0, count 35 to obtain the complete current configuration and
diagnostic block.

FC03 reads any in-range block. FC16 writes a block without returning register
data. FC23 writes one block and reads one block in the same ADU. Writes must
begin and end on complete scalar-field boundaries and must stay entirely within
the base-configuration block (0..2), output block (6..26), or timing-correction
block (27..34). This makes every accepted multi-field write atomic.

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

The engineering snapshot is mirrored into a separate, read-only holding window
for standard FC23 access:

| Address | Count | Access | Type | Field |
| ---: | ---: | --- | --- | --- |
| 256 (`0x0100`) | 75 | R | snapshot layout above | coherent engineering snapshot |

The field at holding address 256 therefore corresponds to input-register
address 0, holding address 258 corresponds to input-register address 2, and so
on through holding address 330. FC03 may also read this mirror. Writes into the
snapshot window or the unsupported gap at addresses 35..255 receive `Illegal
Data Address`.

## Recommended synchronized control transaction (FC23)

Use FC23 Read/Write Multiple Registers with:

- read address 256 and read count 75;
- a write address and count covering one complete writable block; and
- the next output or timing command in the write data.

The firmware samples and constructs one immutable engineering snapshot before
processing Modbus traffic in a publishing cycle. FC23 atomically validates and
retains its write, responds with that cycle's snapshot, and then applies the
resulting output state. This is the Modbus equivalent of the Deimos
sense/respond/act roundtrip and is the recommended interface for synchronous
control. Standard FC23 performs the write before the holding-register read; the
snapshot mirror remains the already-latched beginning-of-cycle measurement,
while an FC23 read of the configuration block reflects the accepted write.

Omitted writable fields retain their last accepted values. Reads never alter
outputs. A rejected write leaves the complete configuration and output state
unchanged. Read-only, unsupported, split-field, and cross-gap writes receive
`Illegal Data Address` (exception 02); invalid field values receive `Illegal
Data Value` (exception 03).

The first accepted request uses defaults of 10 Hz, 600 loss-of-contact cycles
(one minute), zero timing corrections, and safe outputs. A first FC16 write
overlays only its included fields on those defaults; FC23 behaves the same for
its write block.

Each accepted FC03, FC04, FC16, or FC23 request resets loss of contact.

The requested period delta persists until another accepted write replaces it.
The requested phase delta is consumed by the next scheduled publication
interval and then reads back as zero. Firmware saturating-adds the two signed
requests and clamps their combined applied correction to `+/-10%` of the
nominal cycle period. Raw requested values are retained for period readback;
the clamp is an internal timing-safety boundary rather than a Modbus write
validation limit.

Changing the cycle rate is a rare maintenance operation. The board enqueues the
FC16 or FC23 response, preserves the complete output, timeout, and
timing-correction state, and re-enters the shared operating implementation at
the new period. A pending phase request is consumed once after re-entry rather
than on the discarded old-rate interval. The existing filter-cutoff update then
skips one sampler iteration and resets encoder/pulse-counter sampling state.
Wait for the write response before issuing another request.

## Framing and bounded service

The MBAP length field delimits each ADU in the TCP byte stream. The firmware
stages the six-byte prefix first and then reads no farther than the declared ADU
end, so a partial TCP read is retained without consuming a following request.
Invalid protocol IDs or impossible/oversized MBAP lengths abort that connection;
the listening socket then starts the next connection at known byte alignment.

Each binding or Modbus operating cycle performs at most:

- four TCP receive-buffer calls;
- four TCP transmit-buffer calls;
- two Ethernet frame receives; and
- two Ethernet frame transmits.

At most two complete ADUs are parsed per cycle. Each ADU uses at most two
receive calls and two transmit calls, and a retained response under TCP
backpressure prevents another request from being processed until that response
is queued. Both ADUs observe the same immutable beginning-of-cycle snapshot;
accepted configuration writes compose in stream order, and only the final
retained output state is applied. Register-copy loops have protocol constant
bounds (at most 125 read registers and 21 writable registers); there are no
resynchronization scans or unbounded packet-draining loops in the
Modbus-capable IRQ paths.
