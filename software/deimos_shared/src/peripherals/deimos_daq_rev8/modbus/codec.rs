//! Scalar and array codecs for Modbus register and byte order.

pub(super) fn put_u32(registers: &mut [u16], position: &mut usize, value: u32) {
    registers[*position] = (value >> 16) as u16;
    registers[*position + 1] = value as u16;
    *position += 2;
}

pub(super) fn put_u64(registers: &mut [u16], position: &mut usize, value: u64) {
    registers[*position] = (value >> 48) as u16;
    registers[*position + 1] = (value >> 32) as u16;
    registers[*position + 2] = (value >> 16) as u16;
    registers[*position + 3] = value as u16;
    *position += 4;
}

pub(super) fn put_f32(registers: &mut [u16], position: &mut usize, value: f32) {
    put_u32(registers, position, value.to_bits());
}

/// Append an `f32` array with shape `(N,)` to a register buffer.
pub(super) fn put_f32_array<const N: usize>(
    registers: &mut [u16],
    position: &mut usize,
    values: &[f32; N],
) {
    for &value in values {
        put_f32(registers, position, value);
    }
}

/// Append a `u32` array with shape `(N,)` to a register buffer.
pub(super) fn put_u32_array<const N: usize>(
    registers: &mut [u16],
    position: &mut usize,
    values: &[u32; N],
) {
    for &value in values {
        put_u32(registers, position, value);
    }
}

/// Append one `u32` to a byte buffer in Modbus network byte order.
pub(super) fn put_u32_bytes(bytes: &mut [u8], position: &mut usize, value: u32) {
    bytes[*position..*position + 4].copy_from_slice(&value.to_be_bytes());
    *position += 4;
}

/// Append one `f32` bit pattern to a byte buffer in Modbus network byte order.
pub(super) fn put_f32_bytes(bytes: &mut [u8], position: &mut usize, value: f32) {
    put_u32_bytes(bytes, position, value.to_bits());
}

/// Append an `f32` array with shape `(N,)` in Modbus network byte order.
pub(super) fn put_f32_array_bytes<const N: usize>(
    bytes: &mut [u8],
    position: &mut usize,
    values: &[f32; N],
) {
    for &value in values {
        put_f32_bytes(bytes, position, value);
    }
}

/// Append one `u64` to a byte buffer in Modbus network byte order.
pub(super) fn put_u64_bytes(bytes: &mut [u8], position: &mut usize, value: u64) {
    bytes[*position..*position + 8].copy_from_slice(&value.to_be_bytes());
    *position += 8;
}

pub(super) fn read_u32(values: &[u16], write_start: usize, field_start: usize) -> u32 {
    let offset = field_start - write_start;
    (u32::from(values[offset]) << 16) | u32::from(values[offset + 1])
}

pub(super) fn read_u64(values: &[u16], write_start: usize, field_start: usize) -> u64 {
    let offset = field_start - write_start;
    (u64::from(values[offset]) << 48)
        | (u64::from(values[offset + 1]) << 32)
        | (u64::from(values[offset + 2]) << 16)
        | u64::from(values[offset + 3])
}

pub(super) fn take_u32(registers: &[u16], position: &mut usize) -> u32 {
    let value = (u32::from(registers[*position]) << 16) | u32::from(registers[*position + 1]);
    *position += 2;
    value
}

pub(super) fn take_f32(registers: &[u16], position: &mut usize) -> f32 {
    f32::from_bits(take_u32(registers, position))
}

/// Decode an `f32` array with shape `(N,)` from consecutive registers.
pub(super) fn take_f32_array<const N: usize>(registers: &[u16], position: &mut usize) -> [f32; N] {
    core::array::from_fn(|_| take_f32(registers, position))
}

pub(super) fn take_u64(registers: &[u16], position: &mut usize) -> u64 {
    let value = (u64::from(registers[*position]) << 48)
        | (u64::from(registers[*position + 1]) << 32)
        | (u64::from(registers[*position + 2]) << 16)
        | u64::from(registers[*position + 3]);
    *position += 4;
    value
}
