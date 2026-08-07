//! Public filter representations and construction errors.
//!
//! Runtime aliases use fixed-size `f32` filters and state suitable for firmware.
//! Analysis aliases use `f64` transfer functions and Bode data so design and
//! plotting do not reduce the precision of the modeled response.

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

/// Single-lane runtime ADC low-pass filter.
///
/// This form is used for independently sampled values such as board
/// temperature. ADC acquisition uses [`AdcFilterBank`] so all channels share
/// one coefficient set while retaining independent filter histories.
pub type AdcFilter = FixedDeltaSos<f32, ADC_FILTER_SECTIONS, 1>;

/// Runtime state for one ADC low-pass filter.
pub type AdcFilterState = FixedDeltaSosState<f32, ADC_FILTER_SECTIONS, 1>;

/// Multi-lane runtime ADC low-pass filter bank.
///
/// `DeltaSos` applies the same coefficients to every lane and stores separate
/// state for each ADC channel, matching the acquisition filter topology
/// without duplicating the coefficients.
pub type AdcFilterBank = FixedDeltaSos<f32, ADC_FILTER_SECTIONS, ADC_FILTER_COUNT>;

/// Runtime state for the multi-lane ADC low-pass filter bank.
pub type AdcFilterBankState = FixedDeltaSosState<f32, ADC_FILTER_SECTIONS, ADC_FILTER_COUNT>;

/// Transfer function corresponding to one ADC low-pass filter.
pub type AdcFilterTransferFunction = DiscreteTransferFunction<f64>;

/// Transfer functions corresponding to the full ADC low-pass filter bank.
pub type AdcFilterTransferFunctionBank = [AdcFilterTransferFunction; ADC_FILTER_COUNT];

/// Runtime fractional-delay filter used to align ADC channel samples.
pub type AdcFractionalDelayFilter = FixedFir<f32, ADC_FRACTIONAL_DELAY_FILTER_TAPS, 1>;

/// Runtime state for one ADC fractional-delay filter.
pub type AdcFractionalDelayFilterState = FixedFirState<f32, ADC_FRACTIONAL_DELAY_FILTER_TAPS, 1>;

/// Full ADC fractional-delay filter bank.
pub type AdcFractionalDelayFilterBank = [AdcFractionalDelayFilter; ADC_FILTER_COUNT];

/// Transfer function corresponding to one ADC fractional-delay filter.
pub type AdcFractionalDelayTransferFunction = DiscreteTransferFunction<f64>;

/// Transfer functions corresponding to the full ADC fractional-delay filter bank.
pub type AdcFractionalDelayTransferFunctionBank =
    [AdcFractionalDelayTransferFunction; ADC_FILTER_COUNT];

/// Transfer function for one complete digital ADC filter path.
pub type AdcDigitalTransferFunction = DiscreteTransferFunction<f64>;

/// Transfer functions for all complete digital ADC filter paths.
pub type AdcDigitalTransferFunctionBank = [AdcDigitalTransferFunction; ADC_FILTER_COUNT];

/// Continuous-time transfer function for one ADC analog front end.
pub type AdcAnalogFrontendTransferFunction = ContinuousTransferFunction<f64>;

/// Continuous-time transfer functions for all ADC analog front ends.
pub type AdcAnalogFrontendTransferFunctionBank =
    [AdcAnalogFrontendTransferFunction; ADC_CHANNEL_COUNT];

/// Sampled transfer function for one full ADC measurement filter chain.
pub type AdcSampledTransferFunction = DiscreteTransferFunction<f64>;

/// Sampled transfer functions for all ADC measurement filter chains.
pub type AdcSampledTransferFunctionBank = [AdcSampledTransferFunction; ADC_CHANNEL_COUNT];

/// Bode data for one full ADC measurement filter chain.
pub type AdcSampledBodeData = BodeData<f64>;

/// Bode data for all ADC measurement filter chains.
pub type AdcSampledBodeDataBank = [AdcSampledBodeData; ADC_CHANNEL_COUNT];

/// Error returned while constructing ADC filters.
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
