//! Modbus/TCP register layout for engineering snapshots and control state.
//!
//! Addresses are zero-based protocol addresses. Multi-register scalars place
//! the most-significant 16-bit register first, and each register is transmitted
//! in Modbus network byte order by the transport layer.
//!
//! # Input registers (FC04)
//!
//! Read address 0, count [`SNAPSHOT_INPUT_REGISTER_COUNT`] (75), to obtain one
//! coherent engineering snapshot. Partial in-range reads are supported, but a
//! full-block read is the synchronization contract.
//!
//! | Address | Count | Type | Field | Units / shape |
//! | ---: | ---: | --- | --- | --- |
//! | 0 | 2 | `u32` | `magic` | `0xD7000002` |
//! | 2 | 4 | `u64` | `metrics.id` | snapshot count |
//! | 6 | 4 | `i64` | `metrics.sent_time_ns` | ns |
//! | 10 | 4 | `u64` | `metrics.last_input_id` | last accepted transaction ID |
//! | 14 | 4 | `i64` | `metrics.last_input_received_time_ns` | ns |
//! | 18 | 4 | `i64` | `metrics.cycle_time_margin_ns` | ns |
//! | 22 | 4 | `i64` | `sample_time_ns` | ADC acquisition-start time, ns |
//! | 26 | 2 | `f32` | `module_bus_current_a` | A |
//! | 28 | 2 | `f32` | `module_bus_voltage_v` | V |
//! | 30 | 2 | `f32` | `board_temperature_k` | K |
//! | 32 | 8 | `f32[4]` | `current_4_20_a` | A, channels 0..3 |
//! | 40 | 6 | `f32[3]` | `rtd_resistance_ohm` | ohm, channels 0..2 |
//! | 46 | 4 | `f32[2]` | `thermocouple_temperature_k` | K, channels 0..1 |
//! | 50 | 12 | `f32[6]` | `voltage_v` | V, channels 0..5 |
//! | 62 | 4 | `i64` | `encoder` | counts |
//! | 66 | 4 | `i64` | `pulse_counter` | counts |
//! | 70 | 4 | `f32[2]` | `frequency_meas` | Hz, channels 0..1 |
//! | 74 | 1 | `u16` | `gpio` | input bits 0..1 |
//!
//! `sample_time_ns` is captured immediately before the first ADC conversion
//! group contributing to the published filtered values. It is not corrected
//! for fractional-delay or low-pass-filter group delay.
//!
//! # Holding registers (FC03 / FC16 / FC23)
//!
//! Read address 0, count [`HOLDING_REGISTER_COUNT`] (35), to obtain the complete
//! current configuration and diagnostic block. FC03 may read any in-range
//! block. FC16 and the write portion of FC23 must cover complete scalar fields
//! and remain within one atomic writable block: base configuration (0..2),
//! outputs (6..26), or timing corrections (27..34).
//!
//! | Address | Count | Access | Type | Field | Valid values |
//! | ---: | ---: | --- | --- | --- | --- |
//! | 0 | 2 | R/W | `f32` | cycle rate | finite, 5..500 Hz |
//! | 2 | 1 | R/W | `u16` | loss-of-contact limit | 1..65535 cycles |
//! | 3 | 2 | R | `u32` | current cycle period | ns |
//! | 5 | 1 | R | `u16` | current loss counter | cycles |
//! | 6 | 8 | R/W | `f32[4]` | PWM duty fractions | finite, 0..1 |
//! | 14 | 8 | R/W | `u32[4]` | PWM frequencies | nonzero Hz |
//! | 22 | 4 | R/W | `f32[2]` | DAC voltages | finite, 0..2.5 V |
//! | 26 | 1 | R/W | `u16` | GPIO outputs | bits 0..3 only |
//! | 27 | 4 | R/W | `i64` | requested period delta | ns; persistent, internally clamped |
//! | 31 | 4 | R/W | `i64` | requested phase delta | ns; one cycle, internally clamped |
//!
//! The coherent engineering snapshot is also mirrored into the read-only
//! holding-register window beginning at [`HOLDING_SNAPSHOT_START`] (`0x0100`).
//! Its 75-register field layout is identical to the FC04 input-register table,
//! with `0x0100` added to each address.
//!
//! # Synchronized control (FC23)
//!
//! FC23 Read/Write Multiple Registers is the recommended cyclic-control
//! interface. Its write block atomically updates one writable holding-register
//! block, while its read block returns the coherent snapshot mirror at address
//! `0x0100`, count 75. The returned snapshot was captured at the beginning of
//! the same publishing cycle; the newly written outputs are applied after the
//! request is accepted. This matches the Deimos sense/respond/act cycle.
//! If two queued ADUs are serviced in one publishing cycle, both read the same
//! immutable snapshot. Their accepted writes compose in TCP stream order, and
//! firmware applies only the final retained output state after request service.
//!
//! Omitted writable fields retain their values, and a rejected write changes
//! nothing. The requested period delta persists until replaced. The requested
//! phase delta is consumed by the next scheduled publication interval and then
//! reads back as zero. Firmware saturating-adds both requests and clamps the
//! applied correction to +/-10% of the nominal cycle period; raw period values
//! remain available for readback.
//!
//! All `f32` fields use their IEEE-754 bit pattern. Signed integers use two's
//! complement. Every multi-register value is ordered most-significant register
//! first.
//!
//! References:
//!   \[1\] Modbus Organization, *MODBUS Application Protocol Specification
//!   V1.1b3*, 2012.

mod codec;
mod holding;
mod snapshot;

pub use holding::{apply_holding_write, holding_registers};
pub use snapshot::{
    snapshot_from_input_registers, snapshot_input_registers, write_snapshot_input_register_bytes,
};

/// First input register occupied by the coherent engineering snapshot.
pub const SNAPSHOT_INPUT_START: u16 = 0;
/// Number of input registers occupied by one complete engineering snapshot.
pub const SNAPSHOT_INPUT_REGISTER_COUNT: u16 = 75;
/// Number of wire bytes occupied by one complete engineering snapshot register block.
pub const SNAPSHOT_INPUT_BYTE_COUNT: usize = SNAPSHOT_INPUT_REGISTER_COUNT as usize * 2;

/// First holding register of the writable cycle rate in `Hz` as IEEE-754 `f32`.
pub const HOLDING_CYCLE_RATE_HZ: u16 = 0;
/// Writable loss-of-contact limit in publishing cycles.
pub const HOLDING_LOSS_OF_CONTACT_LIMIT: u16 = 2;
/// Read-only current publishing period in `ns` as `u32`.
pub const HOLDING_CYCLE_PERIOD_NS: u16 = 3;
/// Read-only current loss-of-contact counter.
pub const HOLDING_LOSS_OF_CONTACT_COUNTER: u16 = 5;
/// First register of four writable PWM duty fractions as IEEE-754 `f32` values.
pub const HOLDING_PWM_DUTY_FRAC: u16 = 6;
/// First register of four writable PWM frequencies in `Hz` as `u32` values.
pub const HOLDING_PWM_FREQUENCY_HZ: u16 = 14;
/// First register of two writable DAC voltages in `V` as IEEE-754 `f32` values.
pub const HOLDING_DAC_V: u16 = 22;
/// Writable GPIO output bit field in the low byte of one register.
pub const HOLDING_GPIO: u16 = 26;
/// First register of the persistent requested period correction as `i64` `ns`.
pub const HOLDING_PERIOD_DELTA_NS: u16 = 27;
/// First register of the one-cycle requested phase correction as `i64` `ns`.
pub const HOLDING_PHASE_DELTA_NS: u16 = 31;
/// Number of holding registers in the configuration and diagnostic block.
pub const HOLDING_REGISTER_COUNT: u16 = 35;

/// First holding register of the read-only coherent engineering snapshot mirror.
pub const HOLDING_SNAPSHOT_START: u16 = 0x0100;
/// Number of holding registers occupied by the coherent engineering snapshot mirror.
pub const HOLDING_SNAPSHOT_REGISTER_COUNT: u16 = SNAPSHOT_INPUT_REGISTER_COUNT;

/// Function code for standard Read/Write Multiple Registers (FC23).
pub const MODBUS_READ_WRITE_MULTIPLE_REGISTERS_FUNCTION: u8 = 0x17;

/// Fastest supported Modbus publishing rate in `Hz`.
pub const MODBUS_MAX_CYCLE_RATE_HZ: f32 = 500.0;

/// Maximum register count in one standard Modbus read request.
pub const MODBUS_MAX_READ_REGISTERS: u16 = 125;
/// Maximum register count in one standard Modbus multiple-write request.
pub const MODBUS_MAX_WRITE_REGISTERS: u16 = 123;
/// Maximum write-register count in one standard FC23 request.
pub const MODBUS_MAX_READ_WRITE_WRITE_REGISTERS: u16 = 121;
/// Maximum writable holding-register span in the register map.
pub const MAX_HOLDING_WRITE_REGISTERS: usize = 21;

const _: () = assert!(SNAPSHOT_INPUT_REGISTER_COUNT <= MODBUS_MAX_READ_REGISTERS);
const _: () = assert!(HOLDING_SNAPSHOT_REGISTER_COUNT <= MODBUS_MAX_READ_REGISTERS);
const _: () = assert!(
    HOLDING_SNAPSHOT_START as u32 + HOLDING_SNAPSHOT_REGISTER_COUNT as u32 <= u16::MAX as u32 + 1
);
const _: () = assert!(MAX_HOLDING_WRITE_REGISTERS <= MODBUS_MAX_WRITE_REGISTERS as usize);
const _: () =
    assert!(MAX_HOLDING_WRITE_REGISTERS <= MODBUS_MAX_READ_WRITE_WRITE_REGISTERS as usize);

/// Semantic errors produced while validating a holding-register write.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HoldingWriteError {
    /// The requested range is read-only, unsupported, or splits a scalar field.
    IllegalDataAddress,
    /// At least one complete requested field contains an invalid value.
    IllegalDataValue,
}

/// Errors encountered while decoding a complete snapshot register block.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SnapshotDecodeError {
    /// The supplied block does not contain exactly 75 registers.
    InvalidLength,
    /// The decoded packet magic or engineering-value invariants are invalid.
    InvalidSnapshot,
}

#[cfg(test)]
mod tests;
