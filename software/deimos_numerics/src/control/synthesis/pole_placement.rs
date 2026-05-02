//! Dense classical pole-placement utilities.
//!
//! The implementation is dense and real-valued:
//!
//! - SISO state feedback uses the Ackermann construction
//! - MIMO state feedback uses an iterative eigenvector-selection path for real
//!   desired poles
//! - observer placement is always formed by duality on `(A^T, C^T)`
//!
//! The two public entry-point families solve different problems:
//!
//! - `place_poles*` designs a state-feedback gain `K` for `u = -K x` and
//!   places the poles of `A - B K`
//! - `place_observer_poles*` designs an observer gain `L` for the estimator
//!   error dynamics and places the poles of `A - L C`
//!
//! The MIMO path is intentionally limited: it is useful for dense
//! real systems and real desired poles, but it is not yet a full general
//! eigenstructure-assignment package.
//!
//! # Two Intuitions
//!
//! 1. **Spectral-shaping view.** Pole placement directly asks for the desired
//!    closed-loop or estimator eigenvalues instead of solving an optimization
//!    problem.
//! 2. **Controllability view.** In the SISO case the implementation reduces
//!    that request to controllability algebra through Ackermann's formula; in
//!    the dense MIMO case it extends the idea to a dense real assignment
//!    workflow.
//!
//! # Glossary
//!
//! - **Ackermann formula:** Closed-form SISO state-feedback pole placement.
//! - **Observer placement:** Pole placement on the dual pair `(A^T, C^T)`.
//! - **Closed-loop matrix:** `A - B K` or `A - L C`.
//!
//! # Mathematical Formulation
//!
//! The controller side chooses `K` so that the eigenvalues of `A - B K` match
//! a requested set. The observer side chooses `L` so that the eigenvalues of
//! `A - L C` match a requested set.
//!
//! # Implementation Notes
//!
//! - SISO state-feedback placement is Ackermann-based.
//! - Observer placement is implemented by duality.
//! - The current MIMO path is dense real and still conservative: complex MIMO
//!   targets are intentionally deferred.

use crate::control::dense_ops::dense_mul;
use crate::control::lti::{ContinuousStateSpace, DiscreteStateSpace};
use crate::decomp::{
    DecompError, DenseDecompParams, dense_eigenvalues, dense_self_adjoint_eigen, dense_svd,
};
use crate::sparse::compensated::{CompensatedField, CompensatedSum};
use alloc::vec::Vec;
use core::fmt;
use faer::complex::Complex;
use faer::prelude::Solve;
use faer::{Mat, MatRef};
use faer_traits::RealField;
use faer_traits::ext::ComplexFieldExt;
use faer_traits::math_utils::{eps, from_f64};
use num_traits::Float;

/// Result of a dense pole-placement solve.
///
/// `gain` is the state-feedback or observer gain, depending on the entry
/// point. `placed_matrix` is the resulting closed-loop matrix:
///
/// - state feedback: `A - B K`
/// - observer design: `A - L C`
#[derive(Clone, Debug)]
pub struct PolePlacementSolve<T: CompensatedField>
where
    T::Real: Float,
{
    /// State-feedback or observer gain.
    pub gain: Mat<T>,
    /// Closed-loop matrix with the requested poles.
    pub placed_matrix: Mat<T>,
    /// Pole locations requested by the caller.
    pub requested_poles: Vec<Complex<T::Real>>,
    /// Pole locations achieved by the returned gain.
    pub achieved_poles: Vec<Complex<T::Real>>,
    /// Maximum absolute pole-placement mismatch after deterministic sorting.
    pub placement_residual: T::Real,
}

/// Errors produced by dense pole-placement utilities.
#[derive(Debug)]
pub enum PolePlacementError {
    /// The state matrix is not square.
    NonSquareA {
        /// Actual row count in `A`.
        nrows: usize,
        /// Actual column count in `A`.
        ncols: usize,
    },
    /// A supplied matrix had incompatible dimensions.
    DimensionMismatch {
        /// Identifies the matrix that failed validation.
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
    /// The desired pole list length did not match the system order.
    PoleCountMismatch {
        /// Number of poles required to match the system order.
        expected: usize,
        /// Number of poles supplied by the caller.
        actual: usize,
    },
    /// The desired pole list contained a non-finite entry.
    NonFiniteDesiredPoles,
    /// The requested poles do not define a real monic polynomial.
    NonConjugatePoleSet,
    /// The first dense MIMO implementation only supports real desired poles.
    ComplexMimoPolesUnsupported,
    /// The pair `(A, B)` was not numerically controllable.
    Uncontrollable,
    /// The pair `(A, C)` was not numerically observable.
    Unobservable,
    /// A decomposition helper failed.
    Decomp(DecompError),
    /// A solve or intermediate matrix polynomial produced non-finite output.
    NonFiniteResult {
        /// Identifies the solve or polynomial evaluation that failed.
        which: &'static str,
    },
}

impl fmt::Display for PolePlacementError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

impl core::error::Error for PolePlacementError {}

impl From<DecompError> for PolePlacementError {
    fn from(value: DecompError) -> Self {
        Self::Decomp(value)
    }
}

/// Places the poles of a dense real continuous-time pair `(A, B)`.
///
/// The returned gain `K` uses the convention `u = -K x`, so the placed matrix
/// is `A - B K`.
///
/// This is the controller-design path. Use [`place_observer_poles_dense`] when
/// you instead want to shape the estimator dynamics `A - L C`.
///
/// The current dense implementation uses:
///
/// - Ackermann for SISO systems
/// - an iterative real-pole MIMO path for `m > 1`
pub fn place_poles_dense<T>(
    a: MatRef<'_, T>,
    b: MatRef<'_, T>,
    desired_poles: &[Complex<T::Real>],
) -> Result<PolePlacementSolve<T>, PolePlacementError>
where
    T: CompensatedField + RealField,
    T::Real: Float + RealField,
{
    place_state_feedback_impl(a, b, desired_poles)
}

/// Places the poles of a dense real discrete-time pair `(A, B)`.
///
/// As in the continuous-time case, this designs a state-feedback gain `K`
/// rather than an observer gain `L`.
pub fn dplace_poles_dense<T>(
    a: MatRef<'_, T>,
    b: MatRef<'_, T>,
    desired_poles: &[Complex<T::Real>],
) -> Result<PolePlacementSolve<T>, PolePlacementError>
where
    T: CompensatedField + RealField,
    T::Real: Float + RealField,
{
    place_state_feedback_impl(a, b, desired_poles)
}

/// Places the poles of a dense real continuous-time full-order observer.
///
/// This is implemented by duality: pole placement is performed on
/// `(A^T, C^T)`, and the resulting gain is transposed back to form `L`.
///
/// This is the estimator-design path. Use [`place_poles_dense`] when you
/// instead want state-feedback poles for `A - B K`.
pub fn place_observer_poles_dense<T>(
    a: MatRef<'_, T>,
    c: MatRef<'_, T>,
    desired_poles: &[Complex<T::Real>],
) -> Result<PolePlacementSolve<T>, PolePlacementError>
where
    T: CompensatedField + RealField,
    T::Real: Float + RealField,
{
    place_observer_impl(a, c, desired_poles)
}

/// Places the poles of a dense real discrete-time full-order observer.
///
/// The observer gain `L` is returned for the estimator matrix `A - L C`.
///
/// This shapes the estimator error dynamics, not the plant closed-loop matrix
/// `A - B K`.
pub fn dplace_observer_poles_dense<T>(
    a: MatRef<'_, T>,
    c: MatRef<'_, T>,
    desired_poles: &[Complex<T::Real>],
) -> Result<PolePlacementSolve<T>, PolePlacementError>
where
    T: CompensatedField + RealField,
    T::Real: Float + RealField,
{
    place_observer_impl(a, c, desired_poles)
}

impl<T> ContinuousStateSpace<T>
where
    T: CompensatedField + RealField,
    T::Real: Float + RealField,
{
    /// Places the poles of the dense real continuous-time system.
    ///
    /// This returns the state-feedback gain `K` for `u = -K x`, so the placed
    /// matrix is `A - B K`.
    pub fn place_poles(
        &self,
        desired_poles: &[Complex<T::Real>],
    ) -> Result<PolePlacementSolve<T>, PolePlacementError> {
        place_poles_dense(self.a(), self.b(), desired_poles)
    }

    /// Places the poles of the dense real continuous-time observer
    /// `A - L C`.
    ///
    /// This returns the observer gain `L` for the estimator error dynamics,
    /// not a controller gain `K`.
    pub fn place_observer_poles(
        &self,
        desired_poles: &[Complex<T::Real>],
    ) -> Result<PolePlacementSolve<T>, PolePlacementError> {
        place_observer_poles_dense(self.a(), self.c(), desired_poles)
    }
}

impl<T> DiscreteStateSpace<T>
where
    T: CompensatedField + RealField,
    T::Real: Float + RealField,
{
    /// Places the poles of the dense real discrete-time system.
    ///
    /// This returns the state-feedback gain `K` for `u[k] = -K x[k]`.
    pub fn place_poles(
        &self,
        desired_poles: &[Complex<T::Real>],
    ) -> Result<PolePlacementSolve<T>, PolePlacementError> {
        dplace_poles_dense(self.a(), self.b(), desired_poles)
    }

    /// Places the poles of the dense real discrete-time observer `A - L C`.
    ///
    /// This returns the observer gain `L` used in the discrete estimator
    /// update, not a state-feedback gain.
    pub fn place_observer_poles(
        &self,
        desired_poles: &[Complex<T::Real>],
    ) -> Result<PolePlacementSolve<T>, PolePlacementError> {
        dplace_observer_poles_dense(self.a(), self.c(), desired_poles)
    }
}

fn place_state_feedback_impl<T>(
    a: MatRef<'_, T>,
    b: MatRef<'_, T>,
    desired_poles: &[Complex<T::Real>],
) -> Result<PolePlacementSolve<T>, PolePlacementError>
where
    T: CompensatedField + RealField,
    T::Real: Float + RealField,
{
    validate_state_feedback_dims(a, b, desired_poles.len())?;

    let ctrb = controllability_matrix(a, b);
    if numerical_rank(ctrb.as_ref())? != a.nrows() {
        return Err(PolePlacementError::Uncontrollable);
    }

    if b.ncols() == 1 {
        return place_state_feedback_siso_ackermann(a, b, desired_poles);
    }

    if desired_poles
        .iter()
        .any(|pole| pole.im.abs() > pole_tol::<T>(pole.re))
    {
        return Err(PolePlacementError::ComplexMimoPolesUnsupported);
    }

    let desired_real = desired_poles.iter().map(|pole| pole.re).collect::<Vec<_>>();
    place_state_feedback_mimo_real(a, b, &desired_real, desired_poles)
}

fn place_observer_impl<T>(
    a: MatRef<'_, T>,
    c: MatRef<'_, T>,
    desired_poles: &[Complex<T::Real>],
) -> Result<PolePlacementSolve<T>, PolePlacementError>
where
    T: CompensatedField + RealField,
    T::Real: Float + RealField,
{
    validate_observer_dims(a, c, desired_poles.len())?;

    // Observer placement is the dual state-feedback problem on `(A^T, C^T)`.
    let dual = place_state_feedback_impl(
        a.transpose().to_owned().as_ref(),
        c.transpose().to_owned().as_ref(),
        desired_poles,
    )
    .map_err(|err| match err {
        PolePlacementError::Uncontrollable => PolePlacementError::Unobservable,
        other => other,
    })?;
    // Transposing the dual state-feedback gain turns it back into the
    // observer injection gain `L`.
    let gain = dual.gain.transpose().to_owned();
    let lc = dense_mul(gain.as_ref(), c);
    let placed = a - &lc;

    Ok(PolePlacementSolve {
        gain,
        placed_matrix: placed,
        requested_poles: dual.requested_poles,
        achieved_poles: dual.achieved_poles,
        placement_residual: dual.placement_residual,
    })
}

fn place_state_feedback_siso_ackermann<T>(
    a: MatRef<'_, T>,
    b: MatRef<'_, T>,
    desired_poles: &[Complex<T::Real>],
) -> Result<PolePlacementSolve<T>, PolePlacementError>
where
    T: CompensatedField + RealField,
    T::Real: Float + RealField,
{
    let coeffs = real_monic_polynomial_from_roots(desired_poles)?;
    let ctrb = controllability_matrix(a, b);
    let phi_a = evaluate_real_monic_polynomial_at_matrix(a, &coeffs)?;
    let solution = ctrb.full_piv_lu().solve(phi_a.as_ref());
    if !solution.as_ref().is_all_finite() {
        return Err(PolePlacementError::Uncontrollable);
    }

    let gain = Mat::from_fn(1, a.nrows(), |_, col| solution[(a.nrows() - 1, col)]);
    let bk = dense_mul(b, gain.as_ref());
    let placed = a - &bk;
    let (achieved_poles, placement_residual) =
        achieved_pole_diagnostics(placed.as_ref(), desired_poles)?;

    Ok(PolePlacementSolve {
        gain,
        placed_matrix: placed,
        requested_poles: desired_poles.to_vec(),
        achieved_poles,
        placement_residual,
    })
}

fn place_state_feedback_mimo_real<T>(
    a: MatRef<'_, T>,
    b: MatRef<'_, T>,
    desired_poles_real: &[T::Real],
    desired_poles: &[Complex<T::Real>],
) -> Result<PolePlacementSolve<T>, PolePlacementError>
where
    T: CompensatedField + RealField,
    T::Real: Float + RealField,
{
    let n = a.nrows();
    let m = b.ncols();
    let mut x_bases = Vec::with_capacity(n);
    let mut g_bases = Vec::with_capacity(n);
    let mut x_cols = Vec::with_capacity(n);
    let mut g_cols = Vec::with_capacity(n);

    for &pole in desired_poles_real {
        // Build a basis for the nullspace of `[A - lambda I, -B]`. Any vector
        // in that nullspace encodes a candidate eigenvector / input-direction
        // pair satisfying `(A - lambda I) x = B g`.
        let basis = nullspace_basis(a, b, pole)?;
        let x_basis = Mat::from_fn(n, m, |row, col| basis[(row, col)]);
        let g_basis = Mat::from_fn(m, m, |row, col| basis[(n + row, col)]);

        let mut best_idx = 0usize;
        let mut best_norm = T::Real::zero();
        for col in 0..m {
            let norm = vector_norm_sq(x_basis.as_ref(), col).sqrt();
            if norm > best_norm {
                best_norm = norm;
                best_idx = col;
            }
        }
        if best_norm <= pole_tol::<T>(pole) {
            return Err(PolePlacementError::Uncontrollable);
        }

        let x_col = normalize_column(x_basis.as_ref(), best_idx);
        let g_col = scale_column(g_basis.as_ref(), best_idx, T::Real::one() / best_norm);
        x_bases.push(x_basis);
        g_bases.push(g_basis);
        x_cols.push(x_col);
        g_cols.push(g_col);
    }

    for _ in 0..8 {
        for idx in 0..n {
            // KNV-style refinement: choose the current eigenvector inside its
            // admissible subspace so it is as orthogonal as possible to the
            // other assigned eigenvectors.
            let q = complement_direction(&x_cols, idx)?;
            let coeffs = Mat::from_fn(m, 1, |row, _| {
                let mut acc = CompensatedSum::<T>::default();
                for k in 0..n {
                    acc.add(x_bases[idx][(k, row)] * q[(k, 0)]);
                }
                acc.finish()
            });
            let coeff_norm = vector_norm_sq(coeffs.as_ref(), 0).sqrt();
            if coeff_norm <= pole_tol::<T>(desired_poles_real[idx]) {
                continue;
            }
            let x = dense_mul(x_bases[idx].as_ref(), coeffs.as_ref());
            let x_norm = vector_norm_sq(x.as_ref(), 0).sqrt();
            if x_norm <= pole_tol::<T>(desired_poles_real[idx]) {
                continue;
            }
            x_cols[idx] = scale_matrix(x.as_ref(), T::Real::one() / x_norm);
            let g = dense_mul(g_bases[idx].as_ref(), coeffs.as_ref());
            g_cols[idx] = scale_matrix(g.as_ref(), T::Real::one() / x_norm);
        }
    }

    let x = assemble_columns(&x_cols);
    if numerical_rank(x.as_ref())? != n {
        return Err(PolePlacementError::Uncontrollable);
    }
    let g = assemble_columns(&g_cols);
    let gain_t = x
        .full_piv_lu()
        .solve(g.as_ref().transpose().to_owned().as_ref());
    let gain = gain_t.transpose().to_owned();
    if !gain.as_ref().is_all_finite() {
        return Err(PolePlacementError::Uncontrollable);
    }

    let bk = dense_mul(b, gain.as_ref());
    let placed = a - &bk;
    let (achieved_poles, placement_residual) =
        achieved_pole_diagnostics(placed.as_ref(), desired_poles)?;

    Ok(PolePlacementSolve {
        gain,
        placed_matrix: placed,
        requested_poles: desired_poles.to_vec(),
        achieved_poles,
        placement_residual,
    })
}

fn validate_state_feedback_dims<T>(
    a: MatRef<'_, T>,
    b: MatRef<'_, T>,
    n_poles: usize,
) -> Result<(), PolePlacementError> {
    if a.nrows() != a.ncols() {
        return Err(PolePlacementError::NonSquareA {
            nrows: a.nrows(),
            ncols: a.ncols(),
        });
    }
    if b.nrows() != a.nrows() {
        return Err(PolePlacementError::DimensionMismatch {
            which: "b",
            expected_nrows: a.nrows(),
            expected_ncols: b.ncols(),
            actual_nrows: b.nrows(),
            actual_ncols: b.ncols(),
        });
    }
    if n_poles != a.nrows() {
        return Err(PolePlacementError::PoleCountMismatch {
            expected: a.nrows(),
            actual: n_poles,
        });
    }
    Ok(())
}

fn validate_observer_dims<T>(
    a: MatRef<'_, T>,
    c: MatRef<'_, T>,
    n_poles: usize,
) -> Result<(), PolePlacementError> {
    if a.nrows() != a.ncols() {
        return Err(PolePlacementError::NonSquareA {
            nrows: a.nrows(),
            ncols: a.ncols(),
        });
    }
    if c.ncols() != a.nrows() {
        return Err(PolePlacementError::DimensionMismatch {
            which: "c",
            expected_nrows: c.nrows(),
            expected_ncols: a.nrows(),
            actual_nrows: c.nrows(),
            actual_ncols: c.ncols(),
        });
    }
    if n_poles != a.nrows() {
        return Err(PolePlacementError::PoleCountMismatch {
            expected: a.nrows(),
            actual: n_poles,
        });
    }
    Ok(())
}

fn controllability_matrix<T>(a: MatRef<'_, T>, b: MatRef<'_, T>) -> Mat<T>
where
    T: CompensatedField + RealField,
    T::Real: Float,
{
    let n = a.nrows();
    let m = b.ncols();
    let mut out = Mat::zeros(n, n * m);
    let mut block = Mat::from_fn(n, m, |row, col| b[(row, col)]);
    for k in 0..n {
        for row in 0..n {
            for col in 0..m {
                out[(row, k * m + col)] = block[(row, col)];
            }
        }
        if k + 1 != n {
            block = dense_mul(a, block.as_ref());
        }
    }
    out
}

fn nullspace_basis<T>(
    a: MatRef<'_, T>,
    b: MatRef<'_, T>,
    pole: T::Real,
) -> Result<Mat<T>, PolePlacementError>
where
    T: CompensatedField + RealField,
    T::Real: Float + RealField,
{
    let n = a.nrows();
    let m = b.ncols();
    let system = Mat::from_fn(n, n + m, |row, col| {
        if col < n {
            let diag = if row == col {
                T::from_real_imag(pole, T::Real::zero())
            } else {
                T::zero()
            };
            a[(row, col)] - diag
        } else {
            -b[(row, col - n)]
        }
    });
    // Extract the trailing nullspace basis from the Gram matrix eigenvectors.
    // This keeps the implementation inside the existing dense decomposition
    // surface instead of depending on a separate nullspace routine.
    let gram = gram_matrix(system.as_ref());
    let eig = dense_self_adjoint_eigen(gram.as_ref(), &DenseDecompParams::default())?;
    let tol = eig.values[0].abs().max(T::Real::one())
        * from_f64::<T::Real>(256.0)
        * eps::<T::Real>().sqrt();
    let mut basis = Mat::zeros(n + m, m);
    for j in 0..m {
        let src = eig.vectors.ncols() - m + j;
        if eig.values[src].abs() > tol {
            return Err(PolePlacementError::Uncontrollable);
        }
        for row in 0..(n + m) {
            basis[(row, j)] = eig.vectors[(row, src)];
        }
    }
    Ok(basis)
}

fn complement_direction<T>(
    columns: &[Mat<T>],
    skip_idx: usize,
) -> Result<Mat<T>, PolePlacementError>
where
    T: CompensatedField + RealField,
    T::Real: Float + RealField,
{
    let n = columns[0].nrows();
    let k = columns.len().saturating_sub(1);
    if k == 0 {
        return Ok(Mat::from_fn(n, 1, |row, _| {
            if row == 0 { T::one() } else { T::zero() }
        }));
    }
    let others = Mat::from_fn(n, k, |row, col| {
        let src = if col < skip_idx { col } else { col + 1 };
        columns[src][(row, 0)]
    });
    // Use the eigenvector associated with the smallest Gram eigenvalue as a
    // direction in the orthogonal complement of the other columns.
    let gram = dense_mul(others.as_ref(), others.transpose().to_owned().as_ref());
    let eig = dense_self_adjoint_eigen(gram.as_ref(), &DenseDecompParams::default())?;
    let last = eig.vectors.ncols() - 1;
    Ok(Mat::from_fn(n, 1, |row, _| eig.vectors[(row, last)]))
}

fn numerical_rank<T>(matrix: MatRef<'_, T>) -> Result<usize, PolePlacementError>
where
    T: CompensatedField + RealField,
    T::Real: Float,
{
    let svd = dense_svd(matrix, &DenseDecompParams::default())?;
    if svd.s.nrows() == 0 {
        return Ok(0);
    }
    let sigma_max = svd.s[0].abs();
    let dim_scale = from_f64::<T::Real>(matrix.nrows().max(matrix.ncols()) as f64);
    let tol = sigma_max * dim_scale * eps::<T::Real>().sqrt();
    Ok(svd.s.iter().filter(|&&sigma| sigma.abs() > tol).count())
}

fn real_monic_polynomial_from_roots<T>(
    roots: &[Complex<T::Real>],
) -> Result<Vec<T>, PolePlacementError>
where
    T: CompensatedField + RealField,
    T::Real: Float,
{
    let mut coeffs = vec![Complex::<T::Real>::new(T::Real::one(), T::Real::zero())];
    for &root in roots {
        if !root.re.is_finite() || !root.im.is_finite() {
            return Err(PolePlacementError::NonFiniteDesiredPoles);
        }
        // Multiply the current monic polynomial by `(s - root)` in descending
        // coefficient order.
        let mut next =
            vec![Complex::<T::Real>::new(T::Real::zero(), T::Real::zero()); coeffs.len() + 1];
        for (i, coeff) in coeffs.iter().enumerate() {
            next[i] += *coeff;
            next[i + 1] -= *coeff * root;
        }
        coeffs = next;
    }

    let scale = coeffs
        .iter()
        .map(|coeff| coeff.abs())
        .fold(T::Real::one(), |acc, value| acc.max(value));
    let tol = scale * from_f64::<T::Real>(128.0) * eps::<T::Real>().sqrt();
    if coeffs.iter().any(|coeff| coeff.im.abs() > tol) {
        return Err(PolePlacementError::NonConjugatePoleSet);
    }
    Ok(coeffs.into_iter().map(|coeff| coeff.re).collect())
}

fn evaluate_real_monic_polynomial_at_matrix<T>(
    a: MatRef<'_, T>,
    coeffs: &[T],
) -> Result<Mat<T>, PolePlacementError>
where
    T: CompensatedField + RealField,
    T::Real: Float,
{
    let n = a.nrows();
    let mut out = Mat::from_fn(
        n,
        n,
        |row, col| {
            if row == col { coeffs[0] } else { T::zero() }
        },
    );
    for &coeff in &coeffs[1..] {
        // Horner evaluation avoids explicitly building powers of `A`.
        let prod = dense_mul(a, out.as_ref());
        out = Mat::from_fn(n, n, |row, col| {
            if row == col {
                prod[(row, col)] + coeff
            } else {
                prod[(row, col)]
            }
        });
    }
    if out.as_ref().is_all_finite() {
        Ok(out)
    } else {
        Err(PolePlacementError::NonFiniteResult {
            which: "matrix_polynomial",
        })
    }
}

fn achieved_pole_diagnostics<T>(
    placed: MatRef<'_, T>,
    requested_poles: &[Complex<T::Real>],
) -> Result<(Vec<Complex<T::Real>>, T::Real), PolePlacementError>
where
    T: CompensatedField + RealField,
    T::Real: Float,
{
    let mut achieved = dense_eigenvalues(placed)?
        .try_as_col_major()
        .unwrap()
        .as_slice()
        .to_vec();
    let mut requested = requested_poles.to_vec();
    // Sort both spectra with the crate's deterministic convention before
    // comparing them so the residual is stable across platforms.
    achieved.sort_by(|lhs, rhs| compare_poles(*lhs, *rhs));
    requested.sort_by(|lhs, rhs| compare_poles(*lhs, *rhs));
    let residual = achieved
        .iter()
        .zip(requested.iter())
        .fold(T::Real::zero(), |acc, (lhs, rhs)| {
            acc.max((*lhs - *rhs).abs())
        });
    Ok((achieved, residual))
}

fn compare_poles<R: Float>(lhs: Complex<R>, rhs: Complex<R>) -> core::cmp::Ordering {
    let rhs_abs2 = rhs.re * rhs.re + rhs.im * rhs.im;
    let lhs_abs2 = lhs.re * lhs.re + lhs.im * lhs.im;
    rhs_abs2
        .partial_cmp(&lhs_abs2)
        .unwrap_or(core::cmp::Ordering::Equal)
        .then_with(|| {
            rhs.re
                .partial_cmp(&lhs.re)
                .unwrap_or(core::cmp::Ordering::Equal)
        })
        .then_with(|| {
            rhs.im
                .partial_cmp(&lhs.im)
                .unwrap_or(core::cmp::Ordering::Equal)
        })
}

fn gram_matrix<T>(matrix: MatRef<'_, T>) -> Mat<T>
where
    T: CompensatedField + RealField,
    T::Real: Float,
{
    let cols = matrix.ncols();
    let rows = matrix.nrows();
    Mat::from_fn(cols, cols, |i, j| {
        let mut acc = CompensatedSum::<T>::default();
        for k in 0..rows {
            acc.add(matrix[(k, i)] * matrix[(k, j)]);
        }
        acc.finish()
    })
}

fn vector_norm_sq<T>(matrix: MatRef<'_, T>, col: usize) -> T::Real
where
    T: CompensatedField + RealField,
    T::Real: Float,
{
    let mut acc = T::Real::zero();
    for row in 0..matrix.nrows() {
        let value = matrix[(row, col)].abs();
        acc += value * value;
    }
    acc
}

fn normalize_column<T>(matrix: MatRef<'_, T>, col: usize) -> Mat<T>
where
    T: CompensatedField + RealField,
    T::Real: Float,
{
    let norm = vector_norm_sq(matrix, col).sqrt();
    scale_column(matrix, col, T::Real::one() / norm)
}

fn scale_column<T>(matrix: MatRef<'_, T>, col: usize, scale: T::Real) -> Mat<T>
where
    T: CompensatedField + RealField,
    T::Real: Float,
{
    Mat::from_fn(matrix.nrows(), 1, |row, _| {
        matrix[(row, col)] * T::from_real_imag(scale, T::Real::zero())
    })
}

fn scale_matrix<T>(matrix: MatRef<'_, T>, scale: T::Real) -> Mat<T>
where
    T: CompensatedField + RealField,
    T::Real: Float,
{
    Mat::from_fn(matrix.nrows(), matrix.ncols(), |row, col| {
        matrix[(row, col)] * T::from_real_imag(scale, T::Real::zero())
    })
}

fn assemble_columns<T>(columns: &[Mat<T>]) -> Mat<T>
where
    T: CompensatedField + RealField,
    T::Real: Float,
{
    Mat::from_fn(columns[0].nrows(), columns.len(), |row, col| {
        columns[col][(row, 0)]
    })
}

fn pole_tol<T>(scale: T::Real) -> T::Real
where
    T: CompensatedField + RealField,
    T::Real: Float,
{
    from_f64::<T::Real>(256.0) * eps::<T::Real>().sqrt() * scale.abs().max(T::Real::one())
}

#[cfg(test)]
mod tests {
    use super::{
        PolePlacementError, dplace_observer_poles_dense, dplace_poles_dense,
        place_observer_poles_dense, place_poles_dense,
    };
    use crate::control::lti::{ContinuousStateSpace, DiscreteStateSpace};
    use faer::Mat;
    use faer::complex::Complex;
    use nalgebra::ComplexField;

    fn assert_close(lhs: f64, rhs: f64, tol: f64) {
        let err = (lhs - rhs).abs();
        assert!(err <= tol, "lhs={lhs}, rhs={rhs}, err={err}, tol={tol}");
    }

    fn assert_poles_close(lhs: &[Complex<f64>], rhs: &[Complex<f64>], tol: f64) {
        assert_eq!(lhs.len(), rhs.len());
        for (idx, (&lhs, &rhs)) in lhs.iter().zip(rhs.iter()).enumerate() {
            let err = (lhs - rhs).abs();
            assert!(
                err <= tol,
                "pole {idx} mismatch: lhs={lhs:?}, rhs={rhs:?}, err={err}, tol={tol}"
            );
        }
    }

    #[test]
    fn continuous_scalar_place_poles_matches_closed_form() {
        let a = Mat::from_fn(1, 1, |_, _| 1.0f64);
        let b = Mat::from_fn(1, 1, |_, _| 2.0f64);
        let solve = place_poles_dense(a.as_ref(), b.as_ref(), &[Complex::new(-3.0, 0.0)]).unwrap();
        assert_close(solve.gain[(0, 0)], 2.0, 1.0e-12);
        assert_close(solve.placed_matrix[(0, 0)], -3.0, 1.0e-12);
        assert_poles_close(&solve.achieved_poles, &[Complex::new(-3.0, 0.0)], 1.0e-12);
    }

    #[test]
    fn discrete_scalar_place_poles_matches_closed_form() {
        let a = Mat::from_fn(1, 1, |_, _| 1.2f64);
        let b = Mat::from_fn(1, 1, |_, _| 1.0f64);
        let solve = dplace_poles_dense(a.as_ref(), b.as_ref(), &[Complex::new(0.4, 0.0)]).unwrap();
        assert_close(solve.gain[(0, 0)], 0.8, 1.0e-12);
        assert_close(solve.placed_matrix[(0, 0)], 0.4, 1.0e-12);
    }

    #[test]
    fn continuous_complex_conjugate_pair_is_placed() {
        let a = Mat::from_fn(2, 2, |row, col| match (row, col) {
            (0, 1) => 1.0,
            _ => 0.0,
        });
        let b = Mat::from_fn(2, 1, |row, _| if row == 1 { 1.0 } else { 0.0 });
        let desired = [Complex::new(-1.0, 2.0), Complex::new(-1.0, -2.0)];
        let solve = place_poles_dense(a.as_ref(), b.as_ref(), &desired).unwrap();

        let mut expected = desired.to_vec();
        expected.sort_by(|lhs, rhs| super::compare_poles(*lhs, *rhs));
        assert_poles_close(&solve.achieved_poles, &expected, 1.0e-10);
        assert!(solve.placement_residual <= 1.0e-10);
    }

    #[test]
    fn observer_placement_uses_duality() {
        let a = Mat::from_fn(2, 2, |row, col| match (row, col) {
            (0, 1) => 1.0,
            _ => 0.0,
        });
        let c = Mat::from_fn(1, 2, |_, col| if col == 0 { 1.0 } else { 0.0 });
        let desired = [Complex::new(-2.0, 0.0), Complex::new(-3.0, 0.0)];
        let solve = place_observer_poles_dense(a.as_ref(), c.as_ref(), &desired).unwrap();

        let mut expected = desired.to_vec();
        expected.sort_by(|lhs, rhs| super::compare_poles(*lhs, *rhs));
        assert_poles_close(&solve.achieved_poles, &expected, 1.0e-10);
    }

    #[test]
    fn uncontrollable_system_is_rejected() {
        let a = Mat::from_fn(2, 2, |row, col| match (row, col) {
            (0, 0) => 1.0,
            (1, 1) => 2.0,
            _ => 0.0,
        });
        let b = Mat::from_fn(2, 1, |row, _| if row == 0 { 1.0 } else { 0.0 });
        let err = place_poles_dense(
            a.as_ref(),
            b.as_ref(),
            &[Complex::new(-1.0, 0.0), Complex::new(-2.0, 0.0)],
        )
        .unwrap_err();
        assert!(matches!(err, PolePlacementError::Uncontrollable));
    }

    #[test]
    fn unobservable_system_is_rejected() {
        let a = Mat::from_fn(2, 2, |row, col| match (row, col) {
            (0, 0) => 1.0,
            (1, 1) => 2.0,
            _ => 0.0,
        });
        let c = Mat::from_fn(1, 2, |_, col| if col == 0 { 1.0 } else { 0.0 });
        let err = place_observer_poles_dense(
            a.as_ref(),
            c.as_ref(),
            &[Complex::new(-1.0, 0.0), Complex::new(-2.0, 0.0)],
        )
        .unwrap_err();
        assert!(matches!(err, PolePlacementError::Unobservable));
    }

    #[test]
    fn mimo_state_feedback_and_observer_place_real_poles() {
        let a = Mat::from_fn(2, 2, |row, col| if row == col { 1.0 } else { 0.0 });
        let b = Mat::from_fn(2, 2, |row, col| if row == col { 1.0 } else { 0.0 });
        let c = Mat::from_fn(2, 2, |row, col| if row == col { 1.0 } else { 0.0 });
        let desired = [Complex::new(-1.0, 0.0), Complex::new(-2.0, 0.0)];

        let fb = place_poles_dense(a.as_ref(), b.as_ref(), &desired).unwrap();
        let obs = place_observer_poles_dense(a.as_ref(), c.as_ref(), &desired).unwrap();

        let mut expected = desired.to_vec();
        expected.sort_by(|lhs, rhs| super::compare_poles(*lhs, *rhs));
        assert_poles_close(&fb.achieved_poles, &expected, 1.0e-8);
        assert_poles_close(&obs.achieved_poles, &expected, 1.0e-8);
    }

    #[test]
    fn mimo_complex_poles_are_rejected_in_first_pass() {
        let a = Mat::from_fn(2, 2, |row, col| if row == col { 1.0 } else { 0.0 });
        let b = Mat::from_fn(2, 2, |row, col| if row == col { 1.0 } else { 0.0 });

        let err = place_poles_dense(
            a.as_ref(),
            b.as_ref(),
            &[Complex::new(-1.0, 1.0), Complex::new(-1.0, -1.0)],
        )
        .unwrap_err();
        assert!(matches!(
            err,
            PolePlacementError::ComplexMimoPolesUnsupported
        ));
    }

    #[test]
    fn state_space_wrappers_match_free_functions() {
        let continuous = ContinuousStateSpace::new(
            Mat::from_fn(2, 2, |row, col| match (row, col) {
                (0, 1) => 1.0,
                _ => 0.0,
            }),
            Mat::from_fn(2, 1, |row, _| if row == 1 { 1.0 } else { 0.0 }),
            Mat::from_fn(1, 2, |_, col| if col == 0 { 1.0 } else { 0.0 }),
            Mat::zeros(1, 1),
        )
        .unwrap();
        let discrete = DiscreteStateSpace::new(
            Mat::from_fn(2, 2, |row, col| match (row, col) {
                (0, 0) => 1.0,
                (0, 1) => 1.0,
                (1, 1) => 1.0,
                _ => 0.0,
            }),
            Mat::from_fn(2, 1, |row, _| if row == 1 { 1.0 } else { 0.0 }),
            Mat::from_fn(1, 2, |_, col| if col == 0 { 1.0 } else { 0.0 }),
            Mat::zeros(1, 1),
            0.1,
        )
        .unwrap();

        let desired_c = [Complex::new(-2.0, 0.0), Complex::new(-3.0, 0.0)];
        let desired_d = [Complex::new(0.2, 0.0), Complex::new(0.3, 0.0)];
        let free_c = place_poles_dense(continuous.a(), continuous.b(), &desired_c).unwrap();
        let meth_c = continuous.place_poles(&desired_c).unwrap();
        let free_d = dplace_observer_poles_dense(discrete.a(), discrete.c(), &desired_d).unwrap();
        let meth_d = discrete.place_observer_poles(&desired_d).unwrap();

        assert_close(free_c.gain[(0, 0)], meth_c.gain[(0, 0)], 1.0e-12);
        assert_close(free_c.gain[(0, 1)], meth_c.gain[(0, 1)], 1.0e-12);
        assert_close(free_d.gain[(0, 0)], meth_d.gain[(0, 0)], 1.0e-12);
        assert_close(free_d.gain[(1, 0)], meth_d.gain[(1, 0)], 1.0e-12);
    }
}
