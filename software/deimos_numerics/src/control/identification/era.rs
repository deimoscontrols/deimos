//! Eigensystem realization algorithm (ERA).
//!
//! # Two Intuitions
//!
//! 1. **Impulse-response view.** ERA turns the first few impulse-response
//!    blocks of a system into a compact state-space model that reproduces those
//!    blocks.
//! 2. **Shifted-subspace view.** The same procedure can be seen as discovering
//!    a low-rank shift-invariant subspace inside a pair of block-Hankel
//!    matrices and reading the dynamics matrix `A` out of the shift.
//!
//! # Glossary
//!
//! - **Markov sequence:** Discrete-time impulse-response blocks `H_k`.
//! - **Shifted Hankel pair:** Two Hankel matrices offset by one time step.
//! - **Retained order:** Rank or model size kept after truncation.
//!
//! # Mathematical Formulation
//!
//! ERA builds a block-Hankel pair `(H_0, H_1)`, computes an SVD of `H_0`, and
//! uses the retained singular subspace to form a realization whose `A` matrix
//! is the one-step shift seen through that subspace.
//!
//! # Implementation Notes
//!
//! - The module accepts either a raw Markov sequence or an already assembled
//!   shifted Hankel pair.
//! - The direct term `D = H_0` must be supplied explicitly on the shifted-pair
//!   entry point because the shifted pair alone does not contain it.
//! - Returned internals mirror the actual algebra used to build the realized
//!   model so later debugging or research workflows can inspect the retained
//!   subspace data.

use crate::control::dense_ops::dense_mul;
use crate::control::lti::state_space::{DiscreteStateSpace, StateSpaceError};
use crate::control::realization::{MarkovSequence, RealizationError, ShiftedBlockHankelPair};
use crate::decomp::{DecompError, DenseDecompParams, dense_svd};
use crate::sparse::compensated::CompensatedField;
use core::fmt;
use faer::{Col, ColRef, Mat, MatRef};
use faer_traits::ext::ComplexFieldExt;
use num_traits::Float;

/// Controls how much internal ERA data is retained in the returned result.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum EraInternalsLevel {
    /// Return only the realized model and the retained singular values.
    #[default]
    Summary,
    /// Also retain the block-Hankel pair used by ERA.
    Hankel,
    /// Also retain the truncated SVD factors and derived observability /
    /// controllability factors.
    Full,
}

impl EraInternalsLevel {
    #[inline]
    fn keep_hankel(self) -> bool {
        !matches!(self, Self::Summary)
    }

    #[inline]
    fn keep_full(self) -> bool {
        matches!(self, Self::Full)
    }
}

/// Builder-style parameters for ERA.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EraParams<R> {
    /// Requested retained order. `None` keeps all numerically retained modes.
    pub order: Option<usize>,
    /// Optional tolerance on Hankel singular values. If omitted, a relative
    /// machine-precision-based default is used.
    pub sigma_tol: Option<R>,
    /// Sample interval assigned to the realized discrete-time model.
    pub sample_time: R,
    /// Requested level of retained internal ERA data.
    pub internals: EraInternalsLevel,
}

impl<R> EraParams<R> {
    /// Creates parameters with documented defaults for the supplied sample
    /// interval.
    #[must_use]
    pub fn new(sample_time: R) -> Self {
        Self {
            order: None,
            sigma_tol: None,
            sample_time,
            internals: EraInternalsLevel::Summary,
        }
    }

    /// Requests a specific retained order.
    #[must_use]
    pub fn with_order(mut self, order: usize) -> Self {
        self.order = Some(order);
        self
    }

    /// Overrides the Hankel singular value retention tolerance.
    #[must_use]
    pub fn with_sigma_tol(mut self, sigma_tol: R) -> Self {
        self.sigma_tol = Some(sigma_tol);
        self
    }

    /// Chooses how much internal ERA data to retain.
    #[must_use]
    pub fn with_internals(mut self, internals: EraInternalsLevel) -> Self {
        self.internals = internals;
        self
    }
}

/// Optional retained internal ERA data.
#[derive(Clone, Debug)]
pub struct EraInternals<T: CompensatedField>
where
    T::Real: Float,
{
    /// Unshifted block-Hankel matrix `H0`, when requested.
    pub h0: Option<crate::control::realization::BlockHankel<T>>,
    /// Shifted block-Hankel matrix `H1`, when requested.
    pub h1: Option<crate::control::realization::BlockHankel<T>>,
    /// Truncated left singular vectors of `H0`, when requested.
    pub u_r: Option<Mat<T>>,
    /// Retained singular values of `H0`, when requested.
    pub sigma_r: Option<Col<T::Real>>,
    /// Adjoint of the truncated right singular vector matrix, when requested.
    pub v_r_h: Option<Mat<T>>,
    /// Extended observability factor `U_r Σ_r^{1/2}`, when requested.
    pub observability_factor: Option<Mat<T>>,
    /// Extended controllability factor `Σ_r^{1/2} V_r^H`, when requested.
    pub controllability_factor: Option<Mat<T>>,
}

/// Result of ERA realization.
#[derive(Clone, Debug)]
pub struct EraResult<T: CompensatedField>
where
    T::Real: Float,
{
    /// Realized discrete-time state-space model.
    pub realized: DiscreteStateSpace<T>,
    /// Singular values of the ERA block-Hankel matrix `H0`, ordered by
    /// descending magnitude.
    pub singular_values: Col<T::Real>,
    /// Final retained realization order.
    pub retained_order: usize,
    /// Optional retained ERA internal data.
    pub internals: Option<EraInternals<T>>,
}

/// Errors produced by ERA front-ends.
#[derive(Debug)]
pub enum EraError {
    /// Markov or block-Hankel assembly failed.
    Realization(RealizationError),
    /// Dense SVD failed.
    Decomposition(DecompError),
    /// Final state-space construction failed.
    StateSpace(StateSpaceError),
    /// The requested retained order exceeds the available spectrum size.
    InvalidOrder {
        /// Requested reduced order.
        requested: usize,
        /// Largest order that can be retained from the assembled data.
        available: usize,
    },
    /// The direct term `D` did not match the block output/input dimensions.
    DirectFeedthroughDimensionMismatch {
        /// Required row count implied by the Markov and Hankel data.
        expected_nrows: usize,
        /// Required column count implied by the Markov and Hankel data.
        expected_ncols: usize,
        /// Actual row count in the supplied `D` block.
        actual_nrows: usize,
        /// Actual column count in the supplied `D` block.
        actual_ncols: usize,
    },
    /// No numerically meaningful Hankel singular values remained after
    /// thresholding.
    EmptyRetainedSpectrum,
    /// A derived factor or realized matrix contained a non-finite entry.
    NonFiniteResult {
        /// Identifies the derived quantity that contained the non-finite entry.
        which: &'static str,
    },
}

impl fmt::Display for EraError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

impl core::error::Error for EraError {}

impl From<RealizationError> for EraError {
    fn from(value: RealizationError) -> Self {
        Self::Realization(value)
    }
}

impl From<DecompError> for EraError {
    fn from(value: DecompError) -> Self {
        Self::Decomposition(value)
    }
}

impl From<StateSpaceError> for EraError {
    fn from(value: StateSpaceError) -> Self {
        Self::StateSpace(value)
    }
}

/// Runs ERA starting from a discrete-time Markov sequence.
///
/// This is the main convenience entry point. It builds the standard shifted
/// Hankel pair `(H0, H1)` from the sequence and then delegates to
/// [`era_from_shifted_hankel`].
pub fn era_from_markov<T>(
    sequence: &MarkovSequence<T>,
    row_blocks: usize,
    col_blocks: usize,
    params: &EraParams<T::Real>,
) -> Result<EraResult<T>, EraError>
where
    T: CompensatedField,
    T::Real: Float,
{
    let pair = sequence.shifted_hankel_pair(row_blocks, col_blocks)?;
    let direct = sequence.block(0);
    era_from_shifted_hankel(&pair, direct, params)
}

/// Runs ERA from a preassembled shifted block-Hankel pair plus the direct term
/// `D = H_0`.
///
/// `H0/H1` alone do not contain the feedthrough block, so callers that already
/// hold the shifted Hankel pair must supply `D` explicitly.
pub fn era_from_shifted_hankel<T>(
    pair: &ShiftedBlockHankelPair<T>,
    direct_feedthrough: MatRef<'_, T>,
    params: &EraParams<T::Real>,
) -> Result<EraResult<T>, EraError>
where
    T: CompensatedField,
    T::Real: Float,
{
    validate_direct_feedthrough(pair, direct_feedthrough)?;

    let svd = dense_svd(pair.h0().matrix(), &DenseDecompParams::<T>::new())?;
    let singular_values = real_singular_values(svd.s.as_ref());
    let retained_order = retained_order(&singular_values, params)?;

    let realized = if retained_order == 0 {
        zero_order_realization(
            pair.h0().noutputs(),
            pair.h0().ninputs(),
            direct_feedthrough,
            params.sample_time,
        )?
    } else {
        let u_r = truncated_columns(svd.u.as_ref(), retained_order);
        let v_r = truncated_columns(svd.v.as_ref(), retained_order);
        let sigma_r = truncated_real_col(singular_values.as_ref(), retained_order);
        let sigma_sqrt = diagonal_from_real(&sigma_r, RealScale::Sqrt);
        let sigma_inv_sqrt = diagonal_from_real(&sigma_r, RealScale::InvSqrt);
        let observability_factor = dense_mul(u_r.as_ref(), sigma_sqrt.as_ref());
        let v_r_h = v_r.adjoint().to_owned();
        let controllability_factor = dense_mul(sigma_sqrt.as_ref(), v_r_h.as_ref());
        let a_mid = dense_mul(
            dense_mul(u_r.adjoint().to_owned().as_ref(), pair.h1().matrix()).as_ref(),
            v_r.as_ref(),
        );
        let a_r = dense_mul(
            dense_mul(sigma_inv_sqrt.as_ref(), a_mid.as_ref()).as_ref(),
            sigma_inv_sqrt.as_ref(),
        );
        let b_r = first_columns(controllability_factor.as_ref(), pair.h0().ninputs());
        let c_r = first_rows(observability_factor.as_ref(), pair.h0().noutputs());

        check_finite(a_r.as_ref(), "a_r")?;
        check_finite(b_r.as_ref(), "b_r")?;
        check_finite(c_r.as_ref(), "c_r")?;
        check_finite(direct_feedthrough, "d_r")?;

        DiscreteStateSpace::new(
            a_r,
            b_r,
            c_r,
            direct_feedthrough.to_owned(),
            params.sample_time,
        )?
    };

    let internals = if matches!(params.internals, EraInternalsLevel::Summary) {
        None
    } else {
        let mut internals = EraInternals {
            h0: None,
            h1: None,
            u_r: None,
            sigma_r: None,
            v_r_h: None,
            observability_factor: None,
            controllability_factor: None,
        };
        if params.internals.keep_hankel() {
            internals.h0 = Some(pair.h0().clone());
            internals.h1 = Some(pair.h1().clone());
        }
        if params.internals.keep_full() && retained_order > 0 {
            let u_r = truncated_columns(svd.u.as_ref(), retained_order);
            let v_r = truncated_columns(svd.v.as_ref(), retained_order);
            let sigma_r = truncated_real_col(singular_values.as_ref(), retained_order);
            let sigma_sqrt = diagonal_from_real(&sigma_r, RealScale::Sqrt);
            let observability_factor = dense_mul(u_r.as_ref(), sigma_sqrt.as_ref());
            let v_r_h = v_r.adjoint().to_owned();
            let controllability_factor = dense_mul(sigma_sqrt.as_ref(), v_r_h.as_ref());
            internals.u_r = Some(u_r);
            internals.sigma_r = Some(sigma_r);
            internals.v_r_h = Some(v_r_h);
            internals.observability_factor = Some(observability_factor);
            internals.controllability_factor = Some(controllability_factor);
        }
        Some(internals)
    };

    Ok(EraResult {
        realized,
        singular_values,
        retained_order,
        internals,
    })
}

fn validate_direct_feedthrough<T>(
    pair: &ShiftedBlockHankelPair<T>,
    direct_feedthrough: MatRef<'_, T>,
) -> Result<(), EraError>
where
    T: CompensatedField,
    T::Real: Float,
{
    if direct_feedthrough.nrows() != pair.h0().noutputs()
        || direct_feedthrough.ncols() != pair.h0().ninputs()
    {
        return Err(EraError::DirectFeedthroughDimensionMismatch {
            expected_nrows: pair.h0().noutputs(),
            expected_ncols: pair.h0().ninputs(),
            actual_nrows: direct_feedthrough.nrows(),
            actual_ncols: direct_feedthrough.ncols(),
        });
    }
    Ok(())
}

fn retained_order<R>(singular_values: &Col<R>, params: &EraParams<R>) -> Result<usize, EraError>
where
    R: Float,
{
    let mut max_sigma = R::zero();
    for i in 0..singular_values.nrows() {
        if singular_values[i] > max_sigma {
            max_sigma = singular_values[i];
        }
    }
    let sigma_tol = params
        .sigma_tol
        .unwrap_or_else(|| R::epsilon().sqrt() * max_sigma);
    let available = (0..singular_values.nrows())
        .take_while(|&i| singular_values[i] > sigma_tol)
        .count();
    let retained_order = match params.order {
        Some(order) if order > singular_values.nrows() => {
            return Err(EraError::InvalidOrder {
                requested: order,
                available: singular_values.nrows(),
            });
        }
        Some(order) if params.sigma_tol.is_some() => order.min(available),
        Some(order) if order > available => {
            return Err(EraError::InvalidOrder {
                requested: order,
                available,
            });
        }
        Some(order) => order,
        None => available,
    };
    if retained_order == 0 && params.order != Some(0) {
        return Err(EraError::EmptyRetainedSpectrum);
    }
    Ok(retained_order)
}

fn zero_order_realization<T>(
    noutputs: usize,
    ninputs: usize,
    direct_feedthrough: MatRef<'_, T>,
    sample_time: T::Real,
) -> Result<DiscreteStateSpace<T>, StateSpaceError>
where
    T: CompensatedField,
    T::Real: Float,
{
    DiscreteStateSpace::new(
        Mat::zeros(0, 0),
        Mat::zeros(0, ninputs),
        Mat::zeros(noutputs, 0),
        direct_feedthrough.to_owned(),
        sample_time,
    )
}

fn real_singular_values<T>(values: ColRef<'_, T>) -> Col<T::Real>
where
    T: CompensatedField,
    T::Real: Float,
{
    Col::from_fn(values.nrows(), |i| values[i].abs())
}

fn truncated_columns<T: Copy>(matrix: MatRef<'_, T>, ncols: usize) -> Mat<T> {
    Mat::from_fn(matrix.nrows(), ncols, |row, col| matrix[(row, col)])
}

fn truncated_real_col<R: Copy>(col: faer::ColRef<'_, R>, len: usize) -> Col<R> {
    Col::from_fn(len, |i| col[i])
}

#[derive(Clone, Copy)]
enum RealScale {
    Sqrt,
    InvSqrt,
}

fn diagonal_from_real<T>(values: &Col<T::Real>, scale: RealScale) -> Mat<T>
where
    T: CompensatedField,
    T::Real: Float,
{
    let mut out = Mat::zeros(values.nrows(), values.nrows());
    for i in 0..values.nrows() {
        let value = match scale {
            RealScale::Sqrt => values[i].sqrt(),
            RealScale::InvSqrt => values[i].sqrt().recip(),
        };
        out[(i, i)] = T::one().mul_real(value);
    }
    out
}

fn first_rows<T: Copy>(matrix: MatRef<'_, T>, nrows: usize) -> Mat<T> {
    Mat::from_fn(nrows, matrix.ncols(), |row, col| matrix[(row, col)])
}

fn first_columns<T: Copy>(matrix: MatRef<'_, T>, ncols: usize) -> Mat<T> {
    Mat::from_fn(matrix.nrows(), ncols, |row, col| matrix[(row, col)])
}

fn check_finite<T>(matrix: MatRef<'_, T>, which: &'static str) -> Result<(), EraError>
where
    T: CompensatedField,
    T::Real: Float,
{
    for col in 0..matrix.ncols() {
        for row in 0..matrix.nrows() {
            let value = matrix[(row, col)];
            if !value.real().is_finite() || !value.imag().is_finite() {
                return Err(EraError::NonFiniteResult { which });
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{EraInternalsLevel, EraParams, era_from_markov, era_from_shifted_hankel};
    use crate::control::lti::state_space::DiscreteStateSpace;
    use crate::control::realization::MarkovSequence;
    use faer::{Mat, MatRef};

    fn assert_close(lhs: MatRef<'_, f64>, rhs: MatRef<'_, f64>, tol: f64) {
        assert_eq!(lhs.nrows(), rhs.nrows());
        assert_eq!(lhs.ncols(), rhs.ncols());
        for col in 0..lhs.ncols() {
            for row in 0..lhs.nrows() {
                let err = (lhs[(row, col)] - rhs[(row, col)]).abs();
                assert!(
                    err <= tol,
                    "entry ({row}, {col}) differs: lhs={}, rhs={}, err={err}, tol={tol}",
                    lhs[(row, col)],
                    rhs[(row, col)],
                );
            }
        }
    }

    fn scalar_system() -> DiscreteStateSpace<f64> {
        DiscreteStateSpace::new(
            Mat::from_fn(1, 1, |_, _| 0.5),
            Mat::from_fn(1, 1, |_, _| 2.0),
            Mat::from_fn(1, 1, |_, _| 3.0),
            Mat::from_fn(1, 1, |_, _| 4.0),
            0.1,
        )
        .unwrap()
    }

    #[test]
    fn era_from_markov_recovers_scalar_response() {
        let sys = scalar_system();
        let sequence = sys.markov_parameters(6);
        let params = EraParams::new(sys.sample_time());
        let era = era_from_markov(&sequence, 2, 2, &params).unwrap();
        assert_eq!(era.retained_order, 1);
        let recovered = era.realized.markov_parameters(6);
        for k in 0..sequence.len() {
            assert_close(recovered.block(k), sequence.block(k), 1.0e-10);
        }
    }

    #[test]
    fn era_from_shifted_hankel_matches_markov_entry_point() {
        let sys = scalar_system();
        let sequence = sys.markov_parameters(6);
        let pair = sequence.shifted_hankel_pair(2, 2).unwrap();
        let params = EraParams::new(sys.sample_time()).with_internals(EraInternalsLevel::Full);
        let from_markov = era_from_markov(&sequence, 2, 2, &params).unwrap();
        let from_pair = era_from_shifted_hankel(&pair, sequence.block(0), &params).unwrap();
        let lhs = from_markov.realized.markov_parameters(6);
        let rhs = from_pair.realized.markov_parameters(6);
        for k in 0..lhs.len() {
            assert_close(lhs.block(k), rhs.block(k), 1.0e-10);
        }
        assert!(from_pair.internals.is_some());
    }

    #[test]
    fn era_handles_small_mimo_markov_sequence() {
        let sys = DiscreteStateSpace::new(
            Mat::from_fn(2, 2, |row, col| match (row, col) {
                (0, 0) => 0.5,
                (1, 1) => 0.25,
                _ => 0.0,
            }),
            Mat::from_fn(2, 2, |row, col| if row == col { 1.0 } else { 0.0 }),
            Mat::from_fn(2, 2, |row, col| if row == col { 2.0 } else { 0.0 }),
            Mat::from_fn(2, 2, |row, col| if row == col { 3.0 } else { 0.0 }),
            0.2,
        )
        .unwrap();
        let sequence = sys.markov_parameters(8);
        let era = era_from_markov(&sequence, 3, 3, &EraParams::new(sys.sample_time())).unwrap();
        let recovered = era.realized.markov_parameters(8);
        for k in 0..sequence.len() {
            assert_close(recovered.block(k), sequence.block(k), 1.0e-10);
        }
    }

    #[test]
    fn era_supports_order_zero_direct_term_only() {
        let sequence = MarkovSequence::from_blocks(vec![
            Mat::from_fn(1, 1, |_, _| 5.0),
            Mat::from_fn(1, 1, |_, _| 0.0),
            Mat::from_fn(1, 1, |_, _| 0.0),
        ])
        .unwrap();
        let era = era_from_markov(&sequence, 1, 1, &EraParams::new(1.0).with_order(0)).unwrap();
        assert_eq!(era.retained_order, 0);
        let recovered = era.realized.markov_parameters(3);
        assert_close(recovered.block(0), sequence.block(0), 1.0e-12);
        assert_close(recovered.block(1), sequence.block(1), 1.0e-12);
        assert_close(recovered.block(2), sequence.block(2), 1.0e-12);
    }
}
