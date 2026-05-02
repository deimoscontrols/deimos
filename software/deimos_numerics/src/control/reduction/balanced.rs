//! Balanced realization and balanced truncation for state-space models.
//!
//! This module relies on the Gramian solver set needed for balanced
//! realization and truncation:
//!
//! - dense continuous Lyapunov
//! - sparse continuous low-rank Lyapunov
//! - dense discrete Stein
//! - sparse discrete low-rank Stein
//!
//! This module sits above those solvers and performs the balancing step:
//!
//! 1. obtain controllability and observability Gramian data
//! 2. delegate the balancing-core work to [`super::hsvd`]
//! 3. assemble the reduced state-space system from the returned projections
//!
//! Standard balanced realization and truncation assume the plant is
//! asymptotically stable in the relevant time domain:
//!
//! - continuous time: all poles lie in the open left half-plane
//! - discrete time: all poles lie strictly inside the unit disk
//!
//! The implementation follows that standard setting. In practice this
//! means the underlying Gramian solves are only meaningful for stable systems,
//! so callers should check or enforce stability before using this module on an
//! open-loop plant.
//!
//! The returned result always includes the actual projection operators used to
//! build the reduced model, and can optionally retain more of the internal
//! balancing algebra on request.
//!
//! Literature:
//!
//! - Brunton and Kutz, *Data-Driven Science and Engineering*, 2nd ed.,
//!   Cambridge University Press, 2022, for a pedagogical treatment of
//!   balanced coordinates, balanced truncation, and model reduction.
//!
//! # Two Intuitions
//!
//! 1. **Input-output view.** Balanced truncation removes states that are both
//!    hard to reach from the inputs and hard to see at the outputs.
//! 2. **Full-rank view.** Balanced realization changes coordinates without
//!    changing input-output behavior, but it orders and scales the states so
//!    controllability and observability energy are directly comparable.
//! 3. **Projection view.** It is also just a carefully chosen pair of left and
//!    right projection matrices built from the HSVD core.
//!
//! # Glossary
//!
//! - **Balanced coordinates:** Coordinates where controllability and
//!   observability energies are aligned and diagonalized together.
//! - **Balanced realization:** Full numerical-rank coordinate change into
//!   balanced coordinates without an intentional order reduction.
//! - **Controllability energy:** Input effort associated with reaching a state
//!   direction, encoded by the controllability Gramian.
//! - **Observability energy:** Output response strength associated with a state
//!   direction, encoded by the observability Gramian.
//! - **Tail bound:** Classical balanced-truncation error bound based on
//!   discarded Hankel singular values.
//!
//! # Mathematical Formulation
//!
//! The module solves the appropriate Gramians, computes HSVD, and forms a
//! reduced model:
//!
//! - `A_r = W^H A V`
//! - `B_r = W^H B`
//! - `C_r = C V`
//! - `D_r = D`
//!
//! with `V` and `W` built from the retained balanced directions.
//!
//! # Implementation Notes
//!
//! - The module is a workflow layer on top of Gramian solvers plus `hsvd`.
//! - Dense and low-rank workflows share the same outward result contract.
//! - Stability is a precondition, not an internal stabilization step.

use super::hsvd::{HsvdError, hsvd_from_dense_gramians, hsvd_from_factors};
use crate::control::dense_ops::{dense_mul, dense_mul_adjoint_lhs};
use crate::control::lti::{
    ContinuousStateSpace, ContinuousTime, DiscreteStateSpace, DiscreteTime, StateSpaceError,
};
use crate::control::matrix_equations::lyapunov::{
    LyapunovError, LyapunovParams, ShiftStrategy, controllability_gramian_dense,
    controllability_gramian_low_rank, observability_gramian_dense, observability_gramian_low_rank,
};
use crate::control::matrix_equations::stein::{
    SteinError, controllability_gramian_discrete_dense, controllability_gramian_discrete_low_rank,
    observability_gramian_discrete_dense, observability_gramian_discrete_low_rank,
};
use crate::sparse::compensated::{CompensatedField, CompensatedSum};
use core::fmt;
use faer::sparse::SparseColMatRef;
use faer::{Col, Index, Mat, MatRef, Unbind};
use faer_traits::Conjugate;
use num_traits::{Float, Zero};

/// Alias of the shared HSVD internal-detail policy used by balanced
/// truncation.
///
/// Balanced truncation intentionally reuses the HSVD detail policy unchanged,
/// because the retained factors and core SVD are exactly the same objects the
/// reduction step consumes.
pub use super::hsvd::HsvdInternalsLevel as InternalsLevel;

/// Alias of the shared HSVD parameter type used by balanced truncation.
///
/// The truncation policy is shared with the standalone HSVD interface so that
/// callers see the same `order` / `sigma_tol` behavior whether they are asking
/// for just the projections or for a full reduced model.
pub use super::hsvd::HsvdParams as BalancedParams;

/// Alias of the shared HSVD internals retained by balanced truncation.
///
/// Balanced truncation does not add any additional internal balancing algebra
/// beyond what HSVD already exposes, so the internals type is reused directly.
pub use super::hsvd::HsvdInternals as BalancedInternals;

/// Result of balanced realization or balanced truncation.
///
/// The output model is accompanied by the actual left/right projection
/// operators used to assemble it. For balanced realization these are the
/// full numerical-rank balancing transforms; for balanced truncation they are
/// the retained projection factors.
///
/// The standard balanced-truncation interpretation of this result assumes the
/// original plant was asymptotically stable. If the input model is unstable,
/// the returned quantities should not be treated as a valid balanced
/// truncation of the original system.
#[derive(Clone, Debug)]
pub struct BalancedTruncationResult<T: CompensatedField, Domain>
where
    T::Real: Float,
{
    /// Reduced state-space model.
    pub reduced: crate::control::lti::state_space::StateSpace<T, Domain>,
    /// Hankel singular values in descending order.
    pub hankel_singular_values: Col<T::Real>,
    /// Final retained order.
    pub reduced_order: usize,
    /// Standard balanced-truncation tail bound when available.
    ///
    /// This bound is the usual stable-system BT bound. It is only meaningful
    /// under the same asymptotic-stability assumptions as the rest of the
    /// balanced-truncation construction.
    pub error_bound: Option<T::Real>,
    /// Left projection operator used to build the reduced model.
    pub left_projection: Mat<T>,
    /// Right projection operator used to build the reduced model.
    pub right_projection: Mat<T>,
    /// Optional retained internal balancing data.
    pub internals: Option<BalancedInternals<T>>,
}

/// Result of balanced realization.
///
/// This is the same storage as [`BalancedTruncationResult`]. The distinct name
/// documents the API intent: retain all positive Hankel singular directions
/// instead of intentionally selecting a smaller reduced order.
pub type BalancedRealizationResult<T, Domain> = BalancedTruncationResult<T, Domain>;

/// Errors produced by balanced-realization and balanced-truncation front ends.
#[derive(Debug)]
pub enum BalancedError<R> {
    /// Dense or sparse continuous Gramian solve failed.
    Lyapunov(LyapunovError),
    /// Dense or sparse discrete Gramian solve failed.
    Stein(SteinError),
    /// Hankel singular value decomposition or balancing-core construction
    /// failed. This is a direct passthrough of the reusable HSVD layer.
    Hsvd(HsvdError<R>),
    /// Reduced state-space construction failed.
    StateSpace(crate::control::lti::state_space::StateSpaceError),
}

impl<R: fmt::Debug> fmt::Display for BalancedError<R> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

impl<R: fmt::Debug> core::error::Error for BalancedError<R> {}

impl<R> From<LyapunovError> for BalancedError<R> {
    fn from(value: LyapunovError) -> Self {
        Self::Lyapunov(value)
    }
}

impl<R> From<SteinError> for BalancedError<R> {
    fn from(value: SteinError) -> Self {
        Self::Stein(value)
    }
}

impl<R> From<StateSpaceError> for BalancedError<R> {
    fn from(value: StateSpaceError) -> Self {
        Self::StateSpace(value)
    }
}

impl<R> From<HsvdError<R>> for BalancedError<R> {
    fn from(value: HsvdError<R>) -> Self {
        Self::Hsvd(value)
    }
}

/// Computes dense continuous-time balanced truncation.
///
/// This path solves the dense continuous controllability and observability
/// Gramians, converts each Gramian into a PSD square-root factor, forms the
/// balancing core `Ro^H Rc`, and then builds the reduced model from the
/// resulting left/right projection operators.
///
/// The input plant is assumed to be asymptotically stable. For continuous
/// time, that means all poles of `A` must lie strictly in the open left
/// half-plane.
pub fn balanced_truncation_continuous_dense<T>(
    system: &ContinuousStateSpace<T>,
    params: &BalancedParams<T::Real>,
) -> Result<BalancedTruncationResult<T, ContinuousTime>, BalancedError<T::Real>>
where
    T: CompensatedField,
    T::Real: Float,
{
    // Once HSVD has produced `S_r` and `T_r`, balanced truncation only has to
    // assemble the reduced state-space blocks with those projections.
    let wc = controllability_gramian_dense(system.a(), system.b())?;
    let wo = observability_gramian_dense(system.a(), system.c())?;
    let hsvd = hsvd_from_dense_gramians(wc.solution.as_ref(), wo.solution.as_ref(), params)?;
    let reduced = build_dense_reduced_system(
        system.a(),
        system.b(),
        system.c(),
        system.d(),
        hsvd.left_projection.as_ref(),
        hsvd.right_projection.as_ref(),
    )?;

    Ok(BalancedTruncationResult {
        reduced,
        hankel_singular_values: hsvd.hankel_singular_values,
        reduced_order: hsvd.reduced_order,
        error_bound: hsvd.error_bound,
        left_projection: hsvd.left_projection,
        right_projection: hsvd.right_projection,
        internals: hsvd.internals,
    })
}

/// Computes dense discrete-time balanced truncation.
///
/// The balancing algebra is identical to the continuous-time path; only the
/// source of the Gramians changes from Lyapunov solves to discrete Stein
/// solves.
///
/// The input plant is assumed to be asymptotically stable. For discrete time,
/// that means all poles of `A` must lie strictly inside the unit disk.
pub fn balanced_truncation_discrete_dense<T>(
    system: &DiscreteStateSpace<T>,
    params: &BalancedParams<T::Real>,
) -> Result<BalancedTruncationResult<T, DiscreteTime<T::Real>>, BalancedError<T::Real>>
where
    T: CompensatedField,
    T::Real: Float,
{
    // Discrete balanced truncation uses the same projection algebra as the
    // continuous case; only the Gramian source differs.
    let wc = controllability_gramian_discrete_dense(system.a(), system.b())?;
    let wo = observability_gramian_discrete_dense(system.a(), system.c())?;
    let hsvd = hsvd_from_dense_gramians(wc.solution.as_ref(), wo.solution.as_ref(), params)?;
    let reduced = build_dense_reduced_discrete_system(
        system.a(),
        system.b(),
        system.c(),
        system.d(),
        system.sample_time(),
        hsvd.left_projection.as_ref(),
        hsvd.right_projection.as_ref(),
    )?;

    Ok(BalancedTruncationResult {
        reduced,
        hankel_singular_values: hsvd.hankel_singular_values,
        reduced_order: hsvd.reduced_order,
        error_bound: hsvd.error_bound,
        left_projection: hsvd.left_projection,
        right_projection: hsvd.right_projection,
        internals: hsvd.internals,
    })
}

/// Computes a dense continuous-time balanced realization.
///
/// This is a convenience wrapper around dense balanced truncation with the
/// requested order set to the state dimension and the Hankel singular value
/// lower bound set to zero. For a minimal stable system this keeps all states.
/// Exact zero Hankel singular directions are still omitted because the
/// balancing transform would require dividing by `sqrt(sigma)`.
///
/// Args:
///     system: Stable continuous-time state-space model with `n` states.
///
/// Returns:
///     Balanced realization result whose model has up to `n` states. Fewer
///     states are returned only when exact zero Hankel singular directions are
///     present.
pub fn balanced_realization_continuous_dense<T>(
    system: &ContinuousStateSpace<T>,
) -> Result<BalancedRealizationResult<T, ContinuousTime>, BalancedError<T::Real>>
where
    T: CompensatedField,
    T::Real: Float,
{
    let params = full_rank_balanced_params(system.nstates());
    balanced_truncation_continuous_dense(system, &params)
}

/// Computes a dense discrete-time balanced realization.
///
/// This is a convenience wrapper around dense balanced truncation with the
/// requested order set to the state dimension and the Hankel singular value
/// lower bound set to zero. For a minimal stable system this keeps all states.
/// Exact zero Hankel singular directions are still omitted because the
/// balancing transform would require dividing by `sqrt(sigma)`.
///
/// Args:
///     system: Stable discrete-time state-space model with `n` states and
///     sample time in seconds.
///
/// Returns:
///     Balanced realization result whose model has up to `n` states. Fewer
///     states are returned only when exact zero Hankel singular directions are
///     present.
pub fn balanced_realization_discrete_dense<T>(
    system: &DiscreteStateSpace<T>,
) -> Result<BalancedRealizationResult<T, DiscreteTime<T::Real>>, BalancedError<T::Real>>
where
    T: CompensatedField,
    T::Real: Float,
{
    let params = full_rank_balanced_params(system.nstates());
    balanced_truncation_discrete_dense(system, &params)
}

/// Computes sparse continuous-time low-rank balanced truncation.
///
/// This path never forms dense full Gramians. It uses the low-rank factors
/// returned by the sparse Lyapunov solver directly, so the only dense object
/// introduced by the reduction step is the small balancing core `Zo^H Zc`.
///
/// The input plant is assumed to be asymptotically stable. The low-rank
/// continuous-time Gramian solve underneath this routine relies on that
/// assumption.
pub fn balanced_truncation_continuous_low_rank<I, T, ViewT>(
    a: SparseColMatRef<'_, I, ViewT>,
    b: MatRef<'_, T>,
    c: MatRef<'_, T>,
    d: MatRef<'_, T>,
    shifts: &ShiftStrategy<T>,
    gramian_params: LyapunovParams<T::Real>,
    params: &BalancedParams<T::Real>,
) -> Result<BalancedTruncationResult<T, ContinuousTime>, BalancedError<T::Real>>
where
    I: Index,
    T: CompensatedField,
    T::Real: Float,
    ViewT: Conjugate<Canonical = T>,
{
    validate_sparse_system_dims(a.nrows().unbound(), b, c, d)?;

    // The sparse path still reduces through the small dense HSVD core; the
    // sparse matrix only appears again when assembling the reduced `A_r`.
    let wc = controllability_gramian_low_rank(a, b, shifts, gramian_params)?;
    let wo = observability_gramian_low_rank(a, c, shifts, gramian_params)?;
    let hsvd = hsvd_from_factors(wc.factor.z.as_ref(), wo.factor.z.as_ref(), params)?;
    let reduced = build_sparse_reduced_system(
        a.canonical(),
        b,
        c,
        d,
        hsvd.left_projection.as_ref(),
        hsvd.right_projection.as_ref(),
    )?;

    Ok(BalancedTruncationResult {
        reduced,
        hankel_singular_values: hsvd.hankel_singular_values,
        reduced_order: hsvd.reduced_order,
        error_bound: hsvd.error_bound,
        left_projection: hsvd.left_projection,
        right_projection: hsvd.right_projection,
        internals: hsvd.internals,
    })
}

/// Computes sparse discrete-time low-rank balanced truncation.
///
/// As in the sparse continuous-time path, the discrete solver contributes only
/// low-rank Gramian factors. The reduction itself stays factor-based and avoids
/// materializing any dense `n x n` discrete Gramian.
///
/// The input plant is assumed to be asymptotically stable. The discrete
/// low-rank Stein solve underneath this routine relies on that assumption.
pub fn balanced_truncation_discrete_low_rank<I, T, ViewT>(
    a: SparseColMatRef<'_, I, ViewT>,
    b: MatRef<'_, T>,
    c: MatRef<'_, T>,
    d: MatRef<'_, T>,
    sample_time: T::Real,
    shifts: &ShiftStrategy<T>,
    gramian_params: LyapunovParams<T::Real>,
    params: &BalancedParams<T::Real>,
) -> Result<BalancedTruncationResult<T, DiscreteTime<T::Real>>, BalancedError<T::Real>>
where
    I: Index,
    T: CompensatedField,
    T::Real: Float,
    ViewT: Conjugate<Canonical = T>,
{
    validate_sparse_system_dims(a.nrows().unbound(), b, c, d)?;
    if !sample_time.is_finite() || sample_time <= <T::Real as Zero>::zero() {
        return Err(BalancedError::StateSpace(
            StateSpaceError::InvalidSampleTime,
        ));
    }

    // As in the continuous sparse path, the discrete low-rank factors feed the
    // same dense HSVD machinery before the reduced model is assembled.
    let wc = controllability_gramian_discrete_low_rank(a, b, shifts, gramian_params)?;
    let wo = observability_gramian_discrete_low_rank(a, c, shifts, gramian_params)?;
    let hsvd = hsvd_from_factors(wc.factor.z.as_ref(), wo.factor.z.as_ref(), params)?;
    let reduced = build_sparse_reduced_discrete_system(
        a.canonical(),
        b,
        c,
        d,
        sample_time,
        hsvd.left_projection.as_ref(),
        hsvd.right_projection.as_ref(),
    )?;

    Ok(BalancedTruncationResult {
        reduced,
        hankel_singular_values: hsvd.hankel_singular_values,
        reduced_order: hsvd.reduced_order,
        error_bound: hsvd.error_bound,
        left_projection: hsvd.left_projection,
        right_projection: hsvd.right_projection,
        internals: hsvd.internals,
    })
}

impl<T> ContinuousStateSpace<T>
where
    T: CompensatedField,
    T::Real: Float,
{
    /// Computes dense continuous-time balanced truncation for this model.
    ///
    /// The model is assumed to be asymptotically stable. Callers can use the
    /// LTI analysis layer to check this explicitly before truncation.
    pub fn balanced_truncation(
        &self,
        params: &BalancedParams<T::Real>,
    ) -> Result<BalancedTruncationResult<T, ContinuousTime>, BalancedError<T::Real>> {
        balanced_truncation_continuous_dense(self, params)
    }

    /// Computes a dense continuous-time balanced realization for this model.
    ///
    /// This keeps all positive Hankel singular directions and performs no
    /// intentional order reduction. The model is assumed to be asymptotically
    /// stable.
    ///
    /// Returns:
    ///     Balanced realization result with up to `n` states.
    pub fn balanced_realization(
        &self,
    ) -> Result<BalancedRealizationResult<T, ContinuousTime>, BalancedError<T::Real>> {
        balanced_realization_continuous_dense(self)
    }
}

impl<T> DiscreteStateSpace<T>
where
    T: CompensatedField,
    T::Real: Float,
{
    /// Computes dense discrete-time balanced truncation for this model.
    ///
    /// The model is assumed to be asymptotically stable. Callers can use the
    /// LTI analysis layer to check this explicitly before truncation.
    pub fn balanced_truncation(
        &self,
        params: &BalancedParams<T::Real>,
    ) -> Result<BalancedTruncationResult<T, DiscreteTime<T::Real>>, BalancedError<T::Real>> {
        balanced_truncation_discrete_dense(self, params)
    }

    /// Computes a dense discrete-time balanced realization for this model.
    ///
    /// This keeps all positive Hankel singular directions and performs no
    /// intentional order reduction. The model is assumed to be asymptotically
    /// stable.
    ///
    /// Returns:
    ///     Balanced realization result with up to `n` states and the original
    ///     sample time in seconds.
    pub fn balanced_realization(
        &self,
    ) -> Result<BalancedRealizationResult<T, DiscreteTime<T::Real>>, BalancedError<T::Real>> {
        balanced_realization_discrete_dense(self)
    }
}

fn full_rank_balanced_params<R>(nstates: usize) -> BalancedParams<R>
where
    R: Zero,
{
    BalancedParams::new()
        .with_order(nstates)
        .with_sigma_tol(R::zero())
}

fn build_dense_reduced_system<T>(
    a: MatRef<'_, T>,
    b: MatRef<'_, T>,
    c: MatRef<'_, T>,
    d: MatRef<'_, T>,
    left_projection: MatRef<'_, T>,
    right_projection: MatRef<'_, T>,
) -> Result<ContinuousStateSpace<T>, BalancedError<T::Real>>
where
    T: CompensatedField,
    T::Real: Float,
{
    // The reduced dense model is assembled by the usual Petrov-Galerkin
    // formulas: `A_r = S_r^H A T_r`, `B_r = S_r^H B`, `C_r = C T_r`.
    let ar = dense_mul_adjoint_lhs(left_projection, dense_mul(a, right_projection).as_ref());
    let br = dense_mul_adjoint_lhs(left_projection, b);
    let cr = dense_mul(c, right_projection);
    let dr = d.to_owned();
    Ok(ContinuousStateSpace::new(ar, br, cr, dr)?)
}

fn build_dense_reduced_discrete_system<T>(
    a: MatRef<'_, T>,
    b: MatRef<'_, T>,
    c: MatRef<'_, T>,
    d: MatRef<'_, T>,
    sample_time: T::Real,
    left_projection: MatRef<'_, T>,
    right_projection: MatRef<'_, T>,
) -> Result<DiscreteStateSpace<T>, BalancedError<T::Real>>
where
    T: CompensatedField,
    T::Real: Float,
{
    // The discrete assembly is identical to the continuous case apart from
    // preserving the sample time in the returned model metadata.
    let ar = dense_mul_adjoint_lhs(left_projection, dense_mul(a, right_projection).as_ref());
    let br = dense_mul_adjoint_lhs(left_projection, b);
    let cr = dense_mul(c, right_projection);
    let dr = d.to_owned();
    Ok(DiscreteStateSpace::new(ar, br, cr, dr, sample_time)?)
}

fn build_sparse_reduced_system<I, T>(
    a: SparseColMatRef<'_, I, T>,
    b: MatRef<'_, T>,
    c: MatRef<'_, T>,
    d: MatRef<'_, T>,
    left_projection: MatRef<'_, T>,
    right_projection: MatRef<'_, T>,
) -> Result<ContinuousStateSpace<T>, BalancedError<T::Real>>
where
    I: Index,
    T: CompensatedField,
    T::Real: Float,
{
    // The reduced sparse paths still return ordinary dense reduced models. The
    // expensive sparse object is only used while applying `A` to the retained
    // right projection.
    let ar = dense_mul_adjoint_lhs(
        left_projection,
        sparse_matmul_dense(a, right_projection).as_ref(),
    );
    let br = dense_mul_adjoint_lhs(left_projection, b);
    let cr = dense_mul(c, right_projection);
    let dr = d.to_owned();
    Ok(ContinuousStateSpace::new(ar, br, cr, dr)?)
}

fn build_sparse_reduced_discrete_system<I, T>(
    a: SparseColMatRef<'_, I, T>,
    b: MatRef<'_, T>,
    c: MatRef<'_, T>,
    d: MatRef<'_, T>,
    sample_time: T::Real,
    left_projection: MatRef<'_, T>,
    right_projection: MatRef<'_, T>,
) -> Result<DiscreteStateSpace<T>, BalancedError<T::Real>>
where
    I: Index,
    T: CompensatedField,
    T::Real: Float,
{
    // The sparse discrete path mirrors the sparse continuous one, but the
    // sample time must be preserved in the returned reduced model.
    let ar = dense_mul_adjoint_lhs(
        left_projection,
        sparse_matmul_dense(a, right_projection).as_ref(),
    );
    let br = dense_mul_adjoint_lhs(left_projection, b);
    let cr = dense_mul(c, right_projection);
    let dr = d.to_owned();
    Ok(DiscreteStateSpace::new(ar, br, cr, dr, sample_time)?)
}

fn validate_sparse_system_dims<T>(
    nstates: usize,
    b: MatRef<'_, T>,
    c: MatRef<'_, T>,
    d: MatRef<'_, T>,
) -> Result<(), BalancedError<T::Real>>
where
    T: CompensatedField,
    T::Real: Float,
{
    // Sparse balanced truncation still assembles an ordinary dense reduced
    // state-space system, so the same `A/B/C/D` compatibility rules apply at
    // the wrapper boundary before any projection work begins.
    if b.nrows() != nstates {
        return Err(BalancedError::StateSpace(
            StateSpaceError::DimensionMismatch {
                which: "b",
                expected_nrows: nstates,
                expected_ncols: b.ncols(),
                actual_nrows: b.nrows(),
                actual_ncols: b.ncols(),
            },
        ));
    }
    if c.ncols() != nstates {
        return Err(BalancedError::StateSpace(
            StateSpaceError::DimensionMismatch {
                which: "c",
                expected_nrows: c.nrows(),
                expected_ncols: nstates,
                actual_nrows: c.nrows(),
                actual_ncols: c.ncols(),
            },
        ));
    }
    if d.nrows() != c.nrows() || d.ncols() != b.ncols() {
        return Err(BalancedError::StateSpace(
            StateSpaceError::DimensionMismatch {
                which: "d",
                expected_nrows: c.nrows(),
                expected_ncols: b.ncols(),
                actual_nrows: d.nrows(),
                actual_ncols: d.ncols(),
            },
        ));
    }
    Ok(())
}

fn sparse_matmul_dense<I, T>(lhs: SparseColMatRef<'_, I, T>, rhs: MatRef<'_, T>) -> Mat<T>
where
    I: Index,
    T: CompensatedField,
    T::Real: Float,
{
    let lhs = lhs.canonical();
    let nrows = lhs.nrows().unbound();
    let ncols = lhs.ncols().unbound();
    assert_eq!(rhs.nrows(), ncols);

    let mut out = Mat::<T>::zeros(nrows, rhs.ncols());
    let col_ptr = lhs.col_ptr();
    let row_idx = lhs.row_idx();
    let values = lhs.val();

    for out_col in 0..rhs.ncols() {
        let mut acc = vec![CompensatedSum::<T>::default(); nrows];
        for lhs_col in 0..ncols {
            let rhs_value = rhs[(lhs_col, out_col)];
            let start = col_ptr[lhs_col].zx();
            let end = col_ptr[lhs_col + 1].zx();
            for idx in start..end {
                // Accumulate each output row with compensated sums so the
                // projection step is consistent with the full-accuracy policy
                // used throughout the control module.
                acc[row_idx[idx].zx()].add(values[idx] * rhs_value);
            }
        }
        for row in 0..nrows {
            out[(row, out_col)] = acc[row].finish();
        }
    }

    out
}

#[cfg(test)]
mod test {
    use super::{
        BalancedParams, InternalsLevel, balanced_realization_continuous_dense,
        balanced_realization_discrete_dense, balanced_truncation_continuous_dense,
        balanced_truncation_continuous_low_rank, balanced_truncation_discrete_dense,
        balanced_truncation_discrete_low_rank,
    };
    use crate::control::{ContinuousStateSpace, LyapunovParams, ShiftStrategy};
    use faer::sparse::{SparseColMat, Triplet};
    use faer::{Mat, c64};

    fn assert_close(lhs: &Mat<f64>, rhs: &Mat<f64>, tol: f64) {
        assert_eq!(lhs.nrows(), rhs.nrows());
        assert_eq!(lhs.ncols(), rhs.ncols());
        for col in 0..lhs.ncols() {
            for row in 0..lhs.nrows() {
                let err = (lhs[(row, col)] - rhs[(row, col)]).abs();
                assert!(
                    err <= tol,
                    "entry ({row}, {col}) mismatch: err={err}, tol={tol}"
                );
            }
        }
    }

    #[test]
    fn dense_continuous_balanced_truncation_returns_requested_internals() {
        let a = Mat::from_fn(2, 2, |row, col| match (row, col) {
            (0, 0) => -1.0,
            (1, 1) => -2.0,
            _ => 0.0,
        });
        let b = Mat::<f64>::identity(2, 2);
        let c = Mat::<f64>::identity(2, 2);
        let sys = ContinuousStateSpace::with_zero_feedthrough(a, b, c).unwrap();

        let result = balanced_truncation_continuous_dense(
            &sys,
            &BalancedParams::new().with_internals(InternalsLevel::Full),
        )
        .unwrap();

        assert_eq!(result.reduced.nstates(), 2);
        assert_eq!(result.left_projection.nrows(), 2);
        assert_eq!(result.right_projection.nrows(), 2);
        let internals = result.internals.unwrap();
        assert!(internals.dense_controllability_gramian.is_some());
        assert!(internals.dense_observability_gramian.is_some());
        assert!(internals.dense_controllability_sqrt.is_some());
        assert!(internals.dense_observability_sqrt.is_some());
    }

    #[test]
    fn dense_continuous_balanced_realization_keeps_full_order() {
        let a = Mat::from_fn(2, 2, |row, col| match (row, col) {
            (0, 0) => -1.0,
            (1, 1) => -4.0,
            _ => 0.0,
        });
        let b = Mat::<f64>::identity(2, 2);
        let c = Mat::<f64>::identity(2, 2);
        let sys = ContinuousStateSpace::with_zero_feedthrough(a, b, c).unwrap();

        let result = balanced_realization_continuous_dense(&sys).unwrap();

        assert_eq!(result.reduced.nstates(), sys.nstates());
        assert_eq!(result.reduced_order, sys.nstates());
        assert!(result.error_bound.unwrap() <= 1.0e-14);
    }

    #[test]
    fn dense_continuous_balanced_truncation_supports_order_zero() {
        let a = Mat::from_fn(2, 2, |row, col| match (row, col) {
            (0, 0) => -1.0,
            (1, 1) => -3.0,
            _ => 0.0,
        });
        let b = Mat::<f64>::identity(2, 1);
        let c = Mat::<f64>::identity(1, 2);
        let d = Mat::<f64>::from_fn(1, 1, |_, _| 2.0);
        let sys = ContinuousStateSpace::new(a, b, c, d.clone()).unwrap();

        let result =
            balanced_truncation_continuous_dense(&sys, &BalancedParams::new().with_order(0))
                .unwrap();

        assert_eq!(result.reduced.nstates(), 0);
        assert_eq!(result.reduced.b().nrows(), 0);
        assert_eq!(result.reduced.c().ncols(), 0);
        assert_close(&result.reduced.d().to_owned(), &d, 1.0e-12);
    }

    #[test]
    fn dense_discrete_balanced_truncation_runs() {
        let a = Mat::from_fn(2, 2, |row, col| match (row, col) {
            (0, 0) => 0.25,
            (1, 1) => -0.4,
            _ => 0.0,
        });
        let b = Mat::<f64>::identity(2, 2);
        let c = Mat::<f64>::identity(2, 2);
        let sys = crate::control::DiscreteStateSpace::with_zero_feedthrough(a, b, c, 0.1).unwrap();

        let result = balanced_truncation_discrete_dense(&sys, &BalancedParams::new()).unwrap();
        assert_eq!(result.reduced.sample_time(), 0.1);
        assert_eq!(result.hankel_singular_values.nrows(), 2);
    }

    #[test]
    fn dense_discrete_balanced_realization_keeps_full_order() {
        let a = Mat::from_fn(2, 2, |row, col| match (row, col) {
            (0, 0) => 0.1,
            (1, 1) => -0.25,
            _ => 0.0,
        });
        let b = Mat::<f64>::identity(2, 2);
        let c = Mat::<f64>::identity(2, 2);
        let sys = crate::control::DiscreteStateSpace::with_zero_feedthrough(a, b, c, 0.1).unwrap();

        let result = balanced_realization_discrete_dense(&sys).unwrap();

        assert_eq!(result.reduced.nstates(), sys.nstates());
        assert_eq!(result.reduced_order, sys.nstates());
        assert!(result.error_bound.unwrap() <= 1.0e-14);
    }

    #[test]
    fn sparse_continuous_low_rank_balanced_truncation_matches_dense_reference() {
        let a_sparse = SparseColMat::<usize, f64>::try_new_from_triplets(
            3,
            3,
            &[
                Triplet::new(0, 0, -2.0),
                Triplet::new(1, 0, 0.2),
                Triplet::new(0, 1, 0.5),
                Triplet::new(1, 1, -1.5),
                Triplet::new(2, 1, -0.4),
                Triplet::new(1, 2, 0.75),
                Triplet::new(2, 2, -0.8),
            ],
        )
        .unwrap();
        let b = Mat::from_fn(3, 1, |row, _| match row {
            0 => 1.0,
            1 => -0.25,
            _ => 0.5,
        });
        let c = Mat::from_fn(1, 3, |_, col| match col {
            0 => 1.0,
            1 => -0.5,
            _ => 0.25,
        });
        let d = Mat::<f64>::zeros(1, 1);
        let shifts = ShiftStrategy::user_provided(vec![-0.5, -1.0, -2.0, -4.0]);
        let gramian_params = LyapunovParams {
            tol: 1.0e-10,
            max_iters: 24,
        };

        let sparse = balanced_truncation_continuous_low_rank(
            a_sparse.as_ref(),
            b.as_ref(),
            c.as_ref(),
            d.as_ref(),
            &shifts,
            gramian_params,
            &BalancedParams::new().with_order(1),
        )
        .unwrap();

        let a_dense = a_sparse.as_ref().to_dense();
        let dense_sys =
            ContinuousStateSpace::new(a_dense, b.clone(), c.clone(), d.clone()).unwrap();
        let dense =
            balanced_truncation_continuous_dense(&dense_sys, &BalancedParams::new().with_order(1))
                .unwrap();

        assert_close(
            &sparse.reduced.a().to_owned(),
            &dense.reduced.a().to_owned(),
            1.0e-6,
        );
        assert!(
            (sparse.hankel_singular_values[0] - dense.hankel_singular_values[0]).abs() <= 1.0e-8
        );
        assert_close(
            &super::dense_mul(sparse.reduced.c(), sparse.reduced.b()),
            &super::dense_mul(dense.reduced.c(), dense.reduced.b()),
            1.0e-6,
        );
        assert!((sparse.error_bound.unwrap() - dense.error_bound.unwrap()).abs() <= 1.0e-8);
    }

    #[test]
    fn sparse_discrete_low_rank_balanced_truncation_matches_dense_reference() {
        let a_sparse = SparseColMat::<usize, f64>::try_new_from_triplets(
            3,
            3,
            &[
                Triplet::new(0, 0, 0.25),
                Triplet::new(1, 0, 0.05),
                Triplet::new(0, 1, -0.1),
                Triplet::new(1, 1, -0.4),
                Triplet::new(2, 1, 0.03),
                Triplet::new(1, 2, 0.08),
                Triplet::new(2, 2, 0.15),
            ],
        )
        .unwrap();
        let b = Mat::from_fn(3, 1, |row, _| match row {
            0 => 1.0,
            1 => -0.25,
            _ => 0.5,
        });
        let c = Mat::from_fn(1, 3, |_, col| match col {
            0 => 1.0,
            1 => -0.5,
            _ => 0.25,
        });
        let d = Mat::<f64>::zeros(1, 1);
        let shifts = ShiftStrategy::user_provided(vec![-0.25, -0.5, -1.0, -2.0]);
        let gramian_params = LyapunovParams {
            tol: 1.0e-10,
            max_iters: 24,
        };

        let sparse = balanced_truncation_discrete_low_rank(
            a_sparse.as_ref(),
            b.as_ref(),
            c.as_ref(),
            d.as_ref(),
            0.1,
            &shifts,
            gramian_params,
            &BalancedParams::new().with_order(1),
        )
        .unwrap();

        let a_dense = a_sparse.as_ref().to_dense();
        let dense_sys =
            crate::control::DiscreteStateSpace::new(a_dense, b.clone(), c.clone(), d.clone(), 0.1)
                .unwrap();
        let dense =
            balanced_truncation_discrete_dense(&dense_sys, &BalancedParams::new().with_order(1))
                .unwrap();

        assert_close(
            &sparse.reduced.a().to_owned(),
            &dense.reduced.a().to_owned(),
            1.0e-6,
        );
        assert!(
            (sparse.hankel_singular_values[0] - dense.hankel_singular_values[0]).abs() <= 1.0e-8
        );
        assert_close(
            &super::dense_mul(sparse.reduced.c(), sparse.reduced.b()),
            &super::dense_mul(dense.reduced.c(), dense.reduced.b()),
            1.0e-6,
        );
        assert!((sparse.error_bound.unwrap() - dense.error_bound.unwrap()).abs() <= 1.0e-8);
    }

    #[test]
    fn dense_balanced_truncation_handles_complex_system() {
        let a = Mat::from_fn(2, 2, |row, col| match (row, col) {
            (0, 0) => c64::new(-1.0, 0.2),
            (1, 1) => c64::new(-2.0, -0.1),
            _ => c64::new(0.0, 0.0),
        });
        let b = Mat::from_fn(2, 1, |row, _| match row {
            0 => c64::new(1.0, -0.5),
            _ => c64::new(-0.25, 0.75),
        });
        let c = Mat::from_fn(1, 2, |_, col| match col {
            0 => c64::new(1.0, 0.25),
            _ => c64::new(-0.5, 0.5),
        });
        let sys = ContinuousStateSpace::with_zero_feedthrough(a, b, c).unwrap();

        let result =
            balanced_truncation_continuous_dense(&sys, &BalancedParams::new().with_order(1))
                .unwrap();
        assert_eq!(result.reduced.nstates(), 1);
    }
}
