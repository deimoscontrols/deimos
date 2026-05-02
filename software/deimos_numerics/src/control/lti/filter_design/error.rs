use super::super::error::LtiError;
use core::fmt;

/// Errors produced by analog and digital IIR filter design.
#[derive(Debug)]
pub enum FilterDesignError {
    /// Filter order must be at least one.
    InvalidOrder,
    /// The supplied ripple parameter is invalid.
    InvalidRipple,
    /// The supplied stopband attenuation parameter is invalid.
    InvalidAttenuation,
    /// One cutoff frequency is invalid.
    InvalidCutoff {
        /// Identifies the cutoff or band edge that failed validation.
        which: &'static str,
    },
    /// Band edges are invalid.
    InvalidBandEdges,
    /// Passband and stopband shapes are not compatible for order selection.
    IncompatibleOrderSelectionShape,
    /// The supplied order-selection specification is not feasible.
    InfeasibleOrderSelectionSpec,
    /// Sample rate must be positive and finite.
    InvalidSampleRate,
    /// A lower-level LTI conversion or root-extraction helper failed.
    Lti(LtiError),
}

impl fmt::Display for FilterDesignError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

impl core::error::Error for FilterDesignError {}

impl From<LtiError> for FilterDesignError {
    fn from(value: LtiError) -> Self {
        Self::Lti(value)
    }
}
