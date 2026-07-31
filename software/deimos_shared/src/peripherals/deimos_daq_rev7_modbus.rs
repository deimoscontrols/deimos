//! Modbus/TCP register layout for rev7 engineering snapshots and control state.
//!
//! Addresses are zero-based protocol addresses. Multi-register scalars place
//! the most-significant 16-bit register first, and each register is transmitted
//! in Modbus network byte order by the transport layer.
//!
//! # Input registers (FC04)
//!
//! Read address 0, count [`SNAPSHOT_INPUT_REGISTER_COUNT`] (79), to obtain one
//! coherent engineering snapshot. Partial in-range reads are supported, but a
//! full-block read is the synchronization contract.
//!
//! | Address | Count | Type | Field | Units / shape |
//! | ---: | ---: | --- | --- | --- |
//! | 0 | 2 | `u32` | `magic` | `0xD7000002` |
//! | 2 | 4 | `u64` | `metrics.id` | snapshot count |
//! | 6 | 4 | `i64` | `metrics.cycle_time_ns` | ns |
//! | 10 | 4 | `i64` | `metrics.sent_time_ns` | ns |
//! | 14 | 4 | `u64` | `metrics.last_input_id` | last accepted transaction ID |
//! | 18 | 4 | `i64` | `metrics.last_input_received_time_ns` | ns |
//! | 22 | 4 | `i64` | `metrics.cycle_time_margin_ns` | ns |
//! | 26 | 4 | `i64` | `sample_time_ns` | ADC acquisition-start time, ns |
//! | 30 | 2 | `f32` | `module_bus_current_a` | A |
//! | 32 | 2 | `f32` | `module_bus_voltage_v` | V |
//! | 34 | 2 | `f32` | `board_temperature_k` | K |
//! | 36 | 8 | `f32[4]` | `current_4_20_a` | A, channels 0..3 |
//! | 44 | 6 | `f32[3]` | `rtd_resistance_ohm` | ohm, channels 0..2 |
//! | 50 | 4 | `f32[2]` | `thermocouple_temperature_k` | K, channels 0..1 |
//! | 54 | 12 | `f32[6]` | `voltage_v` | V, channels 0..5 |
//! | 66 | 4 | `i64` | `encoder` | counts |
//! | 70 | 4 | `i64` | `pulse_counter` | counts |
//! | 74 | 4 | `f32[2]` | `frequency_meas` | Hz, channels 0..1 |
//! | 78 | 1 | `u16` | `gpio` | input bits 0..1 |
//!
//! `sample_time_ns` is captured immediately before the first ADC conversion
//! group contributing to the published filtered values. It is not corrected
//! for fractional-delay or low-pass-filter group delay.
//!
//! # Holding registers (FC03 / FC16)
//!
//! Read address 0, count [`HOLDING_REGISTER_COUNT`] (35), to obtain the complete
//! current configuration and diagnostic block. FC03 may read any in-range
//! block. FC16 writes must cover complete scalar fields and remain within one
//! atomic writable block: base configuration (0..2), outputs (6..26), or timing
//! corrections (27..34).
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

use super::{ModbusInitialConfig, OperatingSnapshot, DAC_CHANNEL_COUNT, PWM_CHANNEL_COUNT};
use crate::states::OperatingMetrics;

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
/// Total number of readable holding registers.
pub const HOLDING_REGISTER_COUNT: u16 = 35;

/// Slowest supported Modbus publishing rate in `Hz`.
pub const MODBUS_MIN_CYCLE_RATE_HZ: f32 = super::REV7_MIN_CYCLE_RATE_HZ as f32;
/// Fastest supported Modbus publishing rate in `Hz`.
pub const MODBUS_MAX_CYCLE_RATE_HZ: f32 = 500.0;

/// Maximum register count in one standard Modbus read request.
pub const MODBUS_MAX_READ_REGISTERS: u16 = 125;
/// Maximum register count in one standard Modbus multiple-write request.
pub const MODBUS_MAX_WRITE_REGISTERS: u16 = 123;
/// Maximum writable holding-register span in the rev7 map.
pub const MAX_HOLDING_WRITE_REGISTERS: usize = 21;

const _: () = assert!(SNAPSHOT_INPUT_REGISTER_COUNT <= MODBUS_MAX_READ_REGISTERS);
const _: () = assert!(MAX_HOLDING_WRITE_REGISTERS <= MODBUS_MAX_WRITE_REGISTERS as usize);

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

/// Encode one coherent snapshot into its complete Modbus input-register image.
///
/// Args:
///   snapshot: Engineering snapshot to encode.
///
/// Returns:
///   Register values with shape `(SNAPSHOT_INPUT_REGISTER_COUNT,)`, ordered by
///   zero-based input-register address.
pub fn snapshot_input_registers(
    snapshot: &OperatingSnapshot,
) -> [u16; SNAPSHOT_INPUT_REGISTER_COUNT as usize] {
    let mut bytes = [0_u8; SNAPSHOT_INPUT_BYTE_COUNT];
    write_snapshot_input_register_bytes(snapshot, &mut bytes);
    let mut registers = [0_u16; SNAPSHOT_INPUT_REGISTER_COUNT as usize];
    for (register, pair) in registers.iter_mut().zip(bytes.chunks_exact(2)) {
        *register = u16::from_be_bytes([pair[0], pair[1]]);
    }
    registers
}

/// Encode a complete snapshot directly into Modbus/TCP register payload bytes.
///
/// This is the common encoding source for the register-valued host API and the
/// firmware's full-snapshot fast path. Writing directly into the response
/// avoids converting 79 intermediate `u16` values back into network byte order
/// in the realtime communication interrupt.
///
/// Args:
///   snapshot: Engineering snapshot to encode.
///   bytes: Destination with shape `(SNAPSHOT_INPUT_BYTE_COUNT,)` in Modbus
///     register and network byte order.
#[inline(always)]
pub fn write_snapshot_input_register_bytes(
    snapshot: &OperatingSnapshot,
    bytes: &mut [u8; SNAPSHOT_INPUT_BYTE_COUNT],
) {
    let mut position = 0;

    put_u32_bytes(bytes, &mut position, snapshot.magic);
    put_u64_bytes(bytes, &mut position, snapshot.metrics.id);
    put_u64_bytes(bytes, &mut position, snapshot.metrics.cycle_time_ns as u64);
    put_u64_bytes(bytes, &mut position, snapshot.metrics.sent_time_ns as u64);
    put_u64_bytes(bytes, &mut position, snapshot.metrics.last_input_id);
    put_u64_bytes(
        bytes,
        &mut position,
        snapshot.metrics.last_input_received_time_ns as u64,
    );
    put_u64_bytes(
        bytes,
        &mut position,
        snapshot.metrics.cycle_time_margin_ns as u64,
    );
    put_u64_bytes(bytes, &mut position, snapshot.sample_time_ns as u64);

    put_f32_bytes(bytes, &mut position, snapshot.module_bus_current_a);
    put_f32_bytes(bytes, &mut position, snapshot.module_bus_voltage_v);
    put_f32_bytes(bytes, &mut position, snapshot.board_temperature_k);
    put_f32_array_bytes(bytes, &mut position, &snapshot.current_4_20_a);
    put_f32_array_bytes(bytes, &mut position, &snapshot.rtd_resistance_ohm);
    put_f32_array_bytes(bytes, &mut position, &snapshot.thermocouple_temperature_k);
    put_f32_array_bytes(bytes, &mut position, &snapshot.voltage_v);
    put_u64_bytes(bytes, &mut position, snapshot.encoder as u64);
    put_u64_bytes(bytes, &mut position, snapshot.pulse_counter as u64);
    put_f32_array_bytes(bytes, &mut position, &snapshot.frequency_meas);
    bytes[position] = 0;
    bytes[position + 1] = snapshot.gpio;
    position += 2;

    debug_assert_eq!(position, SNAPSHOT_INPUT_BYTE_COUNT);
}

/// Decode one complete Modbus input-register block into its shared snapshot type.
///
/// Args:
///   registers: Register values with shape
///     `(SNAPSHOT_INPUT_REGISTER_COUNT,)`, already decoded from network byte
///     order and beginning at [`SNAPSHOT_INPUT_START`].
///
/// Returns:
///   Validated engineering snapshot, or a length/content error.
pub fn snapshot_from_input_registers(
    registers: &[u16],
) -> Result<OperatingSnapshot, SnapshotDecodeError> {
    if registers.len() != SNAPSHOT_INPUT_REGISTER_COUNT as usize {
        return Err(SnapshotDecodeError::InvalidLength);
    }
    let mut position = 0;
    let snapshot = OperatingSnapshot {
        magic: take_u32(registers, &mut position),
        metrics: OperatingMetrics {
            id: take_u64(registers, &mut position),
            cycle_time_ns: take_u64(registers, &mut position) as i64,
            sent_time_ns: take_u64(registers, &mut position) as i64,
            last_input_id: take_u64(registers, &mut position),
            last_input_received_time_ns: take_u64(registers, &mut position) as i64,
            cycle_time_margin_ns: take_u64(registers, &mut position) as i64,
        },
        sample_time_ns: take_u64(registers, &mut position) as i64,
        module_bus_current_a: take_f32(registers, &mut position),
        module_bus_voltage_v: take_f32(registers, &mut position),
        board_temperature_k: take_f32(registers, &mut position),
        current_4_20_a: take_f32_array(registers, &mut position),
        rtd_resistance_ohm: take_f32_array(registers, &mut position),
        thermocouple_temperature_k: take_f32_array(registers, &mut position),
        voltage_v: take_f32_array(registers, &mut position),
        encoder: take_u64(registers, &mut position) as i64,
        pulse_counter: take_u64(registers, &mut position) as i64,
        frequency_meas: take_f32_array(registers, &mut position),
        gpio: registers[position]
            .try_into()
            .map_err(|_| SnapshotDecodeError::InvalidSnapshot)?,
    };
    position += 1;
    debug_assert_eq!(position, SNAPSHOT_INPUT_REGISTER_COUNT as usize);

    if !snapshot.is_valid() {
        return Err(SnapshotDecodeError::InvalidSnapshot);
    }
    Ok(snapshot)
}

/// Encode current Modbus configuration, diagnostics, and outputs as holding registers.
///
/// Args:
///   config: Current publishing period, timeout, requested timing corrections,
///     and retained output settings.
///   loss_of_contact_counter: Current number of consecutive cycles without an
///     accepted request.
///
/// Returns:
///   Register values with shape `(HOLDING_REGISTER_COUNT,)`, ordered by
///   zero-based holding-register address.
pub fn holding_registers(
    config: &ModbusInitialConfig,
    loss_of_contact_counter: u16,
) -> [u16; HOLDING_REGISTER_COUNT as usize] {
    let mut registers = [0_u16; HOLDING_REGISTER_COUNT as usize];
    let mut position = HOLDING_CYCLE_RATE_HZ as usize;
    put_f32(
        &mut registers,
        &mut position,
        1.0e9_f32 / config.dt_ns as f32,
    );
    registers[HOLDING_LOSS_OF_CONTACT_LIMIT as usize] = config.loss_of_contact_limit;
    position = HOLDING_CYCLE_PERIOD_NS as usize;
    put_u32(&mut registers, &mut position, config.dt_ns);
    registers[HOLDING_LOSS_OF_CONTACT_COUNTER as usize] = loss_of_contact_counter;

    position = HOLDING_PWM_DUTY_FRAC as usize;
    put_f32_array(&mut registers, &mut position, &config.outputs.pwm_duty_frac);
    position = HOLDING_PWM_FREQUENCY_HZ as usize;
    put_u32_array(&mut registers, &mut position, &config.outputs.pwm_freq_hz);
    position = HOLDING_DAC_V as usize;
    put_f32_array(&mut registers, &mut position, &config.outputs.dac_v);
    registers[HOLDING_GPIO as usize] = u16::from(config.outputs.gpio);
    position = HOLDING_PERIOD_DELTA_NS as usize;
    put_u64(&mut registers, &mut position, config.period_delta_ns as u64);
    position = HOLDING_PHASE_DELTA_NS as usize;
    put_u64(&mut registers, &mut position, config.phase_delta_ns as u64);
    registers
}

/// Apply one complete-field holding-register write to a retained configuration.
///
/// The function accepts only the writable base-configuration range `0..3`, a
/// contiguous range within the output block `6..27`, or a contiguous range
/// within the timing-correction block `27..35`. Both ends must coincide with
/// scalar-field boundaries. The candidate is validated in full before it is
/// returned, so rejected writes cannot partially alter state.
///
/// Args:
///   current: Configuration to preserve for omitted fields.
///   address: Zero-based first holding-register address.
///   values: Network-decoded register values with shape `(register_count,)`.
///
/// Returns:
///   Updated complete configuration, or the corresponding Modbus semantic error.
pub fn apply_holding_write(
    current: ModbusInitialConfig,
    address: u16,
    values: &[u16],
) -> Result<ModbusInitialConfig, HoldingWriteError> {
    if values.is_empty() || values.len() > MAX_HOLDING_WRITE_REGISTERS {
        return Err(HoldingWriteError::IllegalDataAddress);
    }
    let end = usize::from(address)
        .checked_add(values.len())
        .ok_or(HoldingWriteError::IllegalDataAddress)?;
    let start = usize::from(address);

    let in_config = start < 3 && end <= 3;
    let in_outputs =
        start >= usize::from(HOLDING_PWM_DUTY_FRAC) && end <= usize::from(HOLDING_PERIOD_DELTA_NS);
    let in_timing =
        start >= usize::from(HOLDING_PERIOD_DELTA_NS) && end <= usize::from(HOLDING_REGISTER_COUNT);
    if !(in_config || in_outputs || in_timing)
        || !is_writable_field_start(start)
        || !is_writable_field_end(end)
    {
        return Err(HoldingWriteError::IllegalDataAddress);
    }

    let mut candidate = current;
    if field_is_covered(start, end, HOLDING_CYCLE_RATE_HZ as usize, 2) {
        let rate_hz = f32::from_bits(read_u32(values, start, HOLDING_CYCLE_RATE_HZ as usize));
        if !rate_hz.is_finite()
            || !(MODBUS_MIN_CYCLE_RATE_HZ..=MODBUS_MAX_CYCLE_RATE_HZ).contains(&rate_hz)
        {
            return Err(HoldingWriteError::IllegalDataValue);
        }
        candidate.dt_ns = (1.0e9_f32 / rate_hz + 0.5) as u32;
    }
    if field_is_covered(start, end, HOLDING_LOSS_OF_CONTACT_LIMIT as usize, 1) {
        candidate.loss_of_contact_limit =
            values[usize::from(HOLDING_LOSS_OF_CONTACT_LIMIT) - start];
        if candidate.loss_of_contact_limit == 0 {
            return Err(HoldingWriteError::IllegalDataValue);
        }
    }
    if field_is_covered(start, end, HOLDING_PERIOD_DELTA_NS as usize, 4) {
        candidate.period_delta_ns =
            read_u64(values, start, HOLDING_PERIOD_DELTA_NS as usize) as i64;
    }
    if field_is_covered(start, end, HOLDING_PHASE_DELTA_NS as usize, 4) {
        candidate.phase_delta_ns = read_u64(values, start, HOLDING_PHASE_DELTA_NS as usize) as i64;
    }

    for index in 0..PWM_CHANNEL_COUNT {
        let field = usize::from(HOLDING_PWM_DUTY_FRAC) + 2 * index;
        if field_is_covered(start, end, field, 2) {
            candidate.outputs.pwm_duty_frac[index] = f32::from_bits(read_u32(values, start, field));
        }
    }
    for index in 0..PWM_CHANNEL_COUNT {
        let field = usize::from(HOLDING_PWM_FREQUENCY_HZ) + 2 * index;
        if field_is_covered(start, end, field, 2) {
            candidate.outputs.pwm_freq_hz[index] = read_u32(values, start, field);
        }
    }
    for index in 0..DAC_CHANNEL_COUNT {
        let field = usize::from(HOLDING_DAC_V) + 2 * index;
        if field_is_covered(start, end, field, 2) {
            candidate.outputs.dac_v[index] = f32::from_bits(read_u32(values, start, field));
        }
    }
    if field_is_covered(start, end, HOLDING_GPIO as usize, 1) {
        candidate.outputs.gpio = values[usize::from(HOLDING_GPIO) - start]
            .try_into()
            .map_err(|_| HoldingWriteError::IllegalDataValue)?;
    }

    if !candidate.outputs.is_valid() {
        return Err(HoldingWriteError::IllegalDataValue);
    }
    Ok(candidate)
}

fn put_u32(registers: &mut [u16], position: &mut usize, value: u32) {
    registers[*position] = (value >> 16) as u16;
    registers[*position + 1] = value as u16;
    *position += 2;
}

fn put_u64(registers: &mut [u16], position: &mut usize, value: u64) {
    registers[*position] = (value >> 48) as u16;
    registers[*position + 1] = (value >> 32) as u16;
    registers[*position + 2] = (value >> 16) as u16;
    registers[*position + 3] = value as u16;
    *position += 4;
}

fn put_f32(registers: &mut [u16], position: &mut usize, value: f32) {
    put_u32(registers, position, value.to_bits());
}

/// Append an `f32` array with shape `(N,)` to a register buffer.
fn put_f32_array<const N: usize>(registers: &mut [u16], position: &mut usize, values: &[f32; N]) {
    for &value in values {
        put_f32(registers, position, value);
    }
}

/// Append a `u32` array with shape `(N,)` to a register buffer.
fn put_u32_array<const N: usize>(registers: &mut [u16], position: &mut usize, values: &[u32; N]) {
    for &value in values {
        put_u32(registers, position, value);
    }
}

/// Append one `u32` to a byte buffer in Modbus network byte order.
fn put_u32_bytes(bytes: &mut [u8], position: &mut usize, value: u32) {
    bytes[*position..*position + 4].copy_from_slice(&value.to_be_bytes());
    *position += 4;
}

/// Append one `f32` bit pattern to a byte buffer in Modbus network byte order.
fn put_f32_bytes(bytes: &mut [u8], position: &mut usize, value: f32) {
    put_u32_bytes(bytes, position, value.to_bits());
}

/// Append an `f32` array with shape `(N,)` in Modbus network byte order.
fn put_f32_array_bytes<const N: usize>(bytes: &mut [u8], position: &mut usize, values: &[f32; N]) {
    for &value in values {
        put_f32_bytes(bytes, position, value);
    }
}

/// Append one `u64` to a byte buffer in Modbus network byte order.
fn put_u64_bytes(bytes: &mut [u8], position: &mut usize, value: u64) {
    bytes[*position..*position + 8].copy_from_slice(&value.to_be_bytes());
    *position += 8;
}

fn read_u32(values: &[u16], write_start: usize, field_start: usize) -> u32 {
    let offset = field_start - write_start;
    (u32::from(values[offset]) << 16) | u32::from(values[offset + 1])
}

fn read_u64(values: &[u16], write_start: usize, field_start: usize) -> u64 {
    let offset = field_start - write_start;
    (u64::from(values[offset]) << 48)
        | (u64::from(values[offset + 1]) << 32)
        | (u64::from(values[offset + 2]) << 16)
        | u64::from(values[offset + 3])
}

fn take_u32(registers: &[u16], position: &mut usize) -> u32 {
    let value = (u32::from(registers[*position]) << 16) | u32::from(registers[*position + 1]);
    *position += 2;
    value
}

fn take_f32(registers: &[u16], position: &mut usize) -> f32 {
    f32::from_bits(take_u32(registers, position))
}

/// Decode an `f32` array with shape `(N,)` from consecutive registers.
fn take_f32_array<const N: usize>(registers: &[u16], position: &mut usize) -> [f32; N] {
    core::array::from_fn(|_| take_f32(registers, position))
}

fn take_u64(registers: &[u16], position: &mut usize) -> u64 {
    let value = (u64::from(registers[*position]) << 48)
        | (u64::from(registers[*position + 1]) << 32)
        | (u64::from(registers[*position + 2]) << 16)
        | u64::from(registers[*position + 3]);
    *position += 4;
    value
}

fn field_is_covered(start: usize, end: usize, field_start: usize, width: usize) -> bool {
    start <= field_start && end >= field_start + width
}

fn is_writable_field_start(address: usize) -> bool {
    matches!(
        address,
        0 | 2 | 6 | 8 | 10 | 12 | 14 | 16 | 18 | 20 | 22 | 24 | 26 | 27 | 31
    )
}

fn is_writable_field_end(address: usize) -> bool {
    matches!(
        address,
        2 | 3 | 8 | 10 | 12 | 14 | 16 | 18 | 20 | 22 | 24 | 26 | 27 | 31 | 35
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::peripherals::deimos_daq_rev7::OPERATING_SNAPSHOT_MAGIC;

    #[test]
    fn snapshot_registers_are_most_significant_register_first() {
        let mut snapshot = OperatingSnapshot::default();
        snapshot.metrics.id = 0x0123_4567_89ab_cdef;
        snapshot.sample_time_ns = 0x1122_3344_5566_7788;
        snapshot.module_bus_current_a = 1.0;
        snapshot.encoder = -2;
        snapshot.gpio = 3;

        let registers = snapshot_input_registers(&snapshot);
        assert_eq!(
            &registers[0..2],
            &[
                (OPERATING_SNAPSHOT_MAGIC >> 16) as u16,
                OPERATING_SNAPSHOT_MAGIC as u16
            ]
        );
        assert_eq!(&registers[2..6], &[0x0123, 0x4567, 0x89ab, 0xcdef]);
        assert_eq!(&registers[26..30], &[0x1122, 0x3344, 0x5566, 0x7788]);
        assert_eq!(&registers[30..32], &[0x3f80, 0x0000]);
        assert_eq!(&registers[66..70], &[0xffff, 0xffff, 0xffff, 0xfffe]);
        assert_eq!(registers[78], 3);

        let decoded = snapshot_from_input_registers(&registers).unwrap();
        assert_eq!(decoded.magic, snapshot.magic);
        assert_eq!(decoded.metrics.id, snapshot.metrics.id);
        assert_eq!(decoded.sample_time_ns, snapshot.sample_time_ns);
        assert_eq!(decoded.module_bus_current_a.to_bits(), 1.0_f32.to_bits());
        assert_eq!(decoded.encoder, -2);
        assert_eq!(decoded.gpio, 3);
    }

    #[test]
    fn snapshot_decoder_rejects_wrong_shape_magic_and_gpio() {
        let valid = snapshot_input_registers(&OperatingSnapshot::default());
        assert!(matches!(
            snapshot_from_input_registers(&valid[..valid.len() - 1]),
            Err(SnapshotDecodeError::InvalidLength)
        ));

        let mut wrong_magic = valid;
        wrong_magic[0] ^= 1;
        assert!(matches!(
            snapshot_from_input_registers(&wrong_magic),
            Err(SnapshotDecodeError::InvalidSnapshot)
        ));

        let mut invalid_gpio = valid;
        invalid_gpio[SNAPSHOT_INPUT_REGISTER_COUNT as usize - 1] = 0x0100;
        assert!(matches!(
            snapshot_from_input_registers(&invalid_gpio),
            Err(SnapshotDecodeError::InvalidSnapshot)
        ));
    }

    #[test]
    fn holding_registers_round_trip_writable_fields() {
        let current = ModbusInitialConfig::default();
        let mut values = [0_u16; 21];
        let mut position = 0;
        for value in [0.25_f32, 0.5, 0.75, 1.0] {
            put_f32(&mut values, &mut position, value);
        }
        for value in [100_u32, 200, 300, 400] {
            put_u32(&mut values, &mut position, value);
        }
        for value in [1.25_f32, 2.5] {
            put_f32(&mut values, &mut position, value);
        }
        values[position] = 0x0a;

        let updated = apply_holding_write(current, HOLDING_PWM_DUTY_FRAC, &values).unwrap();
        assert_eq!(updated.outputs.pwm_duty_frac, [0.25, 0.5, 0.75, 1.0]);
        assert_eq!(updated.outputs.pwm_freq_hz, [100, 200, 300, 400]);
        assert_eq!(updated.outputs.dac_v, [1.25, 2.5]);
        assert_eq!(updated.outputs.gpio, 0x0a);

        let registers = holding_registers(&updated, 7);
        assert_eq!(&registers[6..27], &values);
        assert_eq!(registers[HOLDING_LOSS_OF_CONTACT_COUNTER as usize], 7);
    }

    #[test]
    fn timing_delta_holding_writes_are_signed_atomic_and_big_endian() {
        let current = ModbusInitialConfig::default();
        let values = [
            0x0123, 0x4567, 0x89ab, 0xcdef, 0xfedc, 0xba98, 0x7654, 0x3210,
        ];
        let updated = apply_holding_write(current, HOLDING_PERIOD_DELTA_NS, &values).unwrap();
        assert_eq!(updated.period_delta_ns, 0x0123_4567_89ab_cdef);
        assert_eq!(updated.phase_delta_ns, 0xfedc_ba98_7654_3210_u64 as i64);

        let registers = holding_registers(&updated, 0);
        assert_eq!(&registers[27..35], &values);
        assert_eq!(
            apply_holding_write(current, HOLDING_PERIOD_DELTA_NS + 1, &[0; 4]),
            Err(HoldingWriteError::IllegalDataAddress)
        );
        assert_eq!(
            apply_holding_write(current, HOLDING_GPIO, &[0; 5]),
            Err(HoldingWriteError::IllegalDataAddress)
        );
    }

    #[test]
    fn holding_writes_preserve_omitted_fields_and_validate_atomically() {
        let current = ModbusInitialConfig::default();
        let rate_bits = MODBUS_MAX_CYCLE_RATE_HZ.to_bits();
        let updated = apply_holding_write(
            current,
            HOLDING_CYCLE_RATE_HZ,
            &[(rate_bits >> 16) as u16, rate_bits as u16, 123],
        )
        .unwrap();
        assert_eq!(updated.dt_ns, 2_000_000);
        assert_eq!(updated.loss_of_contact_limit, 123);
        assert_eq!(updated.outputs, current.outputs);

        let minimum_rate_bits = MODBUS_MIN_CYCLE_RATE_HZ.to_bits();
        let minimum = apply_holding_write(
            current,
            HOLDING_CYCLE_RATE_HZ,
            &[(minimum_rate_bits >> 16) as u16, minimum_rate_bits as u16],
        )
        .unwrap();
        assert_eq!(minimum.dt_ns, 200_000_000);

        let excessive_rate_bits = (MODBUS_MAX_CYCLE_RATE_HZ + 1.0).to_bits();
        assert_eq!(
            apply_holding_write(
                current,
                HOLDING_CYCLE_RATE_HZ,
                &[
                    (excessive_rate_bits >> 16) as u16,
                    excessive_rate_bits as u16,
                ],
            ),
            Err(HoldingWriteError::IllegalDataValue),
        );
        let insufficient_rate_bits = (MODBUS_MIN_CYCLE_RATE_HZ - 1.0).to_bits();
        assert_eq!(
            apply_holding_write(
                current,
                HOLDING_CYCLE_RATE_HZ,
                &[
                    (insufficient_rate_bits >> 16) as u16,
                    insufficient_rate_bits as u16,
                ],
            ),
            Err(HoldingWriteError::IllegalDataValue),
        );
        assert_eq!(
            apply_holding_write(current, 1, &[0]),
            Err(HoldingWriteError::IllegalDataAddress)
        );
        assert_eq!(
            apply_holding_write(current, 2, &[1, 2, 3, 4]),
            Err(HoldingWriteError::IllegalDataAddress)
        );
        assert_eq!(
            apply_holding_write(current, HOLDING_GPIO, &[0x10]),
            Err(HoldingWriteError::IllegalDataValue)
        );
        assert_eq!(
            apply_holding_write(current, HOLDING_LOSS_OF_CONTACT_LIMIT, &[0]),
            Err(HoldingWriteError::IllegalDataValue)
        );
        let nan = f32::NAN.to_bits();
        assert_eq!(
            apply_holding_write(
                current,
                HOLDING_PWM_DUTY_FRAC,
                &[(nan >> 16) as u16, nan as u16],
            ),
            Err(HoldingWriteError::IllegalDataValue)
        );
    }
}
