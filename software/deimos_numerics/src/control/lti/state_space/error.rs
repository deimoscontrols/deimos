use core::fmt;

/// Errors produced by state-space construction and conversion routines.
///
/// The state-space layer distinguishes between:
///
/// - shape errors in the `A/B/C/D` blocks
/// - invalid time-domain metadata such as a bad sample interval
/// - mathematically valid but unsupported conversions
/// - numerical failures inside dense conversion formulas
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum StateSpaceError {
    /// The state matrix `A` must be square.
    NonSquareA {
        /// Actual row count in `A`.
        nrows: usize,
        /// Actual column count in `A`.
        ncols: usize,
    },
    /// One of the `A/B/C/D` blocks had incompatible dimensions.
    DimensionMismatch {
        /// Identifies the block that failed validation.
        which: &'static str,
        /// Required row count.
        expected_nrows: usize,
        /// Required column count.
        expected_ncols: usize,
        /// Actual row count.
        actual_nrows: usize,
        /// Actual column count.
        actual_ncols: usize,
    },
    /// A discrete-time sample interval must be positive and finite.
    InvalidSampleTime,
    /// Two discrete-time systems cannot be composed because their sample
    /// intervals do not match closely enough.
    ///
    /// Composition helpers intentionally reject mixed sample times instead of
    /// silently treating nearly-related sampled systems as equivalent models.
    MismatchedSampleTime,
    /// A bilinear prewarp frequency must be finite and nonnegative.
    InvalidPrewarpFrequency,
    /// A conversion required solving against a singular or numerically
    /// unsuitable matrix.
    SingularConversion {
        /// Identifies the matrix solve that failed.
        which: &'static str,
    },
    /// A dense matrix function or solve produced non-finite output.
    NonFiniteResult {
        /// Identifies the conversion step that produced non-finite output.
        which: &'static str,
    },
    /// A matrix entry or sample interval could not be cast to the requested
    /// scalar dtype.
    ScalarConversionFailed {
        /// Identifies the field that could not be cast.
        which: &'static str,
    },
    /// The requested conversion is recognized but not implemented in this
    /// first state-space pass.
    UnsupportedConversion(&'static str),
}

impl fmt::Display for StateSpaceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

impl core::error::Error for StateSpaceError {}
