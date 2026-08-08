//! Sampling policy and measurement-filter construction.

use super::{ADC_IIR_CUTOFF_TO_REPORT_RATE, ADC_OVERSAMPLE_TARGET_RATE_HZ};

#[cfg(feature = "alloc")]
use super::{
    ADC_ANALOG_FRONTEND_FILTER_KINDS, ADC_CHANNEL_COUNT, ADC_CLOCK_HZ, ADC_CONVERSION_CYCLES,
    ADC_FILTER_COUNT, ADC_FILTER_MAX_CUTOFF_RATIO, ADC_FILTER_ORDER, ADC_FILTER_SECTIONS,
    ADC_FRACTIONAL_DELAY_FILTER_TAPS, ADC_INPUT_RC_CAPACITANCE_F, ADC_INPUT_RC_RESISTANCE_OHMS,
    ADC_SAMPLE_HOLD_CYCLES, DAC_CHANNEL_COUNT, SALLEN_KEY_100HZ_RESISTANCE_OHMS,
    SALLEN_KEY_1KHZ_RESISTANCE_OHMS, SALLEN_KEY_3KHZ_RESISTANCE_OHMS, SALLEN_KEY_CAPACITANCE_F,
};

/// Synchronous ADC topology selected for one reporting cycle rate.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AdcSamplingMode {
    /// Take an integer number of samples near the internal-rate target and run
    /// both the fractional-delay FIR and ADC IIR on every group.
    Oversampled,
    /// Take one sample per reporting cycle and run only the fractional-delay
    /// FIR.
    Direct,
}

/// Derived sampling and filtering parameters.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AdcSamplingPolicy {
    /// Selected synchronous acquisition topology.
    pub mode: AdcSamplingMode,
    /// ADC groups acquired in each reporting cycle, in `sample/cycle`.
    pub samples_per_cycle: u32,
    /// Actual ADC-group samplerate, in `sample/s`.
    pub sample_rate_hz: f64,
    /// ADC IIR cutoff, in `Hz`, or `None` when the direct path omits the IIR.
    pub iir_cutoff_hz: Option<f64>,
    /// ADC IIR cutoff divided by samplerate, or `None` in direct mode.
    pub iir_cutoff_ratio: Option<f64>,
}

/// Derive the ADC samplerate and filter cutoff from a reporting cycle rate.
///
/// The integer part of [`super::ADC_OVERSAMPLE_TARGET_HZ`] divided by the
/// reporting rate determines the sample count. Two or more samples use the ADC
/// IIR at the fixed reporting-rate fraction specified by
/// [`ADC_IIR_CUTOFF_TO_REPORT_RATE`]. When only one complete sample fits below
/// the target, the direct path acquires one sample and omits the IIR. This
/// naturally places the nominal topology transition at `4.5 kHz`.
///
/// Args:
///   cycle_rate_hz: Requested reporting cycle rate scalar in `cycle/s`.
///
/// Returns:
///   Derived synchronous sampling policy, or `None` for a non-finite or
///   non-positive cycle rate or an unrepresentable integer sample count.
pub fn adc_sampling_policy(cycle_rate_hz: f64) -> Option<AdcSamplingPolicy> {
    if !cycle_rate_hz.is_finite() || cycle_rate_hz <= 0.0 {
        return None;
    }

    let samples_per_cycle = ADC_OVERSAMPLE_TARGET_RATE_HZ / cycle_rate_hz;
    if samples_per_cycle > f64::from(u32::MAX) {
        return None;
    }
    // For oversampled operation, deliberately floor to the largest whole
    // number of groups that does not exceed the internal samplerate target.
    let samples_per_cycle = samples_per_cycle as u32;

    if samples_per_cycle < 2 {
        // A one-group cycle uses the direct firmware path. Channel alignment
        // remains active there, but there is no oversampled IIR stage.
        return Some(AdcSamplingPolicy {
            mode: AdcSamplingMode::Direct,
            samples_per_cycle: 1,
            sample_rate_hz: cycle_rate_hz,
            iir_cutoff_hz: None,
            iir_cutoff_ratio: None,
        });
    }

    let sample_rate_hz = cycle_rate_hz * f64::from(samples_per_cycle);
    let iir_cutoff_hz = cycle_rate_hz * ADC_IIR_CUTOFF_TO_REPORT_RATE;
    // Filter construction consumes cutoff/samplerate, while the acquisition
    // policy specifies cutoff/reporting-rate.
    Some(AdcSamplingPolicy {
        mode: AdcSamplingMode::Oversampled,
        samples_per_cycle,
        sample_rate_hz,
        iir_cutoff_hz: Some(iir_cutoff_hz),
        iir_cutoff_ratio: Some(iir_cutoff_hz / sample_rate_hz),
    })
}

/// Analog front-end filter variant for one reported ADC voltage channel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AdcAnalogFrontendFilterKind {
    /// No analog low-pass filter is modeled for this channel.
    Unfiltered,
    /// Sallen-Key 100 Hz target, R1 = R2 = 100 kohm and C1 = C2 = 10 nF.
    SallenKey100Hz,
    /// Sallen-Key 1 kHz target, R1 = R2 = 10 kohm and C1 = C2 = 10 nF.
    SallenKey1kHz,
    /// Sallen-Key 3 kHz target, R1 = R2 = 3.3 kohm and C1 = C2 = 10 nF.
    SallenKey3kHz,
}

#[cfg(feature = "alloc")]
mod analysis;
#[cfg(feature = "alloc")]
mod design;
#[cfg(feature = "alloc")]
mod types;

#[cfg(feature = "alloc")]
pub use analysis::*;
#[cfg(feature = "alloc")]
pub use design::*;
#[cfg(feature = "alloc")]
pub use types::*;

#[cfg(all(feature = "alloc", test))]
mod tests;
