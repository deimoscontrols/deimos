//! Runtime IIR and fractional-delay filter construction.

use super::*;
use deimos_numerics::{
    control::lti::{butter, design_digital_filter_tf, FilterDesignError, Fir as DynamicFir},
    embedded::{
        error::EmbeddedError,
        fixed::lti::{
            lagrange_fractional_delay, lagrange_fractional_delay_taps, DeltaSos as FixedDeltaSos,
        },
    },
};

/// Builds the fixed-size delta-SOS ADC filter bank used by firmware.
pub fn adc_filter_bank(cutoff_ratio: f64) -> Result<AdcFilterBank, AdcFilterBuildError> {
    adc_filter_for_lanes::<ADC_FILTER_COUNT>(cutoff_ratio)
}

/// Builds a single-lane fixed-size delta-SOS ADC filter.
///
/// This is useful for values that are sampled independently from the ADC
/// acquisition group but use the same low-pass design.
pub fn adc_filter(cutoff_ratio: f64) -> Result<AdcFilter, AdcFilterBuildError> {
    adc_filter_for_lanes::<1>(cutoff_ratio)
}

/// Builds transfer functions corresponding to the ADC filter bank.
///
/// The returned transfer functions use a normalized sample interval of one
/// sample, matching the normalized cutoff-ratio basis used by the firmware
/// filter construction.
pub fn adc_filter_transfer_functions(
    cutoff_ratio: f64,
) -> Result<AdcFilterTransferFunctionBank, AdcFilterBuildError> {
    let cutoff_ratio = clamp_adc_filter_cutoff_ratio(cutoff_ratio);
    // The design API uses angular frequency with a one-second normalized
    // sample interval, so cycles/sample becomes radians/sample here.
    let transfer_function =
        design_digital_filter_tf(&deimos_numerics::control::lti::DigitalFilterSpec::new(
            ADC_FILTER_ORDER,
            deimos_numerics::control::lti::DigitalFilterFamily::Butterworth,
            deimos_numerics::control::lti::FilterShape::Lowpass {
                cutoff: cutoff_ratio * core::f64::consts::TAU,
            },
            1.0,
        )?)?;

    Ok(core::array::from_fn(|_| transfer_function.clone()))
}

/// Builds the fractional-delay FIR filter bank used to align ADC channels.
pub fn adc_fractional_delay_filter_bank(
    sample_rate_hz: f64,
) -> Result<AdcFractionalDelayFilterBank, AdcFilterBuildError> {
    let delay_samples = adc_fractional_delay_samples(sample_rate_hz)?;
    let sample_time = (1.0 / sample_rate_hz) as f32;
    // Seed the fixed-size array with a valid zero-delay filter, then replace
    // every element with its channel-specific delay without allocation.
    let mut filters =
        [lagrange_fractional_delay::<ADC_FRACTIONAL_DELAY_FILTER_TAPS, 1, f32>(0.0, sample_time)?;
            ADC_FILTER_COUNT];

    for (filter, &delay) in filters.iter_mut().zip(delay_samples.iter()) {
        *filter = lagrange_fractional_delay::<ADC_FRACTIONAL_DELAY_FILTER_TAPS, 1, f32>(
            delay as f32,
            sample_time,
        )?;
    }

    Ok(filters)
}

/// Builds transfer functions corresponding to the ADC fractional-delay filter bank.
pub fn adc_fractional_delay_transfer_functions(
    sample_rate_hz: f64,
) -> Result<AdcFractionalDelayTransferFunctionBank, AdcFilterBuildError> {
    let delay_samples = adc_fractional_delay_samples(sample_rate_hz)?;
    let sample_time = 1.0 / sample_rate_hz;
    // Transfer functions are not Copy, so Options provide safe fixed-size
    // construction while the bounded loop fills every element.
    let mut output: [Option<AdcFractionalDelayTransferFunction>; ADC_FILTER_COUNT] =
        core::array::from_fn(|_| None);
    for (idx, delay) in delay_samples.into_iter().enumerate() {
        let taps = lagrange_fractional_delay_taps::<ADC_FRACTIONAL_DELAY_FILTER_TAPS, f64>(delay)?;
        output[idx] = Some(DynamicFir::new(taps, sample_time)?.to_transfer_function()?);
    }

    Ok(output.map(|transfer_function| transfer_function.unwrap()))
}

/// Build the digital ADC paths implied by a reporting cycle rate.
///
/// Oversampled paths contain the fractional-delay FIR followed by the ADC
/// IIR. Direct paths contain only the fractional-delay FIR, matching the
/// firmware hot path selected by [`super::adc_sampling_policy`].
///
/// Args:
///   cycle_rate_hz: Reporting cycle rate scalar in `cycle/s`.
///
/// Returns:
///   Digital transfer-function bank with shape `(ADC_FILTER_COUNT,)`.
pub fn adc_digital_transfer_functions_for_cycle_rate(
    cycle_rate_hz: f64,
) -> Result<AdcDigitalTransferFunctionBank, AdcFilterBuildError> {
    let policy = validated_sampling_policy(cycle_rate_hz)?;
    adc_digital_transfer_functions(policy.iir_cutoff_ratio, policy.sample_rate_hz)
}

pub(super) fn adc_digital_transfer_functions(
    iir_cutoff_ratio: Option<f64>,
    sample_rate_hz: f64,
) -> Result<AdcDigitalTransferFunctionBank, AdcFilterBuildError> {
    let fractional_delay_transfer_functions =
        adc_fractional_delay_transfer_functions(sample_rate_hz)?;
    let adc_filter_transfer_function = iir_cutoff_ratio
        .map(|cutoff_ratio| {
            adc_filter_transfer_function_at_sample_rate(cutoff_ratio, sample_rate_hz)
        })
        .transpose()?;

    // Cascade the common IIR after each channel-specific alignment FIR. In
    // direct mode the absent IIR leaves the alignment transfer unchanged.
    let mut output: [Option<AdcDigitalTransferFunction>; ADC_CHANNEL_COUNT] =
        core::array::from_fn(|_| None);
    for (idx, fractional_delay) in fractional_delay_transfer_functions.into_iter().enumerate() {
        output[idx] = Some(match &adc_filter_transfer_function {
            Some(adc_filter) => fractional_delay.mul(adc_filter)?,
            None => fractional_delay,
        });
    }
    Ok(output.map(|transfer_function| transfer_function.unwrap()))
}

pub(super) fn validated_sampling_policy(
    cycle_rate_hz: f64,
) -> Result<AdcSamplingPolicy, AdcFilterBuildError> {
    adc_sampling_policy(cycle_rate_hz).ok_or_else(|| {
        AdcFilterBuildError::from(EmbeddedError::InvalidParameter {
            which: "adc.cycle_rate_hz",
        })
    })
}

fn adc_filter_for_lanes<const LANES: usize>(
    cutoff_ratio: f64,
) -> Result<FixedDeltaSos<f32, ADC_FILTER_SECTIONS, LANES>, AdcFilterBuildError> {
    let cutoff_ratio = clamp_adc_filter_cutoff_ratio(cutoff_ratio);
    let dynamic_delta = butter::<ADC_FILTER_ORDER>(cutoff_ratio)
        .and_then(|filter| filter.try_cast::<f32>().map_err(FilterDesignError::from))?;
    Ok(FixedDeltaSos::try_from(&dynamic_delta)?)
}

fn clamp_adc_filter_cutoff_ratio(cutoff_ratio: f64) -> f64 {
    // Keep analytic and embedded construction inside the cutoff range covered
    // by the fixed acquisition policy and coefficient representation.
    cutoff_ratio.min(ADC_FILTER_MAX_CUTOFF_RATIO)
}

fn adc_filter_transfer_function_at_sample_rate(
    cutoff_ratio: f64,
    sample_rate_hz: f64,
) -> Result<AdcFilterTransferFunction, AdcFilterBuildError> {
    validate_sample_rate_hz(sample_rate_hz)?;
    let cutoff_ratio = clamp_adc_filter_cutoff_ratio(cutoff_ratio);
    Ok(design_digital_filter_tf(
        &deimos_numerics::control::lti::DigitalFilterSpec::new(
            ADC_FILTER_ORDER,
            deimos_numerics::control::lti::DigitalFilterFamily::Butterworth,
            deimos_numerics::control::lti::FilterShape::Lowpass {
                cutoff: cutoff_ratio * sample_rate_hz * core::f64::consts::TAU,
            },
            sample_rate_hz,
        )?,
    )?)
}

fn adc_fractional_delay_samples(
    sample_rate_hz: f64,
) -> Result<[f64; ADC_FILTER_COUNT], AdcFilterBuildError> {
    if !sample_rate_hz.is_finite() || sample_rate_hz <= 0.0 {
        return Err(EmbeddedError::InvalidParameter {
            which: "adc.sample_rate_hz",
        }
        .into());
    }

    // ADC1, ADC2, and ADC3 start each tuple concurrently. Tuples execute in
    // order, separated by one sample-and-hold plus conversion duration.
    let delay_per_group = (ADC_SAMPLE_HOLD_CYCLES + ADC_CONVERSION_CYCLES) / ADC_CLOCK_HZ;
    let sample_time = 1.0 / sample_rate_hz;
    let mut delays = [0.0_f64; ADC_CHANNEL_COUNT + super::DAC_CHANNEL_COUNT];

    // Indices follow the compact reported order ain0..ain12, ain15..ain19.
    // Physical channels 15..19 are shifted down by two because 13 and 14 are
    // not included in the reported array.
    let groups = (
        [8, 9, 0],
        [10, 12, 1],
        [11, 2],
        [15 - 2, 3],
        [16 - 2, 17 - 2, 4],
        [18 - 2, 5],
        [19 - 2, 6],
        [7],
    );

    let mut apply_delay = |group: &[usize], group_idx: usize| {
        let delay = group_idx as f64 * delay_per_group;
        for &channel in group {
            delays[channel] = delay;
        }
    };

    apply_delay(&groups.0, 0);
    apply_delay(&groups.1, 1);
    apply_delay(&groups.2, 2);
    apply_delay(&groups.3, 3);
    apply_delay(&groups.4, 4);
    apply_delay(&groups.5, 5);
    apply_delay(&groups.6, 6);
    apply_delay(&groups.7, 7);

    // The Lagrange constructor expresses delay in samples rather than seconds.
    Ok(core::array::from_fn(|idx| delays[idx] / sample_time))
}

pub(super) fn validate_sample_rate_hz(sample_rate_hz: f64) -> Result<f64, AdcFilterBuildError> {
    if !sample_rate_hz.is_finite() || sample_rate_hz <= 0.0 {
        return Err(EmbeddedError::InvalidParameter {
            which: "adc.sample_rate_hz",
        }
        .into());
    }
    Ok(1.0 / sample_rate_hz)
}
