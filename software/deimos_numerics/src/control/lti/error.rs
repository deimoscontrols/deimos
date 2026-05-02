use core::fmt;

use super::state_space::StateSpaceError;
use crate::decomp::DecompError;
use crate::sparse::lu::SparseLuError;
use faer::linalg::solvers::{EvdError, SvdError};
use faer::sparse::{CreationError, FaerError};

/// Errors produced by LTI analysis and representation-conversion routines.
#[derive(Debug)]
pub enum LtiError {
    /// Dense eigendecomposition failed while extracting poles or roots.
    Eigen(EvdError),
    /// Dense SVD failed while making a numerical rank decision.
    Svd(SvdError),
    /// A discrete-time representation was given a nonpositive or nonfinite
    /// sample interval.
    InvalidSampleTime,
    /// Two discrete-time objects cannot be composed because their sample
    /// intervals do not match closely enough.
    MismatchedSampleTime,
    /// A response grid contained an invalid point.
    InvalidSamplePoint {
        /// Identifies the grid entry or sample location that failed validation.
        which: &'static str,
    },
    /// A sampling grid had inconsistent structure, such as mismatched lengths
    /// or non-monotone time points.
    InvalidSampleGrid {
        /// Identifies the grid structure that failed validation.
        which: &'static str,
    },
    /// An analysis or simulation input had incompatible dimensions.
    DimensionMismatch {
        /// Identifies the matrix or vector that failed the shape check.
        which: &'static str,
        /// Required row count.
        expected_nrows: usize,
        /// Required column count.
        expected_ncols: usize,
        /// Actual row count supplied by the caller.
        actual_nrows: usize,
        /// Actual column count supplied by the caller.
        actual_ncols: usize,
    },
    /// A polynomial representation was missing required coefficients.
    EmptyPolynomial {
        /// Identifies the polynomial that was empty.
        which: &'static str,
    },
    /// An FIR representation must contain at least one tap.
    EmptyFir,
    /// The leading coefficient of a polynomial must be nonzero.
    ZeroLeadingCoefficient {
        /// Identifies the polynomial whose leading coefficient was zero.
        which: &'static str,
    },
    /// A conversion expected a single-input single-output state-space system.
    NonSisoStateSpace {
        /// Number of system inputs.
        ninputs: usize,
        /// Number of system outputs.
        noutputs: usize,
    },
    /// A state-space realization exists only for proper transfer functions.
    ///
    /// In this module, ordinary `A/B/C/D` state space can represent:
    ///
    /// - strictly proper systems
    /// - proper systems with nonzero direct feedthrough `D`
    ///
    /// but not strictly improper transfer functions.
    ImproperTransferFunction {
        /// Degree of the numerator polynomial.
        numerator_degree: usize,
        /// Degree of the denominator polynomial.
        denominator_degree: usize,
    },
    /// A conversion from complex roots back to real coefficients requires the
    /// root set to be closed under complex conjugation.
    NotConjugateClosed {
        /// Identifies the root set that violated conjugate closure.
        which: &'static str,
    },
    /// Transfer-function inversion is undefined for the identically zero map.
    ZeroTransferInverse,
    /// Transfer-function division is undefined when the divisor is the
    /// identically zero map.
    ZeroTransferDivisor,
    /// A response or conversion formula produced non-finite values.
    NonFiniteResult {
        /// Identifies the computation that produced non-finite values.
        which: &'static str,
    },
    /// A scalar or sample-time cast between numeric dtypes failed.
    ScalarConversionFailed {
        /// Identifies the representation field that could not be cast.
        which: &'static str,
    },
    /// A second-order-section cascade must contain at least one section.
    EmptySos,
    /// A supplied filter-runtime state object has the wrong structural length.
    ///
    /// This is used by the stateful SOS simulation path, where callers can
    /// retain and reuse per-section delay state across multiple chunks.
    InvalidFilterStateLength {
        /// Identifies the runtime state object being validated.
        which: &'static str,
        /// Required number of stored delay elements.
        expected: usize,
        /// Actual number of stored delay elements.
        actual: usize,
    },
    /// A Savitzky-Golay design specification is invalid.
    InvalidSavGolSpec {
        /// Identifies the invalid Savitzky-Golay parameter.
        which: &'static str,
    },
    /// A decomposition used by an LTI helper failed.
    Decomp(DecompError),
    /// A dense state-space helper used underneath an LTI analysis routine
    /// failed.
    StateSpace(StateSpaceError),
    /// Sparse CSC construction failed while building an analysis operator.
    SparseBuild(CreationError),
    /// Sparse format conversion failed.
    SparseFormat(FaerError),
    /// Sparse LU analysis, factorization, or solve failed.
    SparseLu(SparseLuError),
}

impl fmt::Display for LtiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

impl core::error::Error for LtiError {}

impl From<EvdError> for LtiError {
    fn from(value: EvdError) -> Self {
        Self::Eigen(value)
    }
}

impl From<SvdError> for LtiError {
    fn from(value: SvdError) -> Self {
        Self::Svd(value)
    }
}

impl From<StateSpaceError> for LtiError {
    fn from(value: StateSpaceError) -> Self {
        Self::StateSpace(value)
    }
}

impl From<DecompError> for LtiError {
    fn from(value: DecompError) -> Self {
        Self::Decomp(value)
    }
}

impl From<CreationError> for LtiError {
    fn from(value: CreationError) -> Self {
        Self::SparseBuild(value)
    }
}

impl From<FaerError> for LtiError {
    fn from(value: FaerError) -> Self {
        Self::SparseFormat(value)
    }
}

impl From<SparseLuError> for LtiError {
    fn from(value: SparseLuError) -> Self {
        Self::SparseLu(value)
    }
}
