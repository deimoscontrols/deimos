#![doc = include_str!("modbus_register_map.md")]

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
pub const SNAPSHOT_INPUT_REGISTER_COUNT: u16 = 79;
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
const _: () = assert!(HOLDING_SNAPSHOT_START >= HOLDING_REGISTER_COUNT);
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
    /// The supplied block does not contain exactly 79 registers.
    InvalidLength,
    /// The decoded packet magic or engineering-value invariants are invalid.
    InvalidSnapshot,
}

#[cfg(test)]
mod tests;
