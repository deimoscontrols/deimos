# Deimos DAQ Rev7 Modbus/TCP register map

Addresses are zero-based protocol addresses. Multi-register scalars place
the most-significant 16-bit register first, and registers use network byte
order. `f32` fields contain IEEE-754 bits; signed integers use two's
complement.

## Input registers (FC04)

Read address 0, count 75 for one coherent engineering snapshot. Partial
in-range reads are supported, but the full block is the synchronized-sample
contract.

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

`sample_time_ns` is captured before the first ADC conversion group
contributing to the snapshot. It is not corrected for fractional-delay or
low-pass-filter group delay.

## Holding registers (FC03, FC16, and FC23)

Read address 0, count 35 for the complete configuration and diagnostic
block. FC03 may read any in-range block. FC16 and the write portion of FC23
must cover complete fields within one writable block: base configuration
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
| 27 | 4 | R/W | `i64` | requested period delta | ns; persistent, internally clamped |
| 31 | 4 | R/W | `i64` | requested phase delta | ns; one cycle, internally clamped |

The coherent snapshot is mirrored in read-only holding registers 256..330
(`0x0100`..`0x014A`) with the same layout as input registers 0..74. Writes
to the mirror or the unsupported gap at 35..255 return `Illegal Data
Address`.

## Synchronized control (FC23)

FC23 Read/Write Multiple Registers is the recommended cyclic interface.

Read address 256, count 75 while writing one complete writable block. The
response contains the most recent snapshot; accepted outputs are applied afterward. Does not return configuration registers 0..34.

If two ADUs are serviced in one cycle, both return the same snapshot, and both writes are applied in order and committed in the final combined state. Omitted fields retain their values; rejected writes change nothing.

Period correction persists until replaced, while phase correction applies once and then is cleared for the next cycle.
Timing corrections are clamped to +/-10% of the nominal period.
