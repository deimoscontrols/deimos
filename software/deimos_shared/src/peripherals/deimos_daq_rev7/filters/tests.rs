use super::*;

#[test]
fn low_rate_adc_filter_holds_a_primed_steady_state() {
    let filter = adc_filter_bank(1.0 / 2_250.0).unwrap()[0];
    let mut state = filter.reset_state();
    filter.set_steady_state(&mut state, [1.25]);

    for _ in 0..16 {
        let output = filter.step(&mut state, [1.25])[0];
        assert!(output.is_finite());
        assert!((output - 1.25).abs() < 1.0e-5);
    }
}

#[test]
fn adc_analog_frontends_preserve_unity_dc_gain() {
    let transfer_functions = adc_analog_frontend_transfer_functions().unwrap();

    for transfer_function in transfer_functions {
        let dc_gain = transfer_function.dc_gain().unwrap();
        assert!((dc_gain.re - 1.0).abs() < 1.0e-9);
        assert!(dc_gain.im.abs() < 1.0e-12);
    }
}

#[test]
fn adc_sampled_filter_chains_preserve_dc_and_attenuate_above_cutoff() {
    let transfer_functions =
        adc_sampled_transfer_functions(0.1, super::super::ADC_OVERSAMPLE_TARGET_RATE_HZ).unwrap();
    let stopband_frequency_rad_s = 3_000.0 * core::f64::consts::TAU;

    for transfer_function in transfer_functions {
        let dc_gain = transfer_function.dc_gain().unwrap();
        assert!((dc_gain.re - 1.0).abs() < 1.0e-4);
        assert!(dc_gain.im.abs() < 1.0e-10);
        let stopband = transfer_function
            .bode_data(&[stopband_frequency_rad_s])
            .unwrap();
        assert!(stopband.magnitude_db[0] < -10.0);
    }
}

#[test]
fn cycle_rate_filter_helpers_follow_shared_sampling_policy() {
    let oversampled = adc_sampled_transfer_functions_for_cycle_rate(1_000.0).unwrap();
    for transfer_function in oversampled {
        assert_eq!(transfer_function.sample_time(), 1.0 / 9_000.0);
    }

    let max_rate_hz = f64::from(super::super::DEIMOS_MAX_CYCLE_RATE_HZ);
    let direct = adc_digital_transfer_functions_for_cycle_rate(max_rate_hz).unwrap();
    let fractional = adc_fractional_delay_transfer_functions(max_rate_hz).unwrap();
    for (direct, fractional) in direct.iter().zip(fractional.iter()) {
        assert_eq!(direct.sample_time(), 1.0 / max_rate_hz);
        assert_eq!(direct.numerator(), fractional.numerator());
        assert_eq!(direct.denominator(), fractional.denominator());
    }

    assert!(adc_sampled_transfer_functions_for_cycle_rate(0.0).is_err());
}

#[test]
fn adc_combined_bode_preserves_high_frequency_analog_attenuation() {
    let sample_rate_hz = super::super::ADC_OVERSAMPLE_TARGET_RATE_HZ;
    let frequencies_hz = [1_000.0, 10_000.0, 16_000.0, 30_000.0, 100_000.0];
    let angular_frequencies: alloc::vec::Vec<f64> = frequencies_hz
        .iter()
        .map(|frequency_hz| frequency_hz * core::f64::consts::TAU)
        .collect();

    let analog_transfer_functions = adc_analog_frontend_transfer_functions().unwrap();
    let analog_bode = analog_transfer_functions[10]
        .bode_data(&angular_frequencies)
        .unwrap();
    let combined_bode =
        adc_sampled_bode_data(0.1, sample_rate_hz, &frequencies_hz).unwrap()[10].clone();

    for ((&frequency_hz, &combined_magnitude_db), &analog_magnitude_db) in frequencies_hz
        .iter()
        .zip(combined_bode.magnitude_db.iter())
        .zip(analog_bode.magnitude_db.iter())
    {
        assert!(
            combined_magnitude_db <= analog_magnitude_db + 1.0e-9,
            "combined magnitude at {frequency_hz} Hz is {combined_magnitude_db} dB, analog magnitude is {analog_magnitude_db} dB",
        );
    }
}
