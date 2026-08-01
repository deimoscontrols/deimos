use super::*;
use crate::{
    peripherals::PeripheralId,
    states::{AcknowledgeConfiguration, ByteStruct, ByteStructLen, ConfiguringInput},
};

fn round_trip<T>(value: T) -> T
where
    T: ByteStruct + ByteStructLen,
{
    let mut bytes = [0_u8; 256];
    value.write_bytes(&mut bytes[..T::BYTE_LEN]);
    T::read_bytes(&bytes[..T::BYTE_LEN])
}

#[test]
fn sampling_policy_derives_samplerate_and_iir_cutoff_from_cycle_rate() {
    let low_rate = adc_sampling_policy(f64::from(REV7_MIN_CYCLE_RATE_HZ)).unwrap();
    assert_eq!(low_rate.mode, AdcSamplingMode::Oversampled);
    assert_eq!(low_rate.samples_per_cycle, 1_800);
    assert_eq!(low_rate.sample_rate_hz, 9_000.0);
    assert_eq!(
        low_rate.iir_cutoff_hz,
        Some(f64::from(REV7_MIN_CYCLE_RATE_HZ) * 0.4),
    );
    assert_eq!(low_rate.iir_cutoff_ratio, Some(0.4 / 1_800.0));

    let intermediate_rate = adc_sampling_policy(2_500.0).unwrap();
    assert_eq!(intermediate_rate.mode, AdcSamplingMode::Oversampled);
    assert_eq!(intermediate_rate.samples_per_cycle, 3);
    assert_eq!(intermediate_rate.sample_rate_hz, 7_500.0);
    assert_eq!(intermediate_rate.iir_cutoff_hz, Some(1_000.0));
    assert_eq!(intermediate_rate.iir_cutoff_ratio, Some(2.0 / 15.0));

    // Adjacent integer-nanosecond periods straddle the natural 4.5 kHz
    // topology transition implied by the 9 kHz target.
    let below_cutover = adc_sampling_policy(1.0e9 / 222_223.0).unwrap();
    assert_eq!(below_cutover.mode, AdcSamplingMode::Oversampled);
    assert_eq!(below_cutover.samples_per_cycle, 2);
    assert_eq!(below_cutover.iir_cutoff_ratio, Some(0.2));

    let cutover = adc_sampling_policy(1.0e9 / 222_222.0).unwrap();
    assert_eq!(cutover.mode, AdcSamplingMode::Direct);
    assert_eq!(cutover.samples_per_cycle, 1);
    assert_eq!(cutover.sample_rate_hz, 1.0e9 / 222_222.0);
    assert_eq!(cutover.iir_cutoff_hz, None);
    assert_eq!(cutover.iir_cutoff_ratio, None);

    assert!(adc_sampling_policy(0.0).is_none());
    assert!(adc_sampling_policy(f64::NAN).is_none());
    assert!(adc_sampling_policy(f64::MIN_POSITIVE).is_none());
}

#[test]
fn timing_correction_saturates_clamps_and_consumes_phase_once() {
    assert_eq!(
        bounded_cycle_timing_correction_ns(1_000, i64::MAX, i64::MAX),
        100
    );
    assert_eq!(
        bounded_cycle_timing_correction_ns(1_000, i64::MIN, i64::MIN),
        -100
    );

    let mut config = ModbusInitialConfig {
        dt_ns: 1_000,
        period_delta_ns: 20,
        phase_delta_ns: 30,
        ..ModbusInitialConfig::default()
    };
    assert_eq!(config.take_timing_correction_ns(), 50);
    assert_eq!(config.period_delta_ns, 20);
    assert_eq!(config.phase_delta_ns, 0);
    assert_eq!(config.take_timing_correction_ns(), 20);
}

#[test]
fn packet_magics_are_direction_specific_and_validated() {
    let markers = [
        BINDING_INPUT_MAGIC,
        BINDING_OUTPUT_MAGIC,
        CONFIGURING_INPUT_MAGIC,
        CONFIGURING_OUTPUT_MAGIC,
        OPERATING_INPUT_MAGIC,
        OPERATING_SNAPSHOT_MAGIC,
    ];
    for (index, marker) in markers.iter().enumerate() {
        assert!(!markers[..index].contains(marker));
    }

    let mut binding_input = Rev7BindingInput::new(1_000);
    assert!(round_trip(binding_input).is_valid());
    binding_input.magic ^= 1;
    assert!(!round_trip(binding_input).is_valid());

    let mut binding_output = Rev7BindingOutput::new(PeripheralId {
        model_number: MODEL_NUMBER,
        serial_number: 3,
    });
    assert!(round_trip(binding_output).is_valid());
    binding_output.peripheral_id.model_number ^= 1;
    assert!(!round_trip(binding_output).is_valid());

    let mut configuring_input = Rev7ConfiguringInput::from_base(ConfiguringInput::default());
    configuring_input.dt_ns = DEIMOS_MIN_CYCLE_PERIOD_NS;
    assert!(round_trip(configuring_input).is_valid());
    configuring_input.dt_ns = DEIMOS_MIN_CYCLE_PERIOD_NS - 1;
    assert!(!round_trip(configuring_input).is_valid());
    configuring_input.dt_ns = DEIMOS_MAX_CYCLE_PERIOD_NS;
    assert!(round_trip(configuring_input).is_valid());
    configuring_input.dt_ns = DEIMOS_MAX_CYCLE_PERIOD_NS + 1;
    assert!(!round_trip(configuring_input).is_valid());
    configuring_input.dt_ns = DEIMOS_MAX_CYCLE_PERIOD_NS;
    configuring_input.magic ^= 1;
    assert!(!round_trip(configuring_input).is_valid());

    let mut configuring_output = Rev7ConfiguringOutput::new(AcknowledgeConfiguration::Ack, false);
    assert!(round_trip(configuring_output).is_valid());
    configuring_output.firmware_calibrated = 2;
    assert!(!round_trip(configuring_output).is_valid());

    let mut operating_input = OperatingRoundtripInput::default();
    assert!(round_trip(operating_input).is_valid());
    operating_input.outputs.pwm_duty_frac[0] = f32::NAN;
    assert!(!round_trip(operating_input).is_valid());

    let mut snapshot = OperatingSnapshot {
        sample_time_ns: 0x0012_3456_789a_bcde,
        ..OperatingSnapshot::default()
    };
    assert!(round_trip(snapshot).is_valid());
    assert_eq!(round_trip(snapshot).sample_time_ns, snapshot.sample_time_ns);
    assert_eq!(OperatingSnapshot::BYTE_LEN, 149);
    snapshot.magic ^= 1;
    assert!(!round_trip(snapshot).is_valid());
}

#[test]
fn calibration_binary_round_trips_without_protocol_magic() {
    let mut calibration = Rev7Calibration::default();
    calibration.firmware_calibrated = 1;
    calibration.voltage_cals[4] = LinearCalibration {
        slope: 1.25,
        offset: -0.125,
    };

    let decoded = round_trip(calibration);
    assert!(decoded.is_valid());
    assert!(decoded.is_calibrated());
    assert_eq!(decoded.voltage_cals[4].slope, 1.25);
    assert_eq!(decoded.voltage_cals[4].offset, -0.125);
    assert_eq!(Rev7Calibration::BYTE_LEN, 1 + ADC_CHANNEL_COUNT * 8);
}

#[test]
fn operating_output_settings_round_trip_as_one_preserved_value() {
    let settings = OperatingOutputSettings {
        pwm_duty_frac: [0.1, 0.2, 0.3, 0.4],
        pwm_freq_hz: [1_000, 2_000, 3_000, 4_000],
        dac_v: [0.5, 2.0],
        gpio: 0b1010,
    };
    assert!(settings.is_valid());

    let retained_for_reentry = settings;
    assert_eq!(retained_for_reentry, settings);

    let initial_config = ModbusInitialConfig {
        period_delta_ns: 1_000,
        phase_delta_ns: -250,
        outputs: retained_for_reentry,
        ..ModbusInitialConfig::default()
    };
    assert_eq!(initial_config.dt_ns, MODBUS_DEFAULT_DT_NS);
    assert_eq!(
        initial_config.loss_of_contact_limit,
        MODBUS_DEFAULT_LOSS_OF_CONTACT_LIMIT
    );
    let reentry_config = initial_config.reenter_at_period(200_000);
    assert_eq!(reentry_config.dt_ns, 200_000);
    assert_eq!(
        reentry_config.loss_of_contact_limit,
        initial_config.loss_of_contact_limit
    );
    assert_eq!(
        reentry_config.period_delta_ns,
        initial_config.period_delta_ns
    );
    assert_eq!(reentry_config.phase_delta_ns, initial_config.phase_delta_ns);
    assert_eq!(reentry_config.outputs, settings);

    let packet = OperatingRoundtripInput {
        outputs: reentry_config.outputs,
        ..OperatingRoundtripInput::default()
    };
    let mut encoded = [0_u8; 69];
    packet.write_bytes(&mut encoded);
    assert_eq!(&encoded[28..32], &0.1_f32.to_le_bytes());
    assert_eq!(&encoded[44..48], &1_000_u32.to_le_bytes());
    assert_eq!(&encoded[60..64], &0.5_f32.to_le_bytes());
    assert_eq!(encoded[68], 0b1010);

    let decoded = round_trip(packet);
    assert_eq!(decoded.outputs, settings);
    assert_eq!(OperatingOutputSettings::BYTE_LEN, 41);
    assert_eq!(OperatingRoundtripInput::BYTE_LEN, 69);
}

#[test]
fn engineering_conversion_preserves_channel_order_and_calibration_placement() {
    use crate::calcs::{ktype_voltage_v_f32, pt100_resistance_ohm_f32};

    let cold_junction_k = 300.0_f32;
    let hot_junction_k = 500.0_f32;
    let mut samples = [0.0_f32; ADC_CHANNEL_COUNT];
    samples[0] = 0.3;
    samples[1] = 1.5;
    samples[2] =
        pt100_resistance_ohm_f32(cold_junction_k) * RTD_REFERENCE_CURRENT_A * RTD_FRONTEND_GAIN;
    samples[3] = 0.75;
    samples[7] = 100.0 * RTD_REFERENCE_CURRENT_A * RTD_FRONTEND_GAIN;
    samples[10] = TC_FRONTEND_OFFSET_V
        + TC_FRONTEND_GAIN
            * (ktype_voltage_v_f32(hot_junction_k) - ktype_voltage_v_f32(cold_junction_k));
    samples[12] = 1.25;
    samples[13] = 2.0;
    samples[14] = 2.0;
    samples[15] = 1.5;
    samples[16] = TC_FRONTEND_OFFSET_V + TC_FRONTEND_GAIN * 0.01;
    samples[17] = TC_FRONTEND_OFFSET_V - TC_FRONTEND_GAIN * 0.005;

    let mut calibration = Rev7Calibration::default();
    calibration.voltage_cals[3] = LinearCalibration {
        slope: 2.0,
        offset: 0.15,
    };
    calibration.voltage_cals[7] = LinearCalibration {
        slope: 2.0,
        offset: 0.001,
    };
    calibration.voltage_cals[14] = LinearCalibration {
        slope: 2.0,
        offset: 1.0,
    };
    calibration.voltage_cals[16] = LinearCalibration {
        slope: 2.0,
        offset: 0.001,
    };

    let calculated_board_k = board_temperature_k_f32(&samples, &calibration);
    assert!((calculated_board_k - cold_junction_k).abs() <= 0.01);

    let mut snapshot = OperatingSnapshot::default();
    populate_analog_snapshot_f32(&mut snapshot, &samples, &calibration, cold_junction_k);
    assert!((snapshot.module_bus_current_a - 1.0).abs() < 1.0e-6);
    assert!((snapshot.module_bus_voltage_v - 21.5).abs() < 1.0e-6);
    assert_eq!(snapshot.board_temperature_k, cold_junction_k);
    assert!((snapshot.current_4_20_a[0] - 0.022).abs() < 1.0e-7);
    assert!((snapshot.rtd_resistance_ohm[0] - 204.0).abs() < 1.0e-4);
    assert!((snapshot.thermocouple_temperature_k[0] - hot_junction_k).abs() < 0.02);
    assert_eq!(snapshot.voltage_v[0], 1.25);
    assert_eq!(snapshot.voltage_v[1], 2.0);
    assert_eq!(snapshot.voltage_v[2], 25.0);
    assert_eq!(snapshot.voltage_v[3], 9.0);
    assert!((snapshot.voltage_v[4] - 0.021).abs() < 1.0e-6);
    assert!((snapshot.voltage_v[5] + 0.005).abs() < 1.0e-6);
}
