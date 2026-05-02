//! Dense and sparse continuous-time Lyapunov solves.
//!
//! This module serves two roles:
//!
//! - a direct dense reference solver for modest problem sizes
//! - a sparse low-rank ADI solver for Gramian-style control problems
//!
//! The dense path solves
//!
//! `A X + X A^H + Q = 0`
//!
//! by vectorizing the matrix equation into one dense linear system and solving
//! that system with `faer`'s full-pivoting LU factorization.
//!
//! The sparse path computes a low-rank factor `Z` such that
//!
//! `X ≈ Z Z^H`
//!
//! using the standard low-rank ADI iteration. That path is intentionally
//! conservative in its first form:
//!
//! - only user-provided shifts are supported
//! - shifts live in the same scalar field as the matrix
//! - shifted solves are performed through the staged sparse LU wrapper already
//!   used elsewhere in this crate
//!
//! This keeps the implementation numerically explicit and leaves room for
//! additional shift heuristics or promoted complex shifts without rewriting
//! the outer API.
//!
//! # Two Intuitions
//!
//! 1. **Energy view.** The Lyapunov equation measures how much state energy is
//!    reachable or observable in a stable continuous-time system.
//! 2. **Linear-system view.** It is also just a linear equation in the unknown
//!    entries of `X`; the dense solver literally treats it that way by
//!    vectorizing the matrix.
//!
//! # Glossary
//!
//! - **Gramian:** Positive semidefinite matrix summarizing reachability or
//!   observability energy.
//! - **LR-ADI:** Low-rank alternating-direction implicit iteration.
//! - **Residual factor:** Low-rank factor whose norm bounds the current
//!   Lyapunov residual.
//!
//! # Mathematical Formulation
//!
//! The core equation is:
//!
//! - `A X + X A^H + Q = 0`
//!
//! Dense solves vectorize this into a Kronecker-sum system. Sparse low-rank
//! solves seek `X ≈ Z Z^H` through LR-ADI.
//!
//! # Implementation Notes
//!
//! - Dense solves favor clarity and reference correctness over asymptotic
//!   optimality.
//! - Sparse solves require user-provided ADI shifts.
//! - The same solver surface underlies the controllability and observability
//!   Gramian helper entry points.

use super::vec_index;
use crate::control::dense_ops::{
    dense_mul_adjoint_lhs, dense_mul_adjoint_rhs, frobenius_norm, hermitian_project_in_place,
};
use crate::sparse::compensated::{CompensatedField, CompensatedSum};
use crate::sparse::{SparseLu, SparseLuError};
use alloc::vec::Vec;
use core::fmt;
use faer::linalg::lu::partial_pivoting::factor::PartialPivLuParams;
use faer::linalg::solvers::Solve;
use faer::sparse::linalg::lu::LuSymbolicParams;
use faer::sparse::{CreationError, FaerError, SparseColMat, SparseColMatRef, Triplet};
use faer::{Index, Mat, MatRef, Par, Spec, Unbind};
use faer_traits::ComplexField;
use faer_traits::Conjugate;
use faer_traits::ext::ComplexFieldExt;
use num_traits::Float;

/// Result of a dense continuous-time Lyapunov solve.
///
/// `solution` is the dense matrix `X` satisfying `A X + X A^H + Q = 0`.
/// `residual_norm` is the compensated Frobenius norm of the final residual.
#[derive(Clone, Debug)]
pub struct DenseLyapunovSolve<T: CompensatedField>
where
    T::Real: Float,
{
    /// Dense Lyapunov solution matrix.
    pub solution: Mat<T>,
    /// Compensated Frobenius norm of `A X + X A^H + Q`.
    pub residual_norm: T::Real,
}

/// Parameters controlling the sparse low-rank ADI solve.
///
/// The sparse solver uses the residual factor `W_k` supplied by LR-ADI. The
/// exact residual has the form `R_k = W_k W_k^H`, so the stopping test uses the
/// cheap upper bound `||R_k||_2 <= ||W_k||_F^2`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LyapunovParams<R> {
    /// Absolute tolerance applied to the residual upper bound `||W_k||_F^2`.
    pub tol: R,
    /// Maximum number of ADI steps to take.
    pub max_iters: usize,
}

impl<R: Float> Default for LyapunovParams<R> {
    fn default() -> Self {
        Self {
            tol: R::epsilon().sqrt(),
            max_iters: 32,
        }
    }
}

/// Shift-selection policy for the sparse low-rank ADI solver.
#[derive(Clone, Debug, PartialEq)]
pub enum ShiftStrategy<T> {
    /// Use these shifts in order, cycling if `max_iters` is larger.
    ///
    /// For the continuous-time equation, every shift must lie in the open left
    /// half-plane. Intuitively, each shift defines one sparse solve with
    /// `(A + p I)` that contracts the current residual factor.
    UserProvided(Vec<T>),
}

impl<T> ShiftStrategy<T> {
    /// Builds a user-provided shift strategy.
    #[must_use]
    pub fn user_provided(shifts: impl Into<Vec<T>>) -> Self {
        Self::UserProvided(shifts.into())
    }

    pub(crate) fn as_slice(&self) -> &[T] {
        match self {
            Self::UserProvided(shifts) => shifts.as_slice(),
        }
    }
}

/// Low-rank factor storing the approximation `X ≈ Z Z^H`.
#[derive(Clone, Debug)]
pub struct LowRankFactor<T: CompensatedField>
where
    T::Real: Float,
{
    /// Dense low-rank basis accumulated by ADI.
    pub z: Mat<T>,
}

impl<T: CompensatedField> LowRankFactor<T>
where
    T::Real: Float,
{
    /// Column count of the factor.
    #[must_use]
    pub fn rank(&self) -> usize {
        self.z.ncols()
    }

    /// Forms the explicit dense Gramian `Z Z^H`.
    ///
    /// This is intended for validation and small dense workflows. Large sparse
    /// control problems should generally keep the factor form.
    #[must_use]
    pub fn to_dense(&self) -> Mat<T> {
        dense_mul_adjoint_rhs(self.z.as_ref(), self.z.as_ref())
    }
}

/// Result of the sparse low-rank ADI solve.
#[derive(Clone, Debug)]
pub struct LowRankLyapunovSolve<T: CompensatedField>
where
    T::Real: Float,
{
    /// Final low-rank factor.
    pub factor: LowRankFactor<T>,
    /// Residual upper bound `||W_k||_F^2` derived from the ADI residual factor.
    pub residual_upper_bound: T::Real,
    /// Number of ADI steps taken.
    pub iterations: usize,
    /// Whether the residual upper bound met the requested tolerance.
    pub converged: bool,
}

/// Errors that can occur while building or solving Lyapunov systems.
#[derive(Clone, Copy, Debug)]
pub enum LyapunovError {
    /// The state matrix must be square.
    NonSquare {
        /// Actual row count.
        nrows: usize,
        /// Actual column count.
        ncols: usize,
    },
    /// A supplied matrix has incompatible dimensions.
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
    /// The dense solve produced a non-finite result or residual.
    SolveFailed,
    /// Sparse LR-ADI requires at least one shift.
    NoShifts,
    /// A user-provided shift is not in the open left half-plane.
    InvalidShift {
        /// Index of the offending shift in the supplied shift list.
        index: usize,
    },
    /// Sparse matrix creation failed.
    SparseBuild(CreationError),
    /// Sparse format conversion failed.
    SparseFormat(FaerError),
    /// The shifted sparse LU solve failed.
    SparseLu(SparseLuError),
}

impl fmt::Display for LyapunovError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

impl core::error::Error for LyapunovError {}

impl From<CreationError> for LyapunovError {
    fn from(value: CreationError) -> Self {
        Self::SparseBuild(value)
    }
}

impl From<FaerError> for LyapunovError {
    fn from(value: FaerError) -> Self {
        Self::SparseFormat(value)
    }
}

impl From<SparseLuError> for LyapunovError {
    fn from(value: SparseLuError) -> Self {
        Self::SparseLu(value)
    }
}

#[derive(Clone, Debug)]
struct ShiftedCscMatrix<I: Index, T> {
    matrix: SparseColMat<I, T>,
    base_values: Vec<T>,
    diag_positions: Vec<usize>,
}

impl<I: Index, T: ComplexField + Copy> ShiftedCscMatrix<I, T> {
    fn from_matrix<ViewT>(matrix: SparseColMatRef<'_, I, ViewT>) -> Result<Self, LyapunovError>
    where
        ViewT: Conjugate<Canonical = T>,
    {
        let matrix = matrix.canonical();
        let nrows = matrix.nrows().unbound();
        let ncols = matrix.ncols().unbound();
        let mut triplets: Vec<Triplet<I, I, T>> =
            Vec::with_capacity(matrix.row_idx().len() + nrows.min(ncols));

        // Missing diagonal entries are inserted once so every shifted operator
        // `(A + p I)` shares the exact same CSC symbolic pattern. That is what
        // allows symbolic LU reuse across all ADI shifts.
        for col in 0..ncols {
            let start = matrix.col_ptr()[col].zx();
            let end = matrix.col_ptr()[col + 1].zx();
            let mut has_diag = false;
            for idx in start..end {
                let row = matrix.row_idx()[idx].zx();
                has_diag |= row == col;
                triplets.push(Triplet::new(
                    I::truncate(row),
                    I::truncate(col),
                    matrix.val()[idx],
                ));
            }
            if col < nrows && !has_diag {
                triplets.push(Triplet::new(
                    I::truncate(col),
                    I::truncate(col),
                    T::zero_impl(),
                ));
            }
        }

        let matrix = SparseColMat::<I, T>::try_new_from_triplets(nrows, ncols, &triplets)?;
        let diag_positions = diagonal_positions(matrix.as_ref());
        let base_values = matrix.val().to_vec();
        Ok(Self {
            matrix,
            base_values,
            diag_positions,
        })
    }

    fn apply_shift(&mut self, shift: T) {
        let values = self.matrix.val_mut();
        debug_assert_eq!(values.len(), self.base_values.len());
        for idx in 0..values.len() {
            values[idx] = self.base_values[idx];
        }
        for &diag_idx in &self.diag_positions {
            values[diag_idx] += shift;
        }
    }

    fn as_ref(&self) -> SparseColMatRef<'_, I, T> {
        self.matrix.as_ref()
    }
}

/// Solves the dense continuous-time Lyapunov equation `A X + X A^H + Q = 0`.
///
/// This implementation is intended for modest problem sizes where explicitly
/// forming the dense `n^2 × n^2` Kronecker-sum system is acceptable. It uses
/// `faer`'s full-pivoting LU as a stability-oriented direct reference solve.
///
/// The returned matrix is projected back onto the Hermitian subspace with
/// `(X + X^H) / 2`, since exact Gramians are Hermitian and the direct solve can
/// pick up small asymmetry from finite precision.
pub fn solve_continuous_lyapunov_dense<T>(
    a: MatRef<'_, T>,
    q: MatRef<'_, T>,
) -> Result<DenseLyapunovSolve<T>, LyapunovError>
where
    T: CompensatedField,
    T::Real: Float,
{
    validate_square("a", a.nrows(), a.ncols())?;
    validate_dims("q", q.nrows(), q.ncols(), a.nrows(), a.ncols())?;

    let n = a.nrows();
    if n == 0 {
        return Ok(DenseLyapunovSolve {
            solution: Mat::zeros(0, 0),
            residual_norm: T::Real::zero(),
        });
    }

    let operator = build_continuous_operator(a);
    let rhs = Mat::from_fn(n * n, 1, |index, _| {
        let row = index % n;
        let col = index / n;
        -q[(row, col)]
    });

    let vectorized = operator.full_piv_lu().solve(rhs.as_ref());
    if !vectorized.as_ref().is_all_finite() {
        return Err(LyapunovError::SolveFailed);
    }

    let mut solution = unvectorize_square(vectorized.as_ref(), n);
    hermitian_project_in_place(&mut solution);

    let residual = continuous_residual(a, solution.as_ref(), q);
    let residual_norm = frobenius_norm(residual.as_ref());
    if !residual_norm.is_finite() {
        return Err(LyapunovError::SolveFailed);
    }

    Ok(DenseLyapunovSolve {
        solution,
        residual_norm,
    })
}

/// Computes a sparse low-rank controllability Gramian factor with LR-ADI.
///
/// For a stable sparse system `x' = A x + B u`, this computes a factor `Z`
/// such that `Wc ≈ Z Z^H` satisfies
///
/// `A Wc + Wc A^H + B B^H = 0`.
pub fn controllability_gramian_low_rank<I, T, ViewT>(
    a: SparseColMatRef<'_, I, ViewT>,
    b: MatRef<'_, T>,
    shifts: &ShiftStrategy<T>,
    params: LyapunovParams<T::Real>,
) -> Result<LowRankLyapunovSolve<T>, LyapunovError>
where
    I: Index,
    T: CompensatedField,
    T::Real: Float,
    ViewT: Conjugate<Canonical = T>,
{
    validate_square("a", a.nrows().unbound(), a.ncols().unbound())?;
    validate_dims("b", b.nrows(), b.ncols(), a.nrows().unbound(), b.ncols())?;
    low_rank_adi_core(a.canonical(), b, shifts, params)
}

/// Computes the dense continuous-time controllability Gramian.
///
/// For a stable system `x' = A x + B u`, the controllability Gramian solves
///
/// `A Wc + Wc A^H + B B^H = 0`
///
/// Intuitively, `B B^H` measures how strongly the inputs drive the state
/// variables, and the Gramian accumulates that effect through the stable
/// dynamics in `A`.
pub fn controllability_gramian_dense<T>(
    a: MatRef<'_, T>,
    b: MatRef<'_, T>,
) -> Result<DenseLyapunovSolve<T>, LyapunovError>
where
    T: CompensatedField,
    T::Real: Float,
{
    validate_square("a", a.nrows(), a.ncols())?;
    validate_dims("b", b.nrows(), b.ncols(), a.nrows(), b.ncols())?;

    let q = dense_mul_adjoint_rhs(b, b);
    solve_continuous_lyapunov_dense(a, q.as_ref())
}

/// Computes a sparse low-rank observability Gramian factor with LR-ADI.
///
/// This is the dual controllability solve on `(A^H, C^H)`, producing a factor
/// `Z` such that `Wo ≈ Z Z^H`.
pub fn observability_gramian_low_rank<I, T, ViewT>(
    a: SparseColMatRef<'_, I, ViewT>,
    c: MatRef<'_, T>,
    shifts: &ShiftStrategy<T>,
    params: LyapunovParams<T::Real>,
) -> Result<LowRankLyapunovSolve<T>, LyapunovError>
where
    I: Index,
    T: CompensatedField,
    T::Real: Float,
    ViewT: Conjugate<Canonical = T>,
{
    validate_square("a", a.nrows().unbound(), a.ncols().unbound())?;
    validate_dims("c", c.nrows(), c.ncols(), c.nrows(), a.ncols().unbound())?;

    let a_adjoint = a.adjoint().to_col_major()?;
    let c_adjoint = c.adjoint().to_owned();
    low_rank_adi_core(a_adjoint.as_ref(), c_adjoint.as_ref(), shifts, params)
}

/// Computes the dense continuous-time observability Gramian.
///
/// For a stable system `y = C x`, the observability Gramian solves
///
/// `A^H Wo + Wo A + C^H C = 0`
///
/// This is the dual controllability problem. The implementation uses the same
/// Lyapunov core by solving
///
/// `A_obs X + X A_obs^H + Q = 0`
///
/// with `A_obs = A^H` and `Q = C^H C`.
pub fn observability_gramian_dense<T>(
    a: MatRef<'_, T>,
    c: MatRef<'_, T>,
) -> Result<DenseLyapunovSolve<T>, LyapunovError>
where
    T: CompensatedField,
    T::Real: Float,
{
    validate_square("a", a.nrows(), a.ncols())?;
    validate_dims("c", c.nrows(), c.ncols(), c.nrows(), a.ncols())?;

    let q = dense_mul_adjoint_lhs(c, c);
    let a_adjoint = a.adjoint().to_owned();
    solve_continuous_lyapunov_dense(a_adjoint.as_ref(), q.as_ref())
}

fn low_rank_adi_core<I, T>(
    a: SparseColMatRef<'_, I, T>,
    b: MatRef<'_, T>,
    shifts: &ShiftStrategy<T>,
    params: LyapunovParams<T::Real>,
) -> Result<LowRankLyapunovSolve<T>, LyapunovError>
where
    I: Index,
    T: CompensatedField,
    T::Real: Float,
{
    let shifts = shifts.as_slice();
    if shifts.is_empty() {
        return Err(LyapunovError::NoShifts);
    }
    for (index, shift) in shifts.iter().enumerate() {
        if shift.real() >= T::Real::zero() {
            return Err(LyapunovError::InvalidShift { index });
        }
    }

    let n = a.nrows().unbound();
    let block_cols = b.ncols();
    let mut residual_factor = b.to_owned();
    let mut residual_upper_bound = residual_factor_norm_upper_bound(residual_factor.as_ref());
    if block_cols == 0 || residual_upper_bound <= params.tol {
        return Ok(LowRankLyapunovSolve {
            factor: LowRankFactor {
                z: Mat::zeros(n, 0),
            },
            residual_upper_bound,
            iterations: 0,
            converged: true,
        });
    }

    let mut z = Mat::<T>::zeros(n, block_cols * params.max_iters);
    let mut used_cols = 0usize;
    let mut shifted = ShiftedCscMatrix::from_matrix(a)?;
    let mut lu = SparseLu::<I, T>::analyze(shifted.as_ref(), LuSymbolicParams::default())?;

    // `W_k` always keeps the original RHS width, so the shifted sparse solve
    // width is fixed even though the low-rank factor `Z` grows every step.
    for iter in 0..params.max_iters {
        if residual_upper_bound <= params.tol {
            let mut z_final = z;
            z_final.resize_with(n, used_cols, |_, _| T::zero_impl());
            return Ok(LowRankLyapunovSolve {
                factor: LowRankFactor { z: z_final },
                residual_upper_bound,
                iterations: iter,
                converged: true,
            });
        }

        let shift = shifts[iter % shifts.len()];
        shifted.apply_shift(shift);
        lu.refactor(
            shifted.as_ref(),
            Par::Seq,
            Spec::<PartialPivLuParams, T>::default(),
        )?;

        let mut v = residual_factor.to_owned();
        lu.solve_in_place(v.as_mut(), Par::Seq)?;

        let append_scale = (-(shift.real() + shift.real())).sqrt();
        for col in 0..block_cols {
            for row in 0..n {
                z[(row, used_cols + col)] = v[(row, col)].mul_real(append_scale);
            }
        }
        used_cols += block_cols;

        let residual_scale = -(shift.real() + shift.real());
        for col in 0..block_cols {
            for row in 0..n {
                residual_factor[(row, col)] += v[(row, col)].mul_real(residual_scale);
            }
        }

        residual_upper_bound = residual_factor_norm_upper_bound(residual_factor.as_ref());
    }

    let mut z_final = z;
    z_final.resize_with(n, used_cols, |_, _| T::zero_impl());
    Ok(LowRankLyapunovSolve {
        factor: LowRankFactor { z: z_final },
        residual_upper_bound,
        iterations: params.max_iters,
        converged: residual_upper_bound <= params.tol,
    })
}

fn validate_square(which: &'static str, nrows: usize, ncols: usize) -> Result<(), LyapunovError> {
    if nrows != ncols {
        if which == "a" {
            return Err(LyapunovError::NonSquare { nrows, ncols });
        }
        return Err(LyapunovError::DimensionMismatch {
            which,
            expected_nrows: ncols,
            expected_ncols: ncols,
            actual_nrows: nrows,
            actual_ncols: ncols,
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
) -> Result<(), LyapunovError> {
    if actual_nrows != expected_nrows || actual_ncols != expected_ncols {
        return Err(LyapunovError::DimensionMismatch {
            which,
            expected_nrows,
            expected_ncols,
            actual_nrows,
            actual_ncols,
        });
    }
    Ok(())
}

fn diagonal_positions<I: Index, T>(matrix: SparseColMatRef<'_, I, T>) -> Vec<usize> {
    let n = matrix.nrows().unbound().min(matrix.ncols().unbound());
    let mut positions = Vec::with_capacity(n);
    for col in 0..n {
        let start = matrix.col_ptr()[col].zx();
        let end = matrix.col_ptr()[col + 1].zx();
        let mut found = None;
        for idx in start..end {
            if matrix.row_idx()[idx].zx() == col {
                found = Some(idx);
                break;
            }
        }
        positions.push(found.expect("shifted matrix must contain an explicit diagonal"));
    }
    positions
}

fn build_continuous_operator<T>(a: MatRef<'_, T>) -> Mat<T>
where
    T: ComplexField + Copy,
{
    let n = a.nrows();
    let mut operator = Mat::<T>::zeros(n * n, n * n);

    // Column-major vectorization turns `A X + X A^H` into
    // `(I ⊗ A + conj(A) ⊗ I) vec(X)`.
    for col in 0..n {
        for row in 0..n {
            let eq = vec_index(row, col, n);
            for k in 0..n {
                operator[(eq, vec_index(k, col, n))] += a[(row, k)];
                operator[(eq, vec_index(row, k, n))] += a[(col, k)].conj();
            }
        }
    }

    operator
}

fn unvectorize_square<T: ComplexField + Copy>(values: MatRef<'_, T>, n: usize) -> Mat<T> {
    Mat::from_fn(n, n, |row, col| values[(vec_index(row, col, n), 0)])
}

fn continuous_residual<T>(a: MatRef<'_, T>, x: MatRef<'_, T>, q: MatRef<'_, T>) -> Mat<T>
where
    T: CompensatedField,
    T::Real: Float,
{
    let n = a.nrows();
    Mat::from_fn(n, n, |row, col| {
        let mut acc = CompensatedSum::<T>::default();
        for k in 0..n {
            acc.add(a[(row, k)] * x[(k, col)]);
        }
        for k in 0..n {
            acc.add(x[(row, k)] * a[(col, k)].conj());
        }
        acc.add(q[(row, col)]);
        acc.finish()
    })
}

fn residual_factor_norm_upper_bound<T>(factor: MatRef<'_, T>) -> T::Real
where
    T: CompensatedField,
    T::Real: Float,
{
    let w_norm = frobenius_norm(factor);
    w_norm * w_norm
}

#[cfg(test)]
mod test {
    use super::{
        LowRankFactor, LyapunovError, LyapunovParams, ShiftStrategy, continuous_residual,
        controllability_gramian_dense, controllability_gramian_low_rank,
        observability_gramian_dense, observability_gramian_low_rank,
        solve_continuous_lyapunov_dense,
    };
    use crate::control::dense_ops::{dense_mul_adjoint_lhs, dense_mul_adjoint_rhs, frobenius_norm};
    use faer::sparse::{SparseColMat, Triplet};
    use faer::{Mat, c64};
    use faer_traits::ext::ComplexFieldExt;

    fn diagonal_solution_from_q<T>(diag: &[T], q: &Mat<T>) -> Mat<T>
    where
        T: super::CompensatedField,
        T::Real: num_traits::Float,
    {
        Mat::from_fn(diag.len(), diag.len(), |row, col| {
            -q[(row, col)] / (diag[row] + diag[col].conj())
        })
    }

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

    fn assert_factor_close<T>(factor: &LowRankFactor<T>, expected: &Mat<T>, tol: T::Real)
    where
        T: super::CompensatedField,
        T::Real: num_traits::Float,
    {
        let dense = factor.to_dense();
        assert_close(&dense, expected, tol);
    }

    #[test]
    fn dense_continuous_lyapunov_matches_closed_form_real_diagonal_case() {
        let diag = [-1.0f64, -2.0];
        let a = Mat::from_fn(2, 2, |row, col| if row == col { diag[row] } else { 0.0 });
        let q = Mat::from_fn(2, 2, |row, col| match (row, col) {
            (0, 0) => 2.0,
            (0, 1) => -1.0,
            (1, 0) => -1.0,
            _ => 4.0,
        });

        let solve = solve_continuous_lyapunov_dense(a.as_ref(), q.as_ref()).unwrap();
        let expected = diagonal_solution_from_q(&diag, &q);
        assert_close(&solve.solution, &expected, 1.0e-12);
        assert!(solve.residual_norm <= 1.0e-12);
    }

    #[test]
    fn controllability_gramian_matches_closed_form_complex_diagonal_case() {
        let diag = [c64::new(-1.0, 2.0), c64::new(-3.0, -1.0)];
        let a = Mat::from_fn(2, 2, |row, col| {
            if row == col {
                diag[row]
            } else {
                c64::new(0.0, 0.0)
            }
        });
        let b = Mat::from_fn(2, 2, |row, col| match (row, col) {
            (0, 0) => c64::new(1.0, -1.0),
            (0, 1) => c64::new(2.0, 0.5),
            (1, 0) => c64::new(-0.5, 1.0),
            _ => c64::new(1.5, -2.0),
        });

        let q = dense_mul_adjoint_rhs(b.as_ref(), b.as_ref());
        let expected = diagonal_solution_from_q(&diag, &q);
        let solve = controllability_gramian_dense(a.as_ref(), b.as_ref()).unwrap();

        assert_close(&solve.solution, &expected, 1.0e-11);
        assert!(solve.residual_norm <= 1.0e-11);
        for col in 0..solve.solution.ncols() {
            for row in 0..solve.solution.nrows() {
                assert!(
                    (solve.solution[(row, col)] - solve.solution[(col, row)].conj()).abs1()
                        <= 1.0e-11
                );
            }
        }
    }

    #[test]
    fn observability_gramian_matches_closed_form_complex_diagonal_case() {
        let diag = [c64::new(-0.5, 0.75), c64::new(-2.0, -1.5)];
        let a = Mat::from_fn(2, 2, |row, col| {
            if row == col {
                diag[row]
            } else {
                c64::new(0.0, 0.0)
            }
        });
        let c = Mat::from_fn(3, 2, |row, col| match (row, col) {
            (0, 0) => c64::new(1.0, 0.25),
            (0, 1) => c64::new(-0.5, 2.0),
            (1, 0) => c64::new(0.0, -1.0),
            (1, 1) => c64::new(1.5, 0.5),
            (2, 0) => c64::new(-2.0, 1.0),
            _ => c64::new(0.25, -0.75),
        });

        let q = dense_mul_adjoint_lhs(c.as_ref(), c.as_ref());
        let expected = diagonal_solution_from_q(&[diag[0].conj(), diag[1].conj()], &q);
        let solve = observability_gramian_dense(a.as_ref(), c.as_ref()).unwrap();

        assert_close(&solve.solution, &expected, 1.0e-11);
        assert!(solve.residual_norm <= 1.0e-11);
    }

    #[test]
    fn sparse_controllability_low_rank_matches_dense_reference() {
        let a = SparseColMat::<usize, f64>::try_new_from_triplets(
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
        let shifts = ShiftStrategy::user_provided(vec![-0.5, -1.0, -2.0, -4.0]);
        let params = LyapunovParams {
            tol: 1.0e-10,
            max_iters: 24,
        };

        let sparse =
            controllability_gramian_low_rank(a.as_ref(), b.as_ref(), &shifts, params).unwrap();
        let a_dense = a.as_ref().to_dense();
        let dense = controllability_gramian_dense(a_dense.as_ref(), b.as_ref()).unwrap();

        assert!(
            sparse.converged,
            "residual_upper_bound={:?}",
            sparse.residual_upper_bound
        );
        assert!(sparse.residual_upper_bound <= 1.0e-10);
        assert_factor_close(&sparse.factor, &dense.solution, 2.0e-8);
    }

    #[test]
    fn sparse_observability_low_rank_matches_dense_reference() {
        let a = SparseColMat::<usize, f64>::try_new_from_triplets(
            3,
            3,
            &[
                Triplet::new(0, 0, -3.0),
                Triplet::new(1, 0, 0.25),
                Triplet::new(0, 1, -0.5),
                Triplet::new(1, 1, -1.0),
                Triplet::new(2, 1, 0.1),
                Triplet::new(1, 2, 0.35),
                Triplet::new(2, 2, -2.5),
            ],
        )
        .unwrap();
        let c = Mat::from_fn(2, 3, |row, col| match (row, col) {
            (0, 0) => 1.0,
            (0, 1) => -0.75,
            (0, 2) => 0.25,
            (1, 0) => -0.5,
            (1, 1) => 0.0,
            _ => 1.25,
        });
        let shifts = ShiftStrategy::user_provided(vec![-0.25, -0.75, -1.5, -3.0]);
        let params = LyapunovParams {
            tol: 1.0e-10,
            max_iters: 24,
        };

        let sparse =
            observability_gramian_low_rank(a.as_ref(), c.as_ref(), &shifts, params).unwrap();
        let a_dense = a.as_ref().to_dense();
        let dense = observability_gramian_dense(a_dense.as_ref(), c.as_ref()).unwrap();

        assert!(
            sparse.converged,
            "residual_upper_bound={:?}",
            sparse.residual_upper_bound
        );
        assert!(sparse.residual_upper_bound <= 1.0e-10);
        assert_factor_close(&sparse.factor, &dense.solution, 2.0e-8);
    }

    #[test]
    fn sparse_controllability_handles_complex_system() {
        let a = SparseColMat::<usize, c64>::try_new_from_triplets(
            2,
            2,
            &[
                Triplet::new(0, 0, c64::new(-1.0, 0.5)),
                Triplet::new(1, 0, c64::new(0.1, -0.2)),
                Triplet::new(0, 1, c64::new(-0.25, 0.05)),
                Triplet::new(1, 1, c64::new(-2.0, -0.75)),
            ],
        )
        .unwrap();
        let b = Mat::from_fn(2, 1, |row, _| match row {
            0 => c64::new(1.0, -0.5),
            _ => c64::new(-0.25, 0.75),
        });
        let shifts = ShiftStrategy::user_provided(vec![c64::new(-0.75, 0.0), c64::new(-2.5, 0.0)]);
        let params = LyapunovParams {
            tol: 1.0e-10,
            max_iters: 20,
        };

        let sparse =
            controllability_gramian_low_rank(a.as_ref(), b.as_ref(), &shifts, params).unwrap();
        let a_dense = a.as_ref().to_dense();
        let dense = controllability_gramian_dense(a_dense.as_ref(), b.as_ref()).unwrap();

        assert!(
            sparse.converged,
            "residual_upper_bound={:?}",
            sparse.residual_upper_bound
        );
        assert_factor_close(&sparse.factor, &dense.solution, 5.0e-8);
    }

    #[test]
    fn sparse_low_rank_rejects_non_left_half_plane_shift() {
        let a =
            SparseColMat::<usize, f64>::try_new_from_triplets(1, 1, &[Triplet::new(0, 0, -1.0)])
                .unwrap();
        let b = Mat::from_fn(1, 1, |_, _| 1.0);
        let err = controllability_gramian_low_rank(
            a.as_ref(),
            b.as_ref(),
            &ShiftStrategy::user_provided(vec![0.25]),
            LyapunovParams {
                tol: 1.0e-12,
                max_iters: 1,
            },
        )
        .unwrap_err();
        assert!(matches!(err, LyapunovError::InvalidShift { index: 0 }));
    }

    #[test]
    fn residual_recomputation_is_small_for_nondiagonal_real_case() {
        let a = Mat::from_fn(3, 3, |row, col| match (row, col) {
            (0, 0) => -2.0,
            (0, 1) => 0.5,
            (0, 2) => 0.0,
            (1, 0) => -0.25,
            (1, 1) => -1.5,
            (1, 2) => 0.75,
            (2, 0) => 0.0,
            (2, 1) => -0.4,
            _ => -0.8,
        });
        let q = Mat::from_fn(3, 3, |row, col| match (row, col) {
            (0, 0) => 3.0,
            (0, 1) => -0.5,
            (0, 2) => 0.25,
            (1, 0) => -0.5,
            (1, 1) => 2.0,
            (1, 2) => -0.3,
            (2, 0) => 0.25,
            (2, 1) => -0.3,
            _ => 1.0,
        });

        let solve = solve_continuous_lyapunov_dense(a.as_ref(), q.as_ref()).unwrap();
        let residual = continuous_residual(a.as_ref(), solve.solution.as_ref(), q.as_ref());
        let residual_norm = frobenius_norm(residual.as_ref());
        assert!(residual_norm <= 1.0e-11);
    }

    #[test]
    fn rejects_dimension_mismatches() {
        let a = Mat::<f64>::identity(2, 2);
        let q = Mat::<f64>::identity(3, 3);
        let err = solve_continuous_lyapunov_dense(a.as_ref(), q.as_ref()).unwrap_err();
        assert!(matches!(
            err,
            LyapunovError::DimensionMismatch {
                which: "q",
                expected_nrows: 2,
                expected_ncols: 2,
                actual_nrows: 3,
                actual_ncols: 3,
            }
        ));
    }
}
