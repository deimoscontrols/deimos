//! Holding-register encoding and validated atomic writes.

use super::super::{ModbusInitialConfig, DAC_CHANNEL_COUNT, MIN_CYCLE_RATE_HZ, PWM_CHANNEL_COUNT};
use super::{codec::*, *};

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
            || rate_hz < MIN_CYCLE_RATE_HZ as f32
            || rate_hz > MODBUS_MAX_CYCLE_RATE_HZ
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
