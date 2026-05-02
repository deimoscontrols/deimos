//! Errors shared by the embedded runtime modules.
//!
use core::fmt;

/// Errors produced by the embedded runtime lane.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EmbeddedError {
    /// A sample interval was not positive and finite.
    InvalidSampleTime,
    /// A scalar configuration parameter was not valid.
    InvalidParameter {
        /// Identifies the offending parameter.
        which: &'static str,
    },
    /// A model or runtime state had incompatible dimensions.
    DimensionMismatch {
        /// Identifies the object that failed validation.
        which: &'static str,
        /// Expected row count.
        expected_rows: usize,
        /// Expected column count.
        expected_cols: usize,
        /// Actual row count.
        actual_rows: usize,
        /// Actual column count.
        actual_cols: usize,
    },
    /// Two runtime slices did not have the same length.
    LengthMismatch {
        /// Identifies the slice pair.
        which: &'static str,
        /// Expected length.
        expected: usize,
        /// Actual length.
        actual: usize,
    },
    /// A fixed-size conversion saw the wrong number of sections.
    SectionCountMismatch {
        /// Compile-time section count required by the destination type.
        expected: usize,
        /// Actual number of sections in the source object.
        actual: usize,
    },
    /// A linear solve or inversion encountered a singular matrix.
    SingularMatrix {
        /// Identifies the matrix that could not be solved.
        which: &'static str,
    },
    /// A covariance or factorization input was not positive definite.
    NonPositiveDefinite {
        /// Identifies the matrix that failed the check.
        which: &'static str,
    },
    /// A computed result was not finite.
    NonFiniteValue {
        /// Identifies the computation that failed.
        which: &'static str,
    },
    /// Back-calculation PID mode requires an applied command input.
    TrackingRequired,
}

impl fmt::Display for EmbeddedError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

impl core::error::Error for EmbeddedError {}
