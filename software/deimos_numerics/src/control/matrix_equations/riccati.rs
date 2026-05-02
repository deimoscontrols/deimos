//! Dense algebraic Riccati solvers for continuous and discrete LTI systems.
//!
//! This module is the numerical foundation for synthesis routines such as LQR
//! and Kalman filtering. The implementation is intentionally dense-first and
//! conservative:
//!
//! - CARE uses the stable invariant subspace of the Hamiltonian matrix
//! - DARE uses the stable generalized invariant subspace of the symplectic
//!   pencil
//! - residuals are recomputed in the original matrix equations using the same
//!   compensated dense reductions used elsewhere in the control module
//!
//! The public API returns the Riccati solution together with the induced
//! state-feedback gain and a post-solve stabilizing check on the recovered
//! closed-loop matrix.
//!
//! # Two Intuitions
//!
//! 1. **Optimality view.** Riccati equations encode the tradeoff between state
//!    error and control effort that underlies optimal regulators and
//!    estimators.
//! 2. **Invariant-subspace view.** Numerically, the dense solvers here recover
//!    the Riccati solution from a stable invariant subspace of an augmented
//!    Hamiltonian or symplectic operator.
//!
//! # Glossary
//!
//! - **CARE / DARE:** Continuous/discrete algebraic Riccati equations.
//! - **Hamiltonian matrix:** Continuous-time augmented matrix whose stable
//!   invariant subspace encodes the CARE solution.
//! - **Symplectic pencil:** Discrete-time generalized eigenproblem whose stable
//!   invariant subspace encodes the DARE solution.
//!
//! # Mathematical Formulation
//!
//! For system matrices `A in C^(n x n)`, `B in C^(n x m)`, state weighting
//! `Q = Q^H in C^(n x n)`, and control weighting `R = R^H in C^(m x m)`, the
//! module solves the stabilizing Hermitian solution `X = X^H` of:
//!
//! - continuous-time CARE:
//!   `A^H X + X A - X B R^-1 B^H X + Q = 0`
//! - discrete-time DARE:
//!   `A^H X A - X - A^H X B (R + B^H X B)^-1 B^H X A + Q = 0`
//!
//! It then recovers the corresponding state-feedback gain for the convention
//! `u = -K x`:
//!
//! - CARE gain: `K = R^-1 B^H X`
//! - DARE gain: `K = (R + B^H X B)^-1 B^H X A`
//!
//! A returned solution is considered stabilizing when the induced closed-loop
//! matrix is stable in the appropriate domain:
//!
//! - continuous time: `A - B K` has eigenvalues in the open left half-plane
//! - discrete time: `A - B K` has eigenvalues strictly inside the unit disk
//!
//! # Implementation Notes
//!
//! - CARE uses dense eigen decomposition of the Hamiltonian matrix.
//! - DARE uses dense generalized eigen decomposition of the symplectic pencil.
//! - Residuals are recomputed in the original Riccati equation rather than
//!   inferred from backend diagnostics.

use crate::control::dense_ops::{
    default_solve_tolerance, dense_mul, dense_mul_adjoint_lhs, frobenius_norm,
    hermitian_project_in_place, solve_left_checked, solve_right_checked,
};
use crate::decomp::{
    DecompError, DenseDecompParams, dense_eigen, dense_eigenvalues, dense_generalized_eigen,
};
use crate::sparse::col::col_slice;
use crate::sparse::compensated::CompensatedField;
use alloc::vec::Vec;
use core::fmt;
use faer::complex::Complex;
use faer::linalg::evd::EvdError;
use faer::linalg::gevd::GevdError;
use faer::{Mat, MatRef};
use faer_traits::ext::ComplexFieldExt;
use faer_traits::{ComplexField, RealField};
use num_traits::{Float, One, Zero};

/// Result of a dense Riccati solve.
///
/// `solution` is the Hermitian Riccati matrix `X`. `gain` is the associated
/// state-feedback gain `K` for the convention `u = -K x`.
#[derive(Clone, Debug)]
pub struct RiccatiSolve<T: CompensatedField>
where
    T::Real: Float,
{
    /// Hermitian Riccati solution matrix.
    pub solution: Mat<T>,
    /// State-feedback gain recovered from the solved Riccati equation.
    pub gain: Mat<T>,
    /// Compensated Frobenius norm of the Riccati residual.
    pub residual_norm: T::Real,
    /// Whether the recovered closed-loop matrix satisfies the stabilizing test.
    pub stabilizing: bool,
}

/// Errors that can occur while building or solving dense Riccati systems.
#[derive(Debug)]
pub enum RiccatiError {
    /// A matrix that should be square was not square.
    NonSquare {
        /// Identifies the matrix that should have been square.
        which: &'static str,
        /// Actual row count.
        nrows: usize,
        /// Actual column count.
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
    /// A control-weight matrix or Schur complement was singular or numerically
    /// unusable in a required solve.
    SingularControlWeight {
        /// Identifies the weight matrix or Schur complement that was singular.
        which: &'static str,
    },
    /// The invariant-subspace partition needed to recover `X` was singular or
    /// numerically unusable.
    SingularInvariantSubspace,
    /// Dense eigendecomposition of the Hamiltonian failed.
    Eigen(EvdError),
    /// Dense generalized eigendecomposition of the symplectic pencil failed.
    GeneralizedEigen(GevdError),
    /// The stable invariant-subspace selection did not yield the expected
    /// state dimension.
    NoStabilizingSolution,
    /// A projected real-valued result still carried a materially complex part.
    ComplexProjectionFailed {
        /// Identifies the projected matrix that remained materially complex.
        which: &'static str,
    },
    /// A solve or residual check produced non-finite output.
    NonFiniteResult {
        /// Identifies the solve or residual check that produced non-finite output.
        which: &'static str,
    },
}

impl fmt::Display for RiccatiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

impl core::error::Error for RiccatiError {}

impl From<EvdError> for RiccatiError {
    fn from(value: EvdError) -> Self {
        Self::Eigen(value)
    }
}

impl From<GevdError> for RiccatiError {
    fn from(value: GevdError) -> Self {
        Self::GeneralizedEigen(value)
    }
}

fn expect_dense_evd(err: DecompError) -> RiccatiError {
    match err {
        DecompError::DenseEvd(err) => RiccatiError::Eigen(err),
        // The Riccati paths validate dimensions before entering the wrapper, so
        // the only expected decomposition failure here is backend eigensolver
        // failure itself.
        other => unreachable!("unexpected dense_eigen error in Riccati solver: {other:?}"),
    }
}

fn expect_dense_gevd(err: DecompError) -> RiccatiError {
    match err {
        DecompError::DenseGevd(err) => RiccatiError::GeneralizedEigen(err),
        // As above, the generalized-eigen wrapper is only expected to surface
        // the backend pencil-factorization failure once the Riccati inputs have
        // been validated locally.
        other => {
            unreachable!("unexpected dense_generalized_eigen error in Riccati solver: {other:?}")
        }
    }
}

/// Solves the dense continuous-time algebraic Riccati equation
///
/// `A^H X + X A - X B R^-1 B^H X + Q = 0`
///
/// and returns the associated state-feedback gain
///
/// `K = R^-1 B^H X`
///
/// for the convention `u = -K x`.
pub fn solve_care_dense<T>(
    a: MatRef<'_, T>,
    b: MatRef<'_, T>,
    q: MatRef<'_, T>,
    r: MatRef<'_, T>,
) -> Result<RiccatiSolve<T>, RiccatiError>
where
    T: CompensatedField,
    T::Real: Float + RealField,
{
    validate_riccati_dims(a, b, q, r)?;
    let tol = default_solve_tolerance::<T>();

    // `G = B R^-1 B^H` is the quadratic control-weight term that appears in
    // the Hamiltonian matrix. Forming it once keeps the subsequent invariant-
    // subspace step close to the textbook CARE derivation.
    let r_inv_bh = solve_left_checked(r, b.adjoint().to_owned().as_ref(), tol, || {
        RiccatiError::SingularControlWeight { which: "r" }
    })?;
    let g = dense_mul(b, r_inv_bh.as_ref());

    let hamiltonian = build_care_hamiltonian(
        to_complex_mat(a).as_ref(),
        to_complex_mat(g.as_ref()).as_ref(),
        to_complex_mat(q).as_ref(),
    );
    let eig = dense_eigen(
        hamiltonian.as_ref(),
        &DenseDecompParams::<Complex<T::Real>>::new(),
    )
    .map_err(expect_dense_evd)?;

    // The stabilizing CARE solution is determined by the invariant subspace
    // associated with the Hamiltonian eigenvalues in the open left half-plane.
    let stable = stable_columns_from_eigen(col_slice(&eig.values), tol);
    let (u1, u2) = partition_subspace(eig.vectors.as_ref(), a.nrows(), &stable)?;
    // Writing the stable basis as `[U1; U2]` gives the Riccati solution
    // through the graph relation `X = U2 U1^-1`.
    let mut solution_c = solve_right_checked(u2.as_ref(), u1.as_ref(), tol, || {
        RiccatiError::SingularInvariantSubspace
    })?;
    // The exact stabilizing solution is Hermitian. The invariant-subspace solve
    // can introduce a small skew-Hermitian component, so project it away before
    // checking residuals or recovering the gain.
    hermitian_project_in_place(&mut solution_c);

    let solution = from_complex_mat(solution_c.as_ref(), tol, "care.solution")?;
    let gain = care_gain_from_solution(b, r, solution.as_ref())?;
    let residual = care_residual(a, b, q, gain.as_ref(), solution.as_ref());
    let residual_norm = frobenius_norm(residual.as_ref());
    if !residual_norm.is_finite() {
        return Err(RiccatiError::NonFiniteResult {
            which: "care.residual_norm",
        });
    }

    let stabilizing = care_is_stabilizing(a, b, gain.as_ref(), tol)?;
    Ok(RiccatiSolve {
        solution,
        gain,
        residual_norm,
        stabilizing,
    })
}

/// Solves the dense discrete-time algebraic Riccati equation
///
/// `X = A^H X A - A^H X B (R + B^H X B)^-1 B^H X A + Q`
///
/// and returns the associated state-feedback gain
///
/// `K = (R + B^H X B)^-1 B^H X A`
///
/// for the convention `u[k] = -K x[k]`.
pub fn solve_dare_dense<T>(
    a: MatRef<'_, T>,
    b: MatRef<'_, T>,
    q: MatRef<'_, T>,
    r: MatRef<'_, T>,
) -> Result<RiccatiSolve<T>, RiccatiError>
where
    T: CompensatedField,
    T::Real: Float + RealField,
{
    validate_riccati_dims(a, b, q, r)?;
    let tol = default_solve_tolerance::<T>();

    // As in the CARE path, factor the quadratic input term once so the DARE
    // pencil can be assembled directly from `A`, `Q`, and `G = B R^-1 B^H`.
    let r_inv_bh = solve_left_checked(r, b.adjoint().to_owned().as_ref(), tol, || {
        RiccatiError::SingularControlWeight { which: "r" }
    })?;
    let g = dense_mul(b, r_inv_bh.as_ref());

    let (h, j) = build_dare_pencil(
        to_complex_mat(a).as_ref(),
        to_complex_mat(g.as_ref()).as_ref(),
        to_complex_mat(q).as_ref(),
    );
    let gevd = dense_generalized_eigen(h.as_ref(), j.as_ref()).map_err(expect_dense_gevd)?;
    // For the symplectic pencil, the stabilizing solution is determined by the
    // invariant subspace whose generalized eigenvalues lie strictly inside the
    // unit disk.
    let stable =
        stable_columns_from_generalized_eigen(col_slice(&gevd.alpha), col_slice(&gevd.beta), tol);
    let (u1, u2) = partition_subspace(gevd.vectors.as_ref(), a.nrows(), &stable)?;
    let mut solution_c = solve_right_checked(u2.as_ref(), u1.as_ref(), tol, || {
        RiccatiError::SingularInvariantSubspace
    })?;
    hermitian_project_in_place(&mut solution_c);

    let solution = from_complex_mat(solution_c.as_ref(), tol, "dare.solution")?;
    let gain = dare_gain_from_solution(a, b, r, solution.as_ref())?;
    let residual = dare_residual(a, b, q, gain.as_ref(), solution.as_ref());
    let residual_norm = frobenius_norm(residual.as_ref());
    if !residual_norm.is_finite() {
        return Err(RiccatiError::NonFiniteResult {
            which: "dare.residual_norm",
        });
    }

    let stabilizing = dare_is_stabilizing(a, b, gain.as_ref(), tol)?;
    Ok(RiccatiSolve {
        solution,
        gain,
        residual_norm,
        stabilizing,
    })
}

/// Computes the continuous-time state-feedback gain `K = R^-1 B^H X`.
pub fn care_gain_from_solution<T>(
    b: MatRef<'_, T>,
    r: MatRef<'_, T>,
    x: MatRef<'_, T>,
) -> Result<Mat<T>, RiccatiError>
where
    T: CompensatedField,
    T::Real: Float + RealField,
{
    validate_square("r", r.nrows(), r.ncols())?;
    validate_dims("b.nrows", b.nrows(), b.ncols(), x.nrows(), b.ncols())?;
    validate_dims("x", x.nrows(), x.ncols(), x.nrows(), x.nrows())?;

    let rhs = dense_mul_adjoint_lhs(b, x);
    solve_left_checked(r, rhs.as_ref(), default_solve_tolerance::<T>(), || {
        RiccatiError::SingularControlWeight { which: "r" }
    })
}

/// Computes the discrete-time state-feedback gain
/// `K = (R + B^H X B)^-1 B^H X A`.
pub fn dare_gain_from_solution<T>(
    a: MatRef<'_, T>,
    b: MatRef<'_, T>,
    r: MatRef<'_, T>,
    x: MatRef<'_, T>,
) -> Result<Mat<T>, RiccatiError>
where
    T: CompensatedField,
    T::Real: Float + RealField,
{
    validate_riccati_dims(a, b, x, r)?;
    let b_h_x = dense_mul_adjoint_lhs(b, x);
    let b_h_x_b = dense_mul(b_h_x.as_ref(), b);
    let s = r + &b_h_x_b;
    let rhs = dense_mul(b_h_x.as_ref(), a);
    solve_left_checked(
        s.as_ref(),
        rhs.as_ref(),
        default_solve_tolerance::<T>(),
        || RiccatiError::SingularControlWeight {
            which: "r_plus_bhx b",
        },
    )
}

fn validate_riccati_dims<T>(
    a: MatRef<'_, T>,
    b: MatRef<'_, T>,
    q: MatRef<'_, T>,
    r: MatRef<'_, T>,
) -> Result<(), RiccatiError> {
    validate_square("a", a.nrows(), a.ncols())?;
    validate_square("q", q.nrows(), q.ncols())?;
    validate_square("r", r.nrows(), r.ncols())?;
    validate_dims("b", b.nrows(), b.ncols(), a.nrows(), r.nrows())?;
    validate_dims("q", q.nrows(), q.ncols(), a.nrows(), a.ncols())?;
    Ok(())
}

fn validate_square(which: &'static str, nrows: usize, ncols: usize) -> Result<(), RiccatiError> {
    if nrows != ncols {
        return Err(RiccatiError::NonSquare {
            which,
            nrows,
            ncols,
        });
    }
    Ok(())
}

fn validate_dims(
    which: &'static str,
    actual_nrows: usize,
    actual_ncols: usize,
    expected_nrows: usize,
    expected_ncols: usize,
) -> Result<(), RiccatiError> {
    if actual_nrows != expected_nrows || actual_ncols != expected_ncols {
        return Err(RiccatiError::DimensionMismatch {
            which,
            expected_nrows,
            expected_ncols,
            actual_nrows,
            actual_ncols,
        });
    }
    Ok(())
}

/// Builds the Hamiltonian matrix whose stable invariant subspace encodes the
/// stabilizing CARE solution.
///
/// With `G = B R^-1 B^H`, the standard Hamiltonian form is
///
/// ```text
/// [  A  -G ]
/// [ -Q -A^H]
/// ```
///
/// and the graph of its stable invariant subspace yields `X`.
fn build_care_hamiltonian<R>(
    a: MatRef<'_, Complex<R>>,
    g: MatRef<'_, Complex<R>>,
    q: MatRef<'_, Complex<R>>,
) -> Mat<Complex<R>>
where
    R: Float + RealField,
{
    let n = a.nrows();
    Mat::from_fn(2 * n, 2 * n, |row, col| match (row < n, col < n) {
        (true, true) => a[(row, col)],
        (true, false) => -g[(row, col - n)],
        (false, true) => -q[(row - n, col)],
        (false, false) => -a[(col - n, row - n)].conj(),
    })
}

/// Builds the symplectic generalized-eigen pencil used for the stabilizing
/// DARE solution.
///
/// In this dense implementation the pencil is written as
///
/// ```text
/// H - λ J
/// ```
///
/// with
///
/// ```text
/// H = [ A  0]
///     [-Q  I]
///
/// J = [ I  G ]
///     [ 0  A^H]
/// ```
///
/// so that the invariant subspace inside the unit disk determines the desired
/// Riccati graph.
fn build_dare_pencil<R>(
    a: MatRef<'_, Complex<R>>,
    g: MatRef<'_, Complex<R>>,
    q: MatRef<'_, Complex<R>>,
) -> (Mat<Complex<R>>, Mat<Complex<R>>)
where
    R: Float + RealField,
{
    let n = a.nrows();
    let h = Mat::from_fn(2 * n, 2 * n, |row, col| match (row < n, col < n) {
        (true, true) => a[(row, col)],
        (true, false) => Complex::new(<R as Zero>::zero(), <R as Zero>::zero()),
        (false, true) => -q[(row - n, col)],
        (false, false) => {
            if row == col {
                Complex::new(<R as One>::one(), <R as Zero>::zero())
            } else {
                Complex::new(<R as Zero>::zero(), <R as Zero>::zero())
            }
        }
    });
    let j = Mat::from_fn(2 * n, 2 * n, |row, col| match (row < n, col < n) {
        (true, true) => {
            if row == col {
                Complex::new(<R as One>::one(), <R as Zero>::zero())
            } else {
                Complex::new(<R as Zero>::zero(), <R as Zero>::zero())
            }
        }
        (true, false) => g[(row, col - n)],
        (false, true) => Complex::new(<R as Zero>::zero(), <R as Zero>::zero()),
        (false, false) => a[(col - n, row - n)].conj(),
    });
    (h, j)
}

fn stable_columns_from_eigen<R>(values: &[Complex<R>], tol: R) -> Vec<usize>
where
    R: Float + RealField,
{
    // CARE wants the Hamiltonian subspace in the open left half-plane. A small
    // tolerance keeps numerically marginal eigenvalues from being accepted as
    // safely stable.
    values
        .iter()
        .enumerate()
        .filter_map(|(index, &value)| (value.re < -tol).then_some(index))
        .collect()
}

fn stable_columns_from_generalized_eigen<R>(
    alpha: &[Complex<R>],
    beta: &[Complex<R>],
    tol: R,
) -> Vec<usize>
where
    R: Float + RealField,
{
    // DARE uses generalized eigenvalues of the symplectic pencil. The
    // stabilizing subspace is the one strictly inside the unit disk.
    alpha
        .iter()
        .zip(beta.iter())
        .enumerate()
        .filter_map(|(index, (&alpha, &beta))| {
            if beta.abs() <= tol {
                return None;
            }
            let lambda = alpha / beta;
            (lambda.abs() < <R as One>::one() - tol).then_some(index)
        })
        .collect()
}

fn partition_subspace<R>(
    vectors: MatRef<'_, Complex<R>>,
    n: usize,
    cols: &[usize],
) -> Result<(Mat<Complex<R>>, Mat<Complex<R>>), RiccatiError>
where
    R: Float + RealField,
{
    if cols.len() != n {
        return Err(RiccatiError::NoStabilizingSolution);
    }

    // The invariant-subspace basis is partitioned as `[U1; U2]`, where `U1`
    // is the top state block and `U2` is the bottom state block. Recovering
    // `X` then reduces to one dense solve with `U1`.
    let u1 = Mat::from_fn(n, n, |row, col| vectors[(row, cols[col])]);
    let u2 = Mat::from_fn(n, n, |row, col| vectors[(n + row, cols[col])]);
    Ok((u1, u2))
}

fn care_residual<T>(
    a: MatRef<'_, T>,
    b: MatRef<'_, T>,
    q: MatRef<'_, T>,
    k: MatRef<'_, T>,
    x: MatRef<'_, T>,
) -> Mat<T>
where
    T: CompensatedField,
    T::Real: Float,
{
    let a_h_x = dense_mul_adjoint_lhs(a, x);
    let x_a = dense_mul(x, a);
    let xb = dense_mul(x, b);
    let xbk = dense_mul(xb.as_ref(), k);
    Mat::from_fn(x.nrows(), x.ncols(), |row, col| {
        a_h_x[(row, col)] + x_a[(row, col)] - xbk[(row, col)] + q[(row, col)]
    })
}

fn dare_residual<T>(
    a: MatRef<'_, T>,
    b: MatRef<'_, T>,
    q: MatRef<'_, T>,
    k: MatRef<'_, T>,
    x: MatRef<'_, T>,
) -> Mat<T>
where
    T: CompensatedField,
    T::Real: Float,
{
    let a_h_x = dense_mul_adjoint_lhs(a, x);
    let a_h_x_a = dense_mul(a_h_x.as_ref(), a);
    let a_h_x_b = dense_mul(a_h_x.as_ref(), b);
    let a_h_x_b_k = dense_mul(a_h_x_b.as_ref(), k);
    Mat::from_fn(x.nrows(), x.ncols(), |row, col| {
        a_h_x_a[(row, col)] - a_h_x_b_k[(row, col)] - x[(row, col)] + q[(row, col)]
    })
}

fn care_is_stabilizing<T>(
    a: MatRef<'_, T>,
    b: MatRef<'_, T>,
    k: MatRef<'_, T>,
    tol: T::Real,
) -> Result<bool, RiccatiError>
where
    T: CompensatedField,
    T::Real: Float + RealField,
{
    let bk = dense_mul(b, k);
    let closed_loop = a - &bk;
    let poles = dense_eigenvalues(closed_loop.as_ref())
        .map_err(expect_dense_evd)?
        .try_as_col_major()
        .unwrap()
        .as_slice()
        .to_vec();
    Ok(poles.into_iter().all(|pole| pole.re < -tol))
}

fn dare_is_stabilizing<T>(
    a: MatRef<'_, T>,
    b: MatRef<'_, T>,
    k: MatRef<'_, T>,
    tol: T::Real,
) -> Result<bool, RiccatiError>
where
    T: CompensatedField,
    T::Real: Float + RealField,
{
    let bk = dense_mul(b, k);
    let closed_loop = a - &bk;
    let poles = dense_eigenvalues(closed_loop.as_ref())
        .map_err(expect_dense_evd)?
        .try_as_col_major()
        .unwrap()
        .as_slice()
        .to_vec();
    Ok(poles
        .into_iter()
        .all(|pole| pole.abs() < <T::Real as One>::one() - tol))
}

fn to_complex_mat<T>(matrix: MatRef<'_, T>) -> Mat<Complex<T::Real>>
where
    T: ComplexField + Copy,
    T::Real: Float + RealField,
{
    Mat::from_fn(matrix.nrows(), matrix.ncols(), |row, col| {
        let value = matrix[(row, col)];
        Complex::new(value.real(), value.imag())
    })
}

fn from_complex_mat<T>(
    matrix: MatRef<'_, Complex<T::Real>>,
    tol: T::Real,
    which: &'static str,
) -> Result<Mat<T>, RiccatiError>
where
    T: CompensatedField,
    T::Real: Float + RealField,
{
    // The invariant-subspace formulas are evaluated in complex arithmetic even
    // for real problems. For real data, the true stabilizing solution should
    // still be real, so reject projections that retain a materially imaginary
    // part rather than silently discarding it.
    let mut max_abs = <T::Real as Zero>::zero();
    let mut max_imag = <T::Real as Zero>::zero();
    for col in 0..matrix.ncols() {
        for row in 0..matrix.nrows() {
            let value = matrix[(row, col)];
            max_abs = max_abs.max(value.abs());
            max_imag = max_imag.max(value.im.abs());
        }
    }
    let one = <T::Real as One>::one();
    if T::IS_REAL && max_imag > max_abs.max(one) * tol * (one + one) {
        return Err(RiccatiError::ComplexProjectionFailed { which });
    }

    let out = Mat::from_fn(matrix.nrows(), matrix.ncols(), |row, col| {
        let value = matrix[(row, col)];
        let imag = if T::IS_REAL {
            <T::Real as Zero>::zero()
        } else {
            value.im
        };
        T::from_real_imag(value.re, imag)
    });
    if !out.as_ref().is_all_finite() {
        return Err(RiccatiError::NonFiniteResult { which });
    }
    Ok(out)
}

#[cfg(test)]
mod test {
    use super::{
        RiccatiError, care_gain_from_solution, dare_gain_from_solution, solve_care_dense,
        solve_dare_dense,
    };
    use faer::{Mat, c64};
    use faer_traits::ext::ComplexFieldExt;

    fn assert_close<T>(lhs: &Mat<T>, rhs: &Mat<T>, tol: T::Real)
    where
        T: super::CompensatedField,
        T::Real: num_traits::Float,
    {
        assert_eq!(lhs.nrows(), rhs.nrows());
        assert_eq!(lhs.ncols(), rhs.ncols());
        for col in 0..lhs.ncols() {
            for row in 0..lhs.nrows() {
                let err = (lhs[(row, col)] - rhs[(row, col)]).abs1();
                assert!(
                    err <= tol,
                    "entry ({row}, {col}) mismatch: err={err:?}, tol={tol:?}",
                );
            }
        }
    }

    #[test]
    fn care_matches_scalar_closed_form() {
        let a = Mat::from_fn(1, 1, |_, _| 1.0f64);
        let b = Mat::from_fn(1, 1, |_, _| 1.0f64);
        let q = Mat::from_fn(1, 1, |_, _| 1.0f64);
        let r = Mat::from_fn(1, 1, |_, _| 1.0f64);
        let solve = solve_care_dense(a.as_ref(), b.as_ref(), q.as_ref(), r.as_ref()).unwrap();

        let expected = 1.0 + 2.0f64.sqrt();
        assert!((solve.solution[(0, 0)] - expected).abs() < 1.0e-10);
        assert!((solve.gain[(0, 0)] - expected).abs() < 1.0e-10);
        assert!(solve.residual_norm < 1.0e-10);
        assert!(solve.stabilizing);
    }

    #[test]
    fn care_handles_small_diagonal_system() {
        let a = Mat::from_fn(2, 2, |row, col| match (row, col) {
            (0, 0) => 1.0,
            (1, 1) => -0.5,
            _ => 0.0,
        });
        let b = Mat::from_fn(2, 2, |row, col| if row == col { 1.0 } else { 0.0 });
        let q = Mat::from_fn(
            2,
            2,
            |row, col| if row == col { 1.0 + row as f64 } else { 0.0 },
        );
        let r = Mat::from_fn(2, 2, |row, col| if row == col { 1.0 } else { 0.0 });
        let solve = solve_care_dense(a.as_ref(), b.as_ref(), q.as_ref(), r.as_ref()).unwrap();

        let x11 = 1.0 + 2.0f64.sqrt();
        let x22 = -0.5 + (0.25 + 2.0).sqrt();
        let expected = Mat::from_fn(2, 2, |row, col| match (row, col) {
            (0, 0) => x11,
            (1, 1) => x22,
            _ => 0.0,
        });
        assert_close(&solve.solution, &expected, 1.0e-10);
        assert!(solve.stabilizing);
    }

    #[test]
    fn care_handles_complex_scalar_case() {
        let a = Mat::from_fn(1, 1, |_, _| c64::new(1.0, 1.0));
        let b = Mat::from_fn(1, 1, |_, _| c64::new(1.0, 0.0));
        let q = Mat::from_fn(1, 1, |_, _| c64::new(1.0, 0.0));
        let r = Mat::from_fn(1, 1, |_, _| c64::new(1.0, 0.0));
        let solve = solve_care_dense(a.as_ref(), b.as_ref(), q.as_ref(), r.as_ref()).unwrap();

        let expected = 1.0 + 2.0f64.sqrt();
        assert!((solve.solution[(0, 0)] - c64::new(expected, 0.0)).abs() < 1.0e-10);
        assert!(solve.stabilizing);
    }

    #[test]
    fn dare_matches_scalar_closed_form() {
        let a = Mat::from_fn(1, 1, |_, _| 1.2f64);
        let b = Mat::from_fn(1, 1, |_, _| 1.0f64);
        let q = Mat::from_fn(1, 1, |_, _| 1.0f64);
        let r = Mat::from_fn(1, 1, |_, _| 1.0f64);
        let solve = solve_dare_dense(a.as_ref(), b.as_ref(), q.as_ref(), r.as_ref()).unwrap();

        let expected = (1.44 + (1.44f64 * 1.44 + 4.0).sqrt()) / 2.0;
        assert!((solve.solution[(0, 0)] - expected).abs() < 1.0e-10);
        assert!(solve.residual_norm < 1.0e-10);
        assert!(solve.stabilizing);
    }

    #[test]
    fn dare_handles_small_diagonal_system() {
        let a = Mat::from_fn(2, 2, |row, col| match (row, col) {
            (0, 0) => 1.2,
            (1, 1) => 0.5,
            _ => 0.0,
        });
        let b = Mat::from_fn(2, 2, |row, col| if row == col { 1.0 } else { 0.0 });
        let q = Mat::from_fn(
            2,
            2,
            |row, col| if row == col { 1.0 + row as f64 } else { 0.0 },
        );
        let r = Mat::from_fn(2, 2, |row, col| if row == col { 1.0 } else { 0.0 });
        let solve = solve_dare_dense(a.as_ref(), b.as_ref(), q.as_ref(), r.as_ref()).unwrap();

        let x11 = (1.44 + (1.44f64 * 1.44 + 4.0).sqrt()) / 2.0;
        let x22 = (1.25 + (1.25f64 * 1.25 + 8.0).sqrt()) / 2.0;
        let expected = Mat::from_fn(2, 2, |row, col| match (row, col) {
            (0, 0) => x11,
            (1, 1) => x22,
            _ => 0.0,
        });
        assert_close(&solve.solution, &expected, 1.0e-10);
        assert!(solve.stabilizing);
    }

    #[test]
    fn dare_handles_complex_scalar_case() {
        let a = Mat::from_fn(1, 1, |_, _| c64::new(0.5, 0.2));
        let b = Mat::from_fn(1, 1, |_, _| c64::new(1.0, 0.0));
        let q = Mat::from_fn(1, 1, |_, _| c64::new(1.0, 0.0));
        let r = Mat::from_fn(1, 1, |_, _| c64::new(1.0, 0.0));
        let solve = solve_dare_dense(a.as_ref(), b.as_ref(), q.as_ref(), r.as_ref()).unwrap();

        let aa = 0.5f64 * 0.5 + 0.2f64 * 0.2;
        let bcoef = 1.0 - aa - 1.0;
        let expected = (-bcoef + (bcoef * bcoef + 4.0).sqrt()) / 2.0;
        assert!((solve.solution[(0, 0)] - c64::new(expected, 0.0)).abs() < 1.0e-10);
        assert!(solve.stabilizing);
    }

    #[test]
    fn gain_helpers_match_solver_outputs() {
        let a = Mat::from_fn(1, 1, |_, _| 1.2f64);
        let b = Mat::from_fn(1, 1, |_, _| 1.0f64);
        let q = Mat::from_fn(1, 1, |_, _| 1.0f64);
        let r = Mat::from_fn(1, 1, |_, _| 1.0f64);

        let care = solve_care_dense(a.as_ref(), b.as_ref(), q.as_ref(), r.as_ref()).unwrap();
        let care_gain =
            care_gain_from_solution(b.as_ref(), r.as_ref(), care.solution.as_ref()).unwrap();
        assert_close(&care_gain, &care.gain, 1.0e-12);

        let dare = solve_dare_dense(a.as_ref(), b.as_ref(), q.as_ref(), r.as_ref()).unwrap();
        let dare_gain =
            dare_gain_from_solution(a.as_ref(), b.as_ref(), r.as_ref(), dare.solution.as_ref())
                .unwrap();
        assert_close(&dare_gain, &dare.gain, 1.0e-12);
    }

    #[test]
    fn riccati_rejects_dimension_mismatch() {
        let a = Mat::<f64>::zeros(2, 2);
        let b = Mat::<f64>::zeros(3, 1);
        let q = Mat::<f64>::zeros(2, 2);
        let r = Mat::<f64>::zeros(1, 1);
        let err = solve_care_dense(a.as_ref(), b.as_ref(), q.as_ref(), r.as_ref()).unwrap_err();
        assert!(matches!(err, RiccatiError::DimensionMismatch { .. }));
    }

    #[test]
    fn riccati_rejects_singular_r() {
        let a = Mat::from_fn(1, 1, |_, _| 1.0f64);
        let b = Mat::from_fn(1, 1, |_, _| 1.0f64);
        let q = Mat::from_fn(1, 1, |_, _| 1.0f64);
        let r = Mat::zeros(1, 1);
        let err = solve_care_dense(a.as_ref(), b.as_ref(), q.as_ref(), r.as_ref()).unwrap_err();
        assert!(matches!(
            err,
            RiccatiError::SingularControlWeight { which: "r" }
        ));
    }

    #[test]
    fn care_solution_is_hermitian() {
        let a = Mat::from_fn(2, 2, |row, col| match (row, col) {
            (0, 0) => c64::new(1.0, 1.0),
            (1, 1) => c64::new(-0.5, 0.3),
            _ => c64::new(0.0, 0.0),
        });
        let b = Mat::from_fn(2, 2, |row, col| {
            if row == col {
                c64::new(1.0, 0.0)
            } else {
                c64::new(0.0, 0.0)
            }
        });
        let q = Mat::from_fn(2, 2, |row, col| {
            if row == col {
                c64::new(1.0, 0.0)
            } else {
                c64::new(0.0, 0.0)
            }
        });
        let r = Mat::from_fn(2, 2, |row, col| {
            if row == col {
                c64::new(1.0, 0.0)
            } else {
                c64::new(0.0, 0.0)
            }
        });
        let solve = solve_care_dense(a.as_ref(), b.as_ref(), q.as_ref(), r.as_ref()).unwrap();

        for row in 0..solve.solution.nrows() {
            for col in 0..solve.solution.ncols() {
                assert!(
                    (solve.solution[(row, col)] - solve.solution[(col, row)].conj()).abs()
                        < 1.0e-10
                );
            }
        }
    }
}
