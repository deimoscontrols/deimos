//! Analog, digital, and combined transfer-function and Bode analysis.

use super::{
    design::{adc_digital_transfer_functions, validate_sample_rate_hz, validated_sampling_policy},
    *,
};
use deimos_numerics::control::{
    lti::{sallen_key_lowpass_transfer_function, BodeData, ContinuousTransferFunction, LtiError},
    DiscretizationMethod,
};

/// Builds continuous-time transfer functions for the analog voltage front ends.
///
/// The modeled filtered channels are a unity-gain active Sallen-Key low-pass
/// followed by the ADC input RC filter. Board current and board voltage are
/// modeled as unity transfer functions.
pub fn adc_analog_frontend_transfer_functions(
) -> Result<AdcAnalogFrontendTransferFunctionBank, AdcFilterBuildError> {
    let unfiltered = unfiltered_transfer_function()?;
    let sallen_key_100hz =
        sallen_key_with_adc_rc_transfer_function(SALLEN_KEY_100HZ_RESISTANCE_OHMS)?;
    let sallen_key_1khz =
        sallen_key_with_adc_rc_transfer_function(SALLEN_KEY_1KHZ_RESISTANCE_OHMS)?;
    let sallen_key_3khz =
        sallen_key_with_adc_rc_transfer_function(SALLEN_KEY_3KHZ_RESISTANCE_OHMS)?;

    Ok(core::array::from_fn(|idx| {
        match ADC_ANALOG_FRONTEND_FILTER_KINDS[idx] {
            super::AdcAnalogFrontendFilterKind::Unfiltered => &unfiltered,
            super::AdcAnalogFrontendFilterKind::SallenKey100Hz => &sallen_key_100hz,
            super::AdcAnalogFrontendFilterKind::SallenKey1kHz => &sallen_key_1khz,
            super::AdcAnalogFrontendFilterKind::SallenKey3kHz => &sallen_key_3khz,
        }
        .clone()
    }))
}

/// Builds sampled-sequence transfer functions for the ADC measurement filter chain.
///
/// Each returned transfer function models the channel's analog front end
/// sampled with a bilinear transform at `sample_rate_hz`, followed by the
/// channel's fractional-delay FIR and the firmware ADC Butterworth
/// low-pass filter. The returned bank is ordered like reported ADC
/// voltages: `ain0..ain12` followed by `ain15..ain19`.
///
/// These transfer functions are useful for baseband sampled-sequence
/// analysis. For physical input-frequency Bode data, use
/// [`adc_sampled_bode_data`], which keeps the analog frontend response in
/// continuous time so high-frequency analog attenuation is not aliased.
pub fn adc_sampled_transfer_functions(
    cutoff_ratio: f64,
    sample_rate_hz: f64,
) -> Result<AdcSampledTransferFunctionBank, AdcFilterBuildError> {
    adc_sampled_transfer_functions_with_iir(Some(cutoff_ratio), sample_rate_hz)
}

/// Build sampled-sequence transfer functions implied by a reporting rate.
///
/// Args:
///   cycle_rate_hz: Reporting cycle rate scalar in `cycle/s`.
///
/// Returns:
///   Full sampled transfer-function bank with shape `(ADC_CHANNEL_COUNT,)`.
pub fn adc_sampled_transfer_functions_for_cycle_rate(
    cycle_rate_hz: f64,
) -> Result<AdcSampledTransferFunctionBank, AdcFilterBuildError> {
    let policy = validated_sampling_policy(cycle_rate_hz)?;
    adc_sampled_transfer_functions_with_iir(policy.iir_cutoff_ratio, policy.sample_rate_hz)
}

fn adc_sampled_transfer_functions_with_iir(
    iir_cutoff_ratio: Option<f64>,
    sample_rate_hz: f64,
) -> Result<AdcSampledTransferFunctionBank, AdcFilterBuildError> {
    let sample_time = validate_sample_rate_hz(sample_rate_hz)?;
    let analog_transfer_functions = adc_analog_frontend_transfer_functions()?;
    let digital_transfer_functions =
        adc_digital_transfer_functions(iir_cutoff_ratio, sample_rate_hz)?;

    let mut output: [Option<AdcSampledTransferFunction>; ADC_CHANNEL_COUNT] =
        core::array::from_fn(|_| None);
    for idx in 0..ADC_CHANNEL_COUNT {
        let sampled_analog = analog_transfer_functions[idx]
            .to_state_space()?
            .discretize(
                sample_time,
                DiscretizationMethod::Bilinear {
                    prewarp_frequency: None,
                },
            )
            .map_err(LtiError::from)?
            .to_transfer_function()?;

        output[idx] = Some(sampled_analog.mul(&digital_transfer_functions[idx])?);
    }

    Ok(output.map(|transfer_function| transfer_function.unwrap()))
}

/// Builds physical-input-frequency Bode data for the full ADC measurement filter chain.
///
/// For each requested physical input frequency, this evaluates the analog
/// frontend as a continuous-time response and the fractional-delay plus
/// digital ADC filter as a discrete-time response. The returned magnitude
/// is therefore `|H_analog(jw)| * |H_digital(exp(jwT))|`, so analog
/// attenuation at high input frequencies is preserved even when the sampled
/// digital response aliases.
pub fn adc_sampled_bode_data(
    cutoff_ratio: f64,
    sample_rate_hz: f64,
    frequencies_hz: &[f64],
) -> Result<AdcSampledBodeDataBank, AdcFilterBuildError> {
    adc_sampled_bode_data_with_iir(Some(cutoff_ratio), sample_rate_hz, frequencies_hz)
}

/// Build physical-input-frequency Bode data implied by a reporting rate.
///
/// Args:
///   cycle_rate_hz: Reporting cycle rate scalar in `cycle/s`.
///   frequencies_hz: Physical input-frequency grid in `Hz` with shape `(n,)`.
///
/// Returns:
///   Full measurement-path Bode bank with shape `(ADC_CHANNEL_COUNT,)`.
pub fn adc_sampled_bode_data_for_cycle_rate(
    cycle_rate_hz: f64,
    frequencies_hz: &[f64],
) -> Result<AdcSampledBodeDataBank, AdcFilterBuildError> {
    let policy = validated_sampling_policy(cycle_rate_hz)?;
    adc_sampled_bode_data_with_iir(
        policy.iir_cutoff_ratio,
        policy.sample_rate_hz,
        frequencies_hz,
    )
}

fn adc_sampled_bode_data_with_iir(
    iir_cutoff_ratio: Option<f64>,
    sample_rate_hz: f64,
    frequencies_hz: &[f64],
) -> Result<AdcSampledBodeDataBank, AdcFilterBuildError> {
    validate_sample_rate_hz(sample_rate_hz)?;
    let analog_transfer_functions = adc_analog_frontend_transfer_functions()?;
    let digital_transfer_functions =
        adc_digital_transfer_functions(iir_cutoff_ratio, sample_rate_hz)?;
    let angular_frequencies: alloc::vec::Vec<f64> = frequencies_hz
        .iter()
        .map(|frequency_hz| frequency_hz * core::f64::consts::TAU)
        .collect();
    let mut output: [Option<AdcSampledBodeData>; ADC_CHANNEL_COUNT] =
        core::array::from_fn(|_| None);
    for idx in 0..ADC_CHANNEL_COUNT {
        let analog_bode = analog_transfer_functions[idx].bode_data(&angular_frequencies)?;
        let digital_bode = digital_transfer_functions[idx].bode_data(&angular_frequencies)?;
        output[idx] = Some(combine_bode_data(&analog_bode, &digital_bode)?);
    }

    Ok(output.map(|bode_data| bode_data.unwrap()))
}

fn unfiltered_transfer_function() -> Result<AdcAnalogFrontendTransferFunction, LtiError> {
    ContinuousTransferFunction::continuous([1.0], [1.0])
}

fn sallen_key_with_adc_rc_transfer_function(
    resistance_ohms: f64,
) -> Result<AdcAnalogFrontendTransferFunction, LtiError> {
    sallen_key_lowpass_transfer_function(
        resistance_ohms,
        resistance_ohms,
        SALLEN_KEY_CAPACITANCE_F,
        SALLEN_KEY_CAPACITANCE_F,
    )?
    .mul(&rc_lowpass_transfer_function(
        ADC_INPUT_RC_RESISTANCE_OHMS,
        ADC_INPUT_RC_CAPACITANCE_F,
    )?)
}

fn rc_lowpass_transfer_function(
    resistance_ohms: f64,
    capacitance_f: f64,
) -> Result<AdcAnalogFrontendTransferFunction, LtiError> {
    ContinuousTransferFunction::continuous([1.0], [resistance_ohms * capacitance_f, 1.0])
}

fn combine_bode_data(
    lhs: &BodeData<f64>,
    rhs: &BodeData<f64>,
) -> Result<BodeData<f64>, AdcFilterBuildError> {
    if lhs.angular_frequencies != rhs.angular_frequencies {
        return Err(LtiError::InvalidSampleGrid {
            which: "combine_bode_data",
        }
        .into());
    }
    Ok(BodeData {
        angular_frequencies: lhs.angular_frequencies.clone(),
        magnitude_db: lhs
            .magnitude_db
            .iter()
            .zip(rhs.magnitude_db.iter())
            .map(|(lhs, rhs)| lhs + rhs)
            .collect(),
        phase_deg: lhs
            .phase_deg
            .iter()
            .zip(rhs.phase_deg.iter())
            .map(|(lhs, rhs)| lhs + rhs)
            .collect(),
    })
}
