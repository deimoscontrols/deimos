//! Hankel singular value decomposition and balancing-core utilities.
//!
//! This module extracts the part of balanced truncation that depends only on
//! controllability/observability Gramian data:
//!
//! 1. obtain Gramian square-root factors `Rc` and `Ro`
//! 2. form the balancing core `Ro^H Rc`
//! 3. compute its SVD to obtain the Hankel singular values
//! 4. build the left/right projection operators
//!
//! The resulting projection factors can be used directly by balanced
//! truncation, but they are also useful on their own for model-order analysis
//! and future realization algorithms.
//!
//! # Two Intuitions
//!
//! 1. **Balancing-core view.** HSVD is the algebraic middle layer between raw
//!    Gramian data and a reduced model.
//! 2. **Energy-ranking view.** It tells you which state directions are jointly
//!    easy to control and easy to observe, then orders those directions by
//!    importance.
//!
//! # Glossary
//!
//! - **Hankel singular value:** Magnitude measuring joint
//!   controllability/observability importance.
//! - **Balancing core:** Matrix built from Gramian factors before the SVD.
//! - **Projection operators:** Left/right maps used to compress the state.
//!
//! # Mathematical Formulation
//!
//! Given Gramian factors `Rc` and `Ro`, the module forms the core `Ro^H Rc`,
//! computes its SVD, and uses the retained factors to assemble balanced
//! projection operators.
//!
//! # Implementation Notes
//!
//! - Dense Gramian inputs go through explicit PSD factor extraction.
//! - The same retained internals are reused directly by balanced truncation.
//! - Truncation policy is centralized in `HsvdParams` so higher layers see the
//!   same order/tolerance behavior.

use crate::control::dense_ops::{dense_mul, dense_mul_adjoint_lhs};
use crate::decomp::{DecompError, DenseDecompParams, dense_self_adjoint_eigen, dense_svd};
use crate::sparse::compensated::CompensatedField;
use alloc::vec::Vec;
use core::fmt;
use faer::{Col, Mat, MatRef};
use faer_traits::ext::ComplexFieldExt;
use num_traits::{Float, One, Zero};

/// Controls how much internal HSVD data is retained in the result.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum HsvdInternalsLevel {
    /// Return only the Hankel singular values, truncation summary, and final
    /// left/right projection operators.
    #[default]
    Summary,
    /// Also retain the Gramian factors and balancing-core SVD data.
    Factors,
    /// Also retain dense Gramian and dense square-root data when the HSVD was
    /// built from explicit dense Gramian matrices.
    Full,
}

impl HsvdInternalsLevel {
    #[inline]
    fn keep_factors(self) -> bool {
        !matches!(self, Self::Summary)
    }

    #[inline]
    fn keep_full(self) -> bool {
        matches!(self, Self::Full)
    }
}

/// Builder-style parameters for Hankel singular value decomposition.
///
/// `order` controls how many balanced modes are retained in the returned
/// projections. `sigma_tol` provides an alternate numerical cut-off based on
/// the Hankel singular values themselves. When both are present, the explicit
/// order is still honored, but the tolerance can cap how many singular
/// directions are treated as numerically available.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HsvdParams<R> {
    /// Requested retained order. `None` keeps all numerically retained modes.
    pub order: Option<usize>,
    /// Optional Hankel singular value tolerance. If omitted, a relative
    /// machine-precision-based default is used.
    pub sigma_tol: Option<R>,
    /// Requested internal detail level for the returned result.
    pub internals: HsvdInternalsLevel,
}

impl<R> Default for HsvdParams<R> {
    fn default() -> Self {
        Self {
            order: None,
            sigma_tol: None,
            internals: HsvdInternalsLevel::Summary,
        }
    }
}

impl<R> HsvdParams<R> {
    /// Creates parameters with documented defaults.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
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

    /// Chooses how much internal HSVD data to retain.
    #[must_use]
    pub fn with_internals(mut self, internals: HsvdInternalsLevel) -> Self {
        self.internals = internals;
        self
    }
}

/// Optional retained internal HSVD data.
///
/// These values expose the algebra behind the returned projections. In
/// particular, `controllability_factor`, `observability_factor`, and the core
/// SVD describe exactly how `right_projection` and `left_projection` were
/// built.
#[derive(Clone, Debug)]
pub struct HsvdInternals<T: CompensatedField>
where
    T::Real: Float,
{
    /// Controllability Gramian factor used in the balancing core.
    pub controllability_factor: Mat<T>,
    /// Observability Gramian factor used in the balancing core.
    pub observability_factor: Mat<T>,
    /// Dense balancing core `Ro^H Rc`.
    pub core: Mat<T>,
    /// Left singular vectors of the balancing core.
    pub core_u: Mat<T>,
    /// Singular values of the balancing core.
    pub core_singular_values: Col<T::Real>,
    /// Adjoint of the right singular vector matrix of the balancing core.
    pub core_vh: Mat<T>,
    /// Dense controllability Gramian, when retained.
    pub dense_controllability_gramian: Option<Mat<T>>,
    /// Dense observability Gramian, when retained.
    pub dense_observability_gramian: Option<Mat<T>>,
    /// Dense controllability square-root factor, when retained.
    pub dense_controllability_sqrt: Option<Mat<T>>,
    /// Dense observability square-root factor, when retained.
    pub dense_observability_sqrt: Option<Mat<T>>,
}

/// Result of Hankel singular value decomposition.
///
/// This is not a full system reduction by itself. It is the reusable middle
/// layer between Gramian computation and reduced-model assembly: the Hankel
/// singular values plus the projection factors that a caller such as balanced
/// truncation can apply to `A/B/C/D`.
#[derive(Clone, Debug)]
pub struct HsvdResult<T: CompensatedField>
where
    T::Real: Float,
{
    /// Hankel singular values in descending order.
    pub hankel_singular_values: Col<T::Real>,
    /// Final retained order.
    pub reduced_order: usize,
    /// Standard balanced-truncation tail bound from the discarded Hankel
    /// singular values.
    pub error_bound: Option<T::Real>,
    /// Left projection operator `S_r = Ro U_r Sigma_r^{-1/2}`.
    pub left_projection: Mat<T>,
    /// Right projection operator `T_r = Rc V_r Sigma_r^{-1/2}`.
    pub right_projection: Mat<T>,
    /// Optional retained HSVD internal data.
    pub internals: Option<HsvdInternals<T>>,
}

/// Errors produced by HSVD front-ends.
///
/// These cover only HSVD-specific failures. System-specific operations such as
/// state-space construction are intentionally left to the caller.
#[derive(Debug)]
pub enum HsvdError<R> {
    /// Dense SVD or self-adjoint eigen decomposition failed.
    Decomposition(DecompError),
    /// The controllability and observability data do not have compatible
    /// dimensions.
    DimensionMismatch {
        /// Row count in the controllability factor or Gramian.
        controllability_nrows: usize,
        /// Row count in the observability factor or Gramian.
        observability_nrows: usize,
    },
    /// The requested retained order exceeds the available core dimension.
    InvalidOrder {
        /// Requested reduced order.
        requested: usize,
        /// Largest order that can be retained from the HSVD core.
        available: usize,
    },
    /// No numerically meaningful Hankel singular values remained after
    /// thresholding.
    EmptyRetainedSpectrum,
    /// A dense Gramian expected to be PSD had a clearly negative eigenvalue.
    IndefiniteGramian {
        /// Identifies which Gramian was indefinite.
        which: &'static str,
        /// Negative eigenvalue that triggered the rejection.
        eigenvalue: R,
    },
    /// A computed projection or factor contained a non-finite entry.
    NonFiniteResult {
        /// Identifies the derived quantity with the non-finite entry.
        which: &'static str,
    },
}

impl<R: fmt::Debug> fmt::Display for HsvdError<R> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

impl<R: fmt::Debug> core::error::Error for HsvdError<R> {}

impl<R> From<DecompError> for HsvdError<R> {
    fn from(value: DecompError) -> Self {
        Self::Decomposition(value)
    }
}

#[derive(Clone, Debug)]
struct DenseFactorData<T: CompensatedField>
where
    T::Real: Float,
{
    factor: Mat<T>,
    dense_gramian: Mat<T>,
    dense_sqrt: Mat<T>,
}

#[derive(Clone, Debug)]
struct BalanceCore<T: CompensatedField>
where
    T::Real: Float,
{
    hankel_singular_values: Col<T::Real>,
    reduced_order: usize,
    error_bound: Option<T::Real>,
    left_projection: Mat<T>,
    right_projection: Mat<T>,
    core: Mat<T>,
    core_u: Mat<T>,
    core_singular_values: Col<T::Real>,
    core_vh: Mat<T>,
}

/// Computes HSVD from explicit dense controllability and observability
/// Gramians.
///
/// This path first constructs PSD square-root factors for each dense Gramian
/// and then delegates to the shared balancing-core logic.
///
/// The dense inputs are expected to be controllability and observability
/// Gramians for the same state dimension. They do not need to be strictly
/// positive definite; the HSVD path tolerates tiny negative eigenvalues caused
/// by roundoff when constructing the square-root factors.
pub fn hsvd_from_dense_gramians<T>(
    controllability_gramian: MatRef<'_, T>,
    observability_gramian: MatRef<'_, T>,
    params: &HsvdParams<T::Real>,
) -> Result<HsvdResult<T>, HsvdError<T::Real>>
where
    T: CompensatedField,
    T::Real: Float,
{
    validate_dense_gramians(controllability_gramian, observability_gramian)?;
    let wc_factor = dense_psd_factor("controllability", controllability_gramian)?;
    let wo_factor = dense_psd_factor("observability", observability_gramian)?;
    let core = build_balance_core(wc_factor.factor.as_ref(), wo_factor.factor.as_ref(), params)?;
    let internals = dense_internals(params.internals, wc_factor, wo_factor, &core);
    Ok(HsvdResult {
        hankel_singular_values: core.hankel_singular_values.clone(),
        reduced_order: core.reduced_order,
        error_bound: core.error_bound,
        left_projection: core.left_projection.clone(),
        right_projection: core.right_projection.clone(),
        internals,
    })
}

/// Computes HSVD from controllability and observability Gramian factors.
///
/// This is the natural entry point for sparse low-rank Gramian solvers, where
/// the factors already exist and forming full dense Gramians would be
/// unnecessary and expensive.
///
/// The factors are interpreted as `Wc = Rc Rc^H` and `Wo = Ro Ro^H`. Only the
/// row dimension has to agree; the factor column counts may differ.
pub fn hsvd_from_factors<T>(
    controllability_factor: MatRef<'_, T>,
    observability_factor: MatRef<'_, T>,
    params: &HsvdParams<T::Real>,
) -> Result<HsvdResult<T>, HsvdError<T::Real>>
where
    T: CompensatedField,
    T::Real: Float,
{
    validate_factor_rows(controllability_factor, observability_factor)?;
    let core = build_balance_core(controllability_factor, observability_factor, params)?;
    let internals = factor_internals(
        params.internals,
        controllability_factor.to_owned(),
        observability_factor.to_owned(),
        &core,
    );
    Ok(HsvdResult {
        hankel_singular_values: core.hankel_singular_values.clone(),
        reduced_order: core.reduced_order,
        error_bound: core.error_bound,
        left_projection: core.left_projection.clone(),
        right_projection: core.right_projection.clone(),
        internals,
    })
}

fn validate_dense_gramians<T>(
    controllability_gramian: MatRef<'_, T>,
    observability_gramian: MatRef<'_, T>,
) -> Result<(), HsvdError<T::Real>>
where
    T: CompensatedField,
    T::Real: Float,
{
    // The dense path expects square Gramians for the same underlying state
    // dimension. A mismatch here means the caller is not actually supplying a
    // controllability/observability pair for one system.
    let wc_ok = controllability_gramian.nrows() == controllability_gramian.ncols();
    let wo_ok = observability_gramian.nrows() == observability_gramian.ncols();
    let same = controllability_gramian.nrows() == observability_gramian.nrows();
    if wc_ok && wo_ok && same {
        Ok(())
    } else {
        Err(HsvdError::DimensionMismatch {
            controllability_nrows: controllability_gramian.nrows(),
            observability_nrows: observability_gramian.nrows(),
        })
    }
}

fn validate_factor_rows<T>(
    controllability_factor: MatRef<'_, T>,
    observability_factor: MatRef<'_, T>,
) -> Result<(), HsvdError<T::Real>>
where
    T: CompensatedField,
    T::Real: Float,
{
    // Factor widths may differ because controllability and observability ranks
    // are not required to match. Only the shared state dimension matters here.
    if controllability_factor.nrows() == observability_factor.nrows() {
        Ok(())
    } else {
        Err(HsvdError::DimensionMismatch {
            controllability_nrows: controllability_factor.nrows(),
            observability_nrows: observability_factor.nrows(),
        })
    }
}

fn dense_internals<T>(
    level: HsvdInternalsLevel,
    wc: DenseFactorData<T>,
    wo: DenseFactorData<T>,
    core: &BalanceCore<T>,
) -> Option<HsvdInternals<T>>
where
    T: CompensatedField,
    T::Real: Float,
{
    if !level.keep_factors() {
        return None;
    }

    // The dense path can optionally retain both the solved Gramians and the
    // PSD square-root factors built from them.
    Some(HsvdInternals {
        controllability_factor: wc.factor,
        observability_factor: wo.factor,
        core: core.core.clone(),
        core_u: core.core_u.clone(),
        core_singular_values: core.core_singular_values.clone(),
        core_vh: core.core_vh.clone(),
        dense_controllability_gramian: level.keep_full().then_some(wc.dense_gramian),
        dense_observability_gramian: level.keep_full().then_some(wo.dense_gramian),
        dense_controllability_sqrt: level.keep_full().then_some(wc.dense_sqrt),
        dense_observability_sqrt: level.keep_full().then_some(wo.dense_sqrt),
    })
}

fn factor_internals<T>(
    level: HsvdInternalsLevel,
    controllability_factor: Mat<T>,
    observability_factor: Mat<T>,
    core: &BalanceCore<T>,
) -> Option<HsvdInternals<T>>
where
    T: CompensatedField,
    T::Real: Float,
{
    if !level.keep_factors() {
        return None;
    }

    // Factor-based HSVD has no full dense Gramian to retain, so the internals
    // surface only includes the provided factors and the balancing-core data.
    Some(HsvdInternals {
        controllability_factor,
        observability_factor,
        core: core.core.clone(),
        core_u: core.core_u.clone(),
        core_singular_values: core.core_singular_values.clone(),
        core_vh: core.core_vh.clone(),
        dense_controllability_gramian: None,
        dense_observability_gramian: None,
        dense_controllability_sqrt: None,
        dense_observability_sqrt: None,
    })
}

fn dense_psd_factor<T>(
    which: &'static str,
    gramian: MatRef<'_, T>,
) -> Result<DenseFactorData<T>, HsvdError<T::Real>>
where
    T: CompensatedField,
    T::Real: Float,
{
    // HSVD only needs a Gramian square root. Using a self-adjoint
    // eigendecomposition is more robust than assuming a Cholesky factor exists.
    let eig = dense_self_adjoint_eigen(gramian, &DenseDecompParams::<T>::new())?;
    let mut max_abs = <T::Real as Zero>::zero();
    for i in 0..eig.values.nrows() {
        let abs = eig.values[i].abs();
        if abs > max_abs {
            max_abs = abs;
        }
    }
    let eig_tol = <T::Real as Float>::epsilon().sqrt() * max_abs;
    let mut kept = Vec::new();
    for i in 0..eig.values.nrows() {
        let lambda = eig.values[i].real();
        // Tiny negative eigenvalues are treated as numerical noise. A clearly
        // negative value means the input is not PSD enough to support a valid
        // Hankel singular value decomposition.
        if lambda < -eig_tol {
            return Err(HsvdError::IndefiniteGramian {
                which,
                eigenvalue: lambda,
            });
        }
        if lambda > eig_tol {
            kept.push((i, lambda.sqrt()));
        }
    }

    let factor = Mat::from_fn(gramian.nrows(), kept.len(), |row, col| {
        eig.vectors[(row, kept[col].0)].mul_real(kept[col].1)
    });
    if !factor.as_ref().is_all_finite() {
        return Err(HsvdError::NonFiniteResult { which });
    }

    Ok(DenseFactorData {
        factor: factor.clone(),
        dense_gramian: gramian.to_owned(),
        dense_sqrt: factor,
    })
}

fn build_balance_core<T>(
    controllability_factor: MatRef<'_, T>,
    observability_factor: MatRef<'_, T>,
    params: &HsvdParams<T::Real>,
) -> Result<BalanceCore<T>, HsvdError<T::Real>>
where
    T: CompensatedField,
    T::Real: Float,
{
    // The balancing core remains dense and small even for sparse systems,
    // because it is formed from Gramian factors rather than full Gramians.
    let core = dense_mul_adjoint_lhs(observability_factor, controllability_factor);
    let svd = dense_svd(core.as_ref(), &DenseDecompParams::<T>::new())?;
    let hankel_singular_values = Col::from_fn(svd.s.nrows(), |i| svd.s[i].abs());

    let mut max_sigma = <T::Real as Zero>::zero();
    for i in 0..hankel_singular_values.nrows() {
        let sigma = hankel_singular_values[i];
        if sigma > max_sigma {
            max_sigma = sigma;
        }
    }
    let sigma_tol = params
        .sigma_tol
        .unwrap_or_else(|| <T::Real as Float>::epsilon().sqrt() * max_sigma);
    // `available` counts the singular directions that still look numerically
    // meaningful after the retention threshold is applied.
    let available = (0..hankel_singular_values.nrows())
        .take_while(|&i| hankel_singular_values[i] > sigma_tol)
        .count();

    let reduced_order = match params.order {
        Some(order) if order > hankel_singular_values.nrows() => {
            return Err(HsvdError::InvalidOrder {
                requested: order,
                available: hankel_singular_values.nrows(),
            });
        }
        Some(order) if params.sigma_tol.is_some() => order.min(available),
        Some(order) if order > available => {
            return Err(HsvdError::InvalidOrder {
                requested: order,
                available,
            });
        }
        Some(order) => order,
        None => available,
    };

    if reduced_order == 0 && params.order != Some(0) {
        return Err(HsvdError::EmptyRetainedSpectrum);
    }

    let error_bound = Some(
        (<T::Real as One>::one() + <T::Real as One>::one())
            * tail_sum(hankel_singular_values.as_ref(), reduced_order),
    );

    // These are the square-root balanced truncation projection formulas:
    //
    // T_r = Rc V_r Sigma_r^{-1/2}
    // S_r = Ro U_r Sigma_r^{-1/2}
    let right_projection = build_projection(
        controllability_factor,
        svd.v.as_ref(),
        hankel_singular_values.as_ref(),
        reduced_order,
    );
    let left_projection = build_projection(
        observability_factor,
        svd.u.as_ref(),
        hankel_singular_values.as_ref(),
        reduced_order,
    );
    if !right_projection.as_ref().is_all_finite() {
        return Err(HsvdError::NonFiniteResult {
            which: "right_projection",
        });
    }
    if !left_projection.as_ref().is_all_finite() {
        return Err(HsvdError::NonFiniteResult {
            which: "left_projection",
        });
    }

    Ok(BalanceCore {
        hankel_singular_values: hankel_singular_values.clone(),
        reduced_order,
        error_bound,
        left_projection,
        right_projection,
        core,
        core_u: svd.u,
        core_singular_values: hankel_singular_values,
        core_vh: svd.v.adjoint().to_owned(),
    })
}

fn build_projection<T>(
    factor: MatRef<'_, T>,
    basis: MatRef<'_, T>,
    hankel_singular_values: faer::ColRef<'_, T::Real>,
    order: usize,
) -> Mat<T>
where
    T: CompensatedField,
    T::Real: Float,
{
    let scaled_basis = Mat::from_fn(basis.nrows(), order, |row, col| {
        let sigma_inv_sqrt = hankel_singular_values[col].sqrt().recip();
        basis[(row, col)].mul_real(sigma_inv_sqrt)
    });
    // The final multiplication by the Gramian factor turns the retained core
    // singular vectors into state-space projections.
    dense_mul(factor, scaled_basis.as_ref())
}

fn tail_sum<R>(values: faer::ColRef<'_, R>, from: usize) -> R
where
    R: Float,
{
    let mut sum = <R as Zero>::zero();
    for i in from..values.nrows() {
        sum = sum + values[i];
    }
    sum
}

#[cfg(test)]
mod tests {
    use super::{
        HsvdError, HsvdInternalsLevel, HsvdParams, hsvd_from_dense_gramians, hsvd_from_factors,
    };
    use faer::Mat;

    fn assert_close(lhs: &Mat<f64>, rhs: &Mat<f64>, tol: f64) {
        assert_eq!(lhs.nrows(), rhs.nrows());
        assert_eq!(lhs.ncols(), rhs.ncols());
        for col in 0..lhs.ncols() {
            for row in 0..lhs.nrows() {
                let err = (lhs[(row, col)] - rhs[(row, col)]).abs();
                assert!(err <= tol, "entry ({row}, {col}) mismatch: {err} > {tol}");
            }
        }
    }

    #[test]
    fn dense_hsvd_matches_diagonal_gramian_case() {
        let wc = Mat::from_fn(2, 2, |row, col| match (row, col) {
            (0, 0) => 4.0,
            (1, 1) => 1.0,
            _ => 0.0,
        });
        let wo = Mat::from_fn(2, 2, |row, col| match (row, col) {
            (0, 0) => 9.0,
            (1, 1) => 16.0,
            _ => 0.0,
        });

        let hsvd = hsvd_from_dense_gramians(
            wc.as_ref(),
            wo.as_ref(),
            &HsvdParams::new().with_internals(HsvdInternalsLevel::Full),
        )
        .unwrap();

        assert_eq!(hsvd.reduced_order, 2);
        let sigma0: f64 = hsvd.hankel_singular_values[0];
        let sigma1: f64 = hsvd.hankel_singular_values[1];
        assert!((sigma0 - 6.0_f64).abs() <= 1.0e-12_f64);
        assert!((sigma1 - 4.0_f64).abs() <= 1.0e-12_f64);
        let internals = hsvd.internals.unwrap();
        assert!(internals.dense_controllability_gramian.is_some());
        assert!(internals.dense_observability_gramian.is_some());
    }

    #[test]
    fn factor_hsvd_matches_dense_hsvd_for_same_factors() {
        let rc = Mat::from_fn(3, 2, |row, col| match (row, col) {
            (0, 0) => 2.0,
            (1, 1) => 1.0,
            (2, 0) => 0.5,
            _ => 0.0,
        });
        let ro = Mat::from_fn(3, 2, |row, col| match (row, col) {
            (0, 0) => 1.5,
            (1, 1) => 3.0,
            (2, 1) => -0.25,
            _ => 0.0,
        });
        let wc = super::dense_mul(rc.as_ref(), rc.adjoint().to_owned().as_ref());
        let wo = super::dense_mul(ro.as_ref(), ro.adjoint().to_owned().as_ref());

        let dense = hsvd_from_dense_gramians(wc.as_ref(), wo.as_ref(), &HsvdParams::new()).unwrap();
        let factor = hsvd_from_factors(rc.as_ref(), ro.as_ref(), &HsvdParams::new()).unwrap();

        assert_eq!(dense.reduced_order, factor.reduced_order);
        for i in 0..dense.hankel_singular_values.nrows() {
            let dense_sigma: f64 = dense.hankel_singular_values[i];
            let factor_sigma: f64 = factor.hankel_singular_values[i];
            assert!((dense_sigma - factor_sigma).abs() <= 1.0e-10);
        }

        let dense_identity = super::dense_mul_adjoint_lhs(
            dense.left_projection.as_ref(),
            dense.right_projection.as_ref(),
        );
        let factor_identity = super::dense_mul_adjoint_lhs(
            factor.left_projection.as_ref(),
            factor.right_projection.as_ref(),
        );
        let expected_identity = Mat::<f64>::identity(dense.reduced_order, dense.reduced_order);
        assert_close(
            &dense_identity.as_ref().to_owned(),
            &expected_identity,
            1.0e-10,
        );
        assert_close(
            &factor_identity.as_ref().to_owned(),
            &expected_identity,
            1.0e-10,
        );
    }

    #[test]
    fn rejects_invalid_order() {
        let rc = Mat::from_fn(2, 1, |row, _| if row == 0 { 1.0 } else { 0.5 });
        let ro = Mat::from_fn(2, 1, |row, _| if row == 0 { 1.0 } else { 0.25 });
        let err = hsvd_from_factors(rc.as_ref(), ro.as_ref(), &HsvdParams::new().with_order(2))
            .unwrap_err();
        assert!(matches!(
            err,
            HsvdError::InvalidOrder {
                requested: 2,
                available: 1
            }
        ));
    }
}
