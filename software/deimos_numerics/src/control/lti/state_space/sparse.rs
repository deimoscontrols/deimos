//! Sparse CSC-backed state-space model types.
//!
//! The sparse control path uses the same conceptual `A/B/C/D` layout as the
//! dense model layer, but keeps only the state matrix `A` sparse in the first
//! implementation. That reflects the common large-scale controls case:
//!
//! - `A` is the large sparse dynamics operator
//! - `B`, `C`, and `D` are comparatively small dense maps
//!
//! This shape is enough to support sparse simulation, sparse frequency
//! response, low-rank Gramian solves, and sparse balanced truncation without
//! forcing the user to densify the full model.
//!
//! # Glossary
//!
//! - **CSC:** Compressed sparse column storage.
//! - **Gramian:** Matrix measuring controllability or observability energy.
//! - **Low-rank solve:** Approximation that stores or computes a factorized
//!   Gramian instead of a full dense one.
//! - **Balanced truncation:** Model-reduction method based on matched
//!   controllability and observability coordinates.

use super::{ContinuousTime, DiscreteTime, StateSpaceError, validate_blocks};
use crate::control::matrix_equations::lyapunov::{
    LowRankLyapunovSolve, LyapunovError, LyapunovParams, ShiftStrategy,
    controllability_gramian_low_rank, observability_gramian_low_rank,
};
use crate::control::matrix_equations::stein::{
    SteinError, controllability_gramian_discrete_low_rank, observability_gramian_discrete_low_rank,
};
use crate::control::reduction::{
    BalancedError, BalancedParams, BalancedTruncationResult,
    balanced_truncation_continuous_low_rank, balanced_truncation_discrete_low_rank,
};
use crate::sparse::compensated::CompensatedField;
use faer::sparse::{SparseColMat, SparseColMatRef};
use faer::{Mat, MatRef};
use faer_traits::ComplexField;
use num_traits::{Float, Zero};

/// Sparse linear time-invariant state-space system with CSC state matrix.
///
/// This is the sparse sibling of [`super::StateSpace`]. The main difference is
/// that `A` is stored as an owned CSC matrix, while `B`, `C`, and `D` remain
/// dense.
#[derive(Clone, Debug)]
pub struct SparseStateSpace<T, Domain> {
    pub(crate) a: SparseColMat<usize, T>,
    pub(crate) b: Mat<T>,
    pub(crate) c: Mat<T>,
    pub(crate) d: Mat<T>,
    pub(crate) domain: Domain,
}

/// Sparse continuous-time state-space system with CSC `A`.
pub type SparseContinuousStateSpace<T> = SparseStateSpace<T, ContinuousTime>;

/// Sparse discrete-time state-space system with CSC `A`.
pub type SparseDiscreteStateSpace<T> = SparseStateSpace<T, DiscreteTime<<T as ComplexField>::Real>>;

impl<T, Domain> SparseStateSpace<T, Domain> {
    /// Number of states.
    #[must_use]
    pub fn nstates(&self) -> usize {
        self.a.nrows()
    }

    /// Number of inputs.
    #[must_use]
    pub fn ninputs(&self) -> usize {
        self.b.ncols()
    }

    /// Number of outputs.
    #[must_use]
    pub fn noutputs(&self) -> usize {
        self.c.nrows()
    }

    /// Returns whether the system is single-input single-output.
    #[must_use]
    pub fn is_siso(&self) -> bool {
        self.ninputs() == 1 && self.noutputs() == 1
    }

    /// Sparse state matrix `A`.
    #[must_use]
    pub fn a(&self) -> SparseColMatRef<'_, usize, T> {
        self.a.as_ref()
    }

    /// Dense input matrix `B`.
    #[must_use]
    pub fn b(&self) -> MatRef<'_, T> {
        self.b.as_ref()
    }

    /// Dense output matrix `C`.
    #[must_use]
    pub fn c(&self) -> MatRef<'_, T> {
        self.c.as_ref()
    }

    /// Dense feedthrough matrix `D`.
    #[must_use]
    pub fn d(&self) -> MatRef<'_, T> {
        self.d.as_ref()
    }

    /// Domain metadata carried by the model.
    #[must_use]
    pub fn domain(&self) -> &Domain {
        &self.domain
    }

    /// Splits the model back into its owned parts.
    #[must_use]
    pub fn into_parts(self) -> (SparseColMat<usize, T>, Mat<T>, Mat<T>, Mat<T>, Domain) {
        (self.a, self.b, self.c, self.d, self.domain)
    }
}

impl<T> SparseContinuousStateSpace<T>
where
    T: ComplexField,
{
    /// Creates a sparse continuous-time state-space system with CSC `A`.
    ///
    /// The validated model represents
    ///
    /// `x' = A x + B u`
    ///
    /// `y  = C x + D u`
    pub fn new(
        a: SparseColMat<usize, T>,
        b: Mat<T>,
        c: Mat<T>,
        d: Mat<T>,
    ) -> Result<Self, StateSpaceError> {
        validate_blocks(
            a.nrows(),
            a.ncols(),
            b.nrows(),
            b.ncols(),
            c.nrows(),
            c.ncols(),
            d.nrows(),
            d.ncols(),
        )?;
        Ok(Self {
            a,
            b,
            c,
            d,
            domain: ContinuousTime,
        })
    }

    /// Creates a sparse continuous-time model with zero feedthrough.
    pub fn with_zero_feedthrough(
        a: SparseColMat<usize, T>,
        b: Mat<T>,
        c: Mat<T>,
    ) -> Result<Self, StateSpaceError> {
        let d = Mat::zeros(c.nrows(), b.ncols());
        Self::new(a, b, c, d)
    }
}

impl<T> SparseContinuousStateSpace<T>
where
    T: CompensatedField,
    T::Real: Float,
{
    /// Computes a sparse low-rank controllability Gramian factor.
    ///
    /// This is the large-scale counterpart of the dense controllability
    /// Gramian. The result is returned in factor form so the full dense
    /// `n x n` Gramian never needs to be formed.
    pub fn controllability_gramian(
        &self,
        shifts: &ShiftStrategy<T>,
        params: LyapunovParams<T::Real>,
    ) -> Result<LowRankLyapunovSolve<T>, LyapunovError> {
        controllability_gramian_low_rank(self.a.as_ref(), self.b.as_ref(), shifts, params)
    }

    /// Computes a sparse low-rank observability Gramian factor.
    pub fn observability_gramian(
        &self,
        shifts: &ShiftStrategy<T>,
        params: LyapunovParams<T::Real>,
    ) -> Result<LowRankLyapunovSolve<T>, LyapunovError> {
        observability_gramian_low_rank(self.a.as_ref(), self.c.as_ref(), shifts, params)
    }

    /// Computes sparse continuous-time low-rank balanced truncation.
    pub fn balanced_truncation(
        &self,
        shifts: &ShiftStrategy<T>,
        gramian_params: LyapunovParams<T::Real>,
        params: &BalancedParams<T::Real>,
    ) -> Result<BalancedTruncationResult<T, ContinuousTime>, BalancedError<T::Real>> {
        balanced_truncation_continuous_low_rank(
            self.a.as_ref(),
            self.b.as_ref(),
            self.c.as_ref(),
            self.d.as_ref(),
            shifts,
            gramian_params,
            params,
        )
    }
}

impl<T> SparseDiscreteStateSpace<T>
where
    T: ComplexField,
    T::Real: Float,
{
    /// Creates a sparse discrete-time state-space system with CSC `A`.
    ///
    /// The validated model represents
    ///
    /// `x[k + 1] = A x[k] + B u[k]`
    ///
    /// `y[k]     = C x[k] + D u[k]`
    pub fn new(
        a: SparseColMat<usize, T>,
        b: Mat<T>,
        c: Mat<T>,
        d: Mat<T>,
        sample_time: T::Real,
    ) -> Result<Self, StateSpaceError> {
        validate_blocks(
            a.nrows(),
            a.ncols(),
            b.nrows(),
            b.ncols(),
            c.nrows(),
            c.ncols(),
            d.nrows(),
            d.ncols(),
        )?;
        if !sample_time.is_finite() || sample_time <= T::Real::zero() {
            return Err(StateSpaceError::InvalidSampleTime);
        }
        Ok(Self {
            a,
            b,
            c,
            d,
            domain: DiscreteTime::new(sample_time),
        })
    }

    /// Creates a sparse discrete-time model with zero feedthrough.
    pub fn with_zero_feedthrough(
        a: SparseColMat<usize, T>,
        b: Mat<T>,
        c: Mat<T>,
        sample_time: T::Real,
    ) -> Result<Self, StateSpaceError> {
        let d = Mat::zeros(c.nrows(), b.ncols());
        Self::new(a, b, c, d, sample_time)
    }

    /// Sample interval used by the discrete-time model.
    #[must_use]
    pub fn sample_time(&self) -> T::Real {
        self.domain.sample_time()
    }
}

impl<T> SparseDiscreteStateSpace<T>
where
    T: CompensatedField,
    T::Real: Float,
{
    /// Computes a sparse low-rank discrete controllability Gramian factor.
    pub fn controllability_gramian(
        &self,
        shifts: &ShiftStrategy<T>,
        params: LyapunovParams<T::Real>,
    ) -> Result<LowRankLyapunovSolve<T>, SteinError> {
        controllability_gramian_discrete_low_rank(self.a.as_ref(), self.b.as_ref(), shifts, params)
    }

    /// Computes a sparse low-rank discrete observability Gramian factor.
    pub fn observability_gramian(
        &self,
        shifts: &ShiftStrategy<T>,
        params: LyapunovParams<T::Real>,
    ) -> Result<LowRankLyapunovSolve<T>, SteinError> {
        observability_gramian_discrete_low_rank(self.a.as_ref(), self.c.as_ref(), shifts, params)
    }

    /// Computes sparse discrete-time low-rank balanced truncation.
    pub fn balanced_truncation(
        &self,
        shifts: &ShiftStrategy<T>,
        gramian_params: LyapunovParams<T::Real>,
        params: &BalancedParams<T::Real>,
    ) -> Result<BalancedTruncationResult<T, DiscreteTime<T::Real>>, BalancedError<T::Real>> {
        balanced_truncation_discrete_low_rank(
            self.a.as_ref(),
            self.b.as_ref(),
            self.c.as_ref(),
            self.d.as_ref(),
            self.sample_time(),
            shifts,
            gramian_params,
            params,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::{SparseContinuousStateSpace, SparseDiscreteStateSpace};
    use crate::control::lti::state_space::StateSpaceError;
    use alloc::vec::Vec;
    use faer::Mat;
    use faer::sparse::{SparseColMat, Triplet};

    fn diagonal_csc(diag: &[f64]) -> SparseColMat<usize, f64> {
        let triplets = diag
            .iter()
            .enumerate()
            .map(|(i, &value)| Triplet::new(i, i, value))
            .collect::<Vec<_>>();
        SparseColMat::try_new_from_triplets(diag.len(), diag.len(), &triplets).unwrap()
    }

    #[test]
    fn sparse_continuous_constructor_rejects_bad_dimensions() {
        let a = diagonal_csc(&[-1.0, -2.0]);
        let b = Mat::<f64>::zeros(3, 1);
        let c = Mat::<f64>::zeros(1, 2);
        let d = Mat::<f64>::zeros(1, 1);
        let err = SparseContinuousStateSpace::new(a, b, c, d).unwrap_err();
        assert!(matches!(
            err,
            StateSpaceError::DimensionMismatch { which: "b", .. }
        ));
    }

    #[test]
    fn sparse_discrete_constructor_rejects_invalid_sample_time() {
        let a = diagonal_csc(&[0.5]);
        let b = Mat::<f64>::zeros(1, 1);
        let c = Mat::<f64>::zeros(1, 1);
        let d = Mat::<f64>::zeros(1, 1);
        let err = SparseDiscreteStateSpace::new(a, b, c, d, 0.0).unwrap_err();
        assert_eq!(err, StateSpaceError::InvalidSampleTime);
    }

    #[test]
    fn sparse_zero_feedthrough_constructor_sizes_d_correctly() {
        let a = diagonal_csc(&[-1.0, -2.0]);
        let b = Mat::<f64>::zeros(2, 3);
        let c = Mat::<f64>::zeros(4, 2);
        let sys = SparseContinuousStateSpace::with_zero_feedthrough(a, b, c).unwrap();
        assert_eq!(sys.d().nrows(), 4);
        assert_eq!(sys.d().ncols(), 3);
    }
}
