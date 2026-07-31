//! Public filter representations and construction errors.

use core::fmt;

use super::{
    ADC_CHANNEL_COUNT, ADC_FILTER_COUNT, ADC_FILTER_SECTIONS, ADC_FRACTIONAL_DELAY_FILTER_TAPS,
};
use deimos_numerics::{
    control::lti::{
        BodeData, ContinuousTransferFunction, DiscreteTransferFunction, FilterDesignError, LtiError,
    },
    embedded::{
        error::EmbeddedError,
        fixed::lti::{
            DeltaSos as FixedDeltaSos, DeltaSosState as FixedDeltaSosState, Fir as FixedFir,
            FirState as FixedFirState,
        },
    },
};

/// Runtime ADC low-pass filter used by rev7 firmware.
pub type AdcFilter = FixedDeltaSos<f32, ADC_FILTER_SECTIONS, 1>;

/// Runtime state for one rev7 ADC low-pass filter.
pub type AdcFilterState = FixedDeltaSosState<f32, ADC_FILTER_SECTIONS, 1>;

/// Full rev7 ADC low-pass filter bank.
pub type AdcFilterBank = [AdcFilter; ADC_FILTER_COUNT];

/// Transfer function corresponding to one rev7 ADC low-pass filter.
pub type AdcFilterTransferFunction = DiscreteTransferFunction<f64>;

/// Transfer functions corresponding to the full rev7 ADC low-pass filter bank.
pub type AdcFilterTransferFunctionBank = [AdcFilterTransferFunction; ADC_FILTER_COUNT];

/// Runtime fractional-delay filter used to align rev7 ADC channel samples.
pub type AdcFractionalDelayFilter = FixedFir<f32, ADC_FRACTIONAL_DELAY_FILTER_TAPS, 1>;

/// Runtime state for one rev7 ADC fractional-delay filter.
pub type AdcFractionalDelayFilterState = FixedFirState<f32, ADC_FRACTIONAL_DELAY_FILTER_TAPS, 1>;

/// Full rev7 ADC fractional-delay filter bank.
pub type AdcFractionalDelayFilterBank = [AdcFractionalDelayFilter; ADC_FILTER_COUNT];

/// Transfer function corresponding to one rev7 ADC fractional-delay filter.
pub type AdcFractionalDelayTransferFunction = DiscreteTransferFunction<f64>;

/// Transfer functions corresponding to the full rev7 ADC fractional-delay filter bank.
pub type AdcFractionalDelayTransferFunctionBank =
    [AdcFractionalDelayTransferFunction; ADC_FILTER_COUNT];

/// Transfer function for one complete rev7 digital ADC filter path.
pub type AdcDigitalTransferFunction = DiscreteTransferFunction<f64>;

/// Transfer functions for all complete rev7 digital ADC filter paths.
pub type AdcDigitalTransferFunctionBank = [AdcDigitalTransferFunction; ADC_FILTER_COUNT];

/// Continuous-time transfer function for one rev7 ADC analog front end.
pub type AdcAnalogFrontendTransferFunction = ContinuousTransferFunction<f64>;

/// Continuous-time transfer functions for all rev7 ADC analog front ends.
pub type AdcAnalogFrontendTransferFunctionBank =
    [AdcAnalogFrontendTransferFunction; ADC_CHANNEL_COUNT];

/// Sampled transfer function for one full rev7 ADC measurement filter chain.
pub type AdcSampledTransferFunction = DiscreteTransferFunction<f64>;

/// Sampled transfer functions for all rev7 ADC measurement filter chains.
pub type AdcSampledTransferFunctionBank = [AdcSampledTransferFunction; ADC_CHANNEL_COUNT];

/// Bode data for one full rev7 ADC measurement filter chain.
pub type AdcSampledBodeData = BodeData<f64>;

/// Bode data for all rev7 ADC measurement filter chains.
pub type AdcSampledBodeDataBank = [AdcSampledBodeData; ADC_CHANNEL_COUNT];

/// Error returned while constructing rev7 ADC filters.
#[derive(Debug)]
pub enum AdcFilterBuildError {
    /// Filter design failed.
    FilterDesign(FilterDesignError),
    /// LTI representation conversion failed.
    Lti(LtiError),
    /// Fixed-size embedded representation conversion failed.
    Embedded(EmbeddedError),
}

impl fmt::Display for AdcFilterBuildError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

impl core::error::Error for AdcFilterBuildError {}

impl From<FilterDesignError> for AdcFilterBuildError {
    fn from(value: FilterDesignError) -> Self {
        Self::FilterDesign(value)
    }
}

impl From<LtiError> for AdcFilterBuildError {
    fn from(value: LtiError) -> Self {
        Self::Lti(value)
    }
}

impl From<EmbeddedError> for AdcFilterBuildError {
    fn from(value: EmbeddedError) -> Self {
        Self::Embedded(value)
    }
}
