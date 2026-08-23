//! Coherent engineering snapshot register encoding and decoding.

use super::super::OperatingSnapshot;
use super::{codec::*, *};
use crate::peripherals::deimos_daq_rev7::packets::OperatingSnapshotMetrics;
use crate::states::ByteStructLen;

// The Modbus image preserves the complete packet schema but widens its final
// `u8` GPIO field to one 16-bit register. Keep that relationship checked in
// every build instead of verifying the final cursor only in debug builds.
const _: () = assert!(SNAPSHOT_INPUT_BYTE_COUNT == OperatingSnapshot::BYTE_LEN + 1);

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
/// avoids converting 75 intermediate `u16` values back into network byte order
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
        metrics: OperatingSnapshotMetrics {
            id: take_u64(registers, &mut position),
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

    if !snapshot.is_valid() {
        return Err(SnapshotDecodeError::InvalidSnapshot);
    }
    Ok(snapshot)
}
