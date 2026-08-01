use super::super::{ModbusInitialConfig, OperatingSnapshot, MIN_CYCLE_RATE_HZ};
use super::codec::{put_f32, put_u32};
use super::*;
use crate::peripherals::deimos_daq_rev7::OPERATING_SNAPSHOT_MAGIC;

#[test]
fn synchronized_holding_snapshot_window_is_disjoint_and_complete() {
    assert!(HOLDING_SNAPSHOT_START >= HOLDING_REGISTER_COUNT);
    assert_eq!(
        HOLDING_SNAPSHOT_REGISTER_COUNT,
        SNAPSHOT_INPUT_REGISTER_COUNT
    );
    assert_eq!(MODBUS_READ_WRITE_MULTIPLE_REGISTERS_FUNCTION, 0x17);
}

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
    assert_eq!(&registers[22..26], &[0x1122, 0x3344, 0x5566, 0x7788]);
    assert_eq!(&registers[26..28], &[0x3f80, 0x0000]);
    assert_eq!(&registers[62..66], &[0xffff, 0xffff, 0xffff, 0xfffe]);
    assert_eq!(registers[74], 3);

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

    let minimum_rate_bits = (MIN_CYCLE_RATE_HZ as f32).to_bits();
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
    let insufficient_rate_bits = (MIN_CYCLE_RATE_HZ as f32 - 1.0).to_bits();
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
