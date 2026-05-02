//! Dense discrete-time Stein solves and discrete Gramians.
//!
//! This module provides the discrete-time analogue of the dense Lyapunov
//! helpers in [`super::lyapunov`]. For a stable discrete-time system, the
//! controllability and observability Gramians solve Stein equations of the form
//!
//! `X - A X A^H = Q`
//!
//! rather than the continuous-time Lyapunov form `A X + X A^H + Q = 0`.
//!
//! The implementation is intentionally direct and numerically explicit:
//! it vectorizes the matrix equation into a dense linear system
//!
//! `(I - conj(A) ⊗ A) vec(X) = vec(Q)`
//!
//! and solves that system with `faer`'s full-pivoting LU factorization.
//! That is not the asymptotically best dense algorithm, but it is a reliable
//! reference path that fits the control module cleanly and unlocks
//! dense discrete Gramians for balanced truncation work.
//!
//! # Two Intuitions
//!
//! 1. **Sampled-energy view.** Stein equations play the same role for
//!    discrete-time systems that Lyapunov equations play for continuous-time
//!    systems.
//! 2. **Lifted-linear-system view.** The dense path again treats the matrix
//!    equation as an ordinary linear system after vectorization.
//!
//! # Glossary
//!
//! - **Cayley transform:** Map used here to reuse continuous-time LR-ADI ideas
//!   on a discrete-time problem.
//! - **Stein equation:** Discrete-time Gramian equation `X - A X A^H = Q`.
//!
//! # Mathematical Formulation
//!
//! The dense reference problem is:
//!
//! - `X - A X A^H = Q`
//!
//! The sparse low-rank path maps the discrete equation to a continuous-time
//! Lyapunov problem via a Cayley transform, then reuses low-rank ADI.
//!
//! # Implementation Notes
//!
//! - Dense solves are exact reference-style solves for modest problem sizes.
//! - Sparse solves inherit the shift policy from the continuous low-rank
//!   Lyapunov implementation after the Cayley map.
//! - The API mirrors the Lyapunov module on purpose so dense/sparse Gramian
//!   workflows line up across time domains.

use super::lyapunov::{LowRankLyapunovSolve, LyapunovParams, ShiftStrategy};
use super::vec_index;
use crate::control::dense_ops::{
    dense_mul, dense_mul_adjoint_lhs, dense_mul_adjoint_rhs, frobenius_norm,
    hermitian_project_in_place,
};
use crate::sparse::SparseLuError;
use crate::sparse::compensated::{CompensatedField, CompensatedSum};
use crate::sparse::lu::SparseLu;
use alloc::vec::Vec;
use core::fmt;
use faer::Index;
use faer::linalg::lu::partial_pivoting::factor::PartialPivLuParams;
use faer::linalg::solvers::Solve;
use faer::sparse::linalg::lu::LuSymbolicParams;
use faer::sparse::{CreationError, FaerError, SparseColMat, SparseColMatRef, Triplet};
use faer::{Mat, MatRef, Par, Spec, Unbind};
use faer_traits::ComplexField;
use faer_traits::Conjugate;
use faer_traits::ext::ComplexFieldExt;
use num_traits::{Float, One, Zero};

/// Result of a dense discrete-time Stein solve.
///
/// `solution` is the dense matrix `X` satisfying `X - A X A^H = Q`.
/// `residual_norm` is the compensated Frobenius norm of the final residual.
#[derive(Clone, Debug)]
pub struct DenseSteinSolve<T: CompensatedField>
where
    T::Real: Float,
{
    /// Dense Stein solution matrix.
    pub solution: Mat<T>,
    /// Compensated Frobenius norm of `X - A X A^H - Q`.
    pub residual_norm: T::Real,
}

/// Errors that can occur while building or solving dense Stein systems.
#[derive(Debug)]
pub enum SteinError {
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
    /// A user-provided ADI shift is not in the open left half-plane.
    ///
    /// For the sparse discrete solver, these shifts belong to the transformed
    /// continuous-time Lyapunov problem after the Cayley map, not directly to
    /// the original discrete-time spectrum.
    InvalidShift {
        /// Index of the offending shift in the supplied shift list.
        index: usize,
    },
    /// The Cayley transform requires `(A + I)` to be nonsingular.
    ///
    /// If `A` has an eigenvalue at or very near `-1`, the transform becomes
    /// unusable numerically and the sparse discrete path rejects it here.
    SingularTransform,
    /// Sparse matrix creation failed.
    SparseBuild(CreationError),
    /// Sparse format conversion failed.
    SparseFormat(FaerError),
    /// Sparse LU factorization or solve failed.
    SparseLu(SparseLuError),
}

impl fmt::Display for SteinError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

impl core::error::Error for SteinError {}

impl From<CreationError> for SteinError {
    fn from(value: CreationError) -> Self {
        Self::SparseBuild(value)
    }
}

impl From<FaerError> for SteinError {
    fn from(value: FaerError) -> Self {
        Self::SparseFormat(value)
    }
}

impl From<SparseLuError> for SteinError {
    fn from(value: SparseLuError) -> Self {
        Self::SparseLu(value)
    }
}

// This wrapper keeps a sparse CSC pattern fixed while varying only the affine
// combination `alpha * A + beta * I`. That is exactly the structure needed by
// the transformed shifted solves in the sparse discrete ADI backend, and it
// lets symbolic LU structure be reused across every shift.
#[derive(Clone, Debug)]
struct AffineCscMatrix<I: Index, T> {
    matrix: SparseColMat<I, T>,
    base_values: Vec<T>,
    diag_positions: Vec<usize>,
}

impl<I: Index, T: ComplexField + Copy> AffineCscMatrix<I, T> {
    fn from_matrix<ViewT>(matrix: SparseColMatRef<'_, I, ViewT>) -> Result<Self, SteinError>
    where
        ViewT: Conjugate<Canonical = T>,
    {
        let matrix = matrix.canonical();
        let nrows = matrix.nrows().unbound();
        let ncols = matrix.ncols().unbound();
        let mut triplets: Vec<Triplet<I, I, T>> =
            Vec::with_capacity(matrix.row_idx().len() + nrows.min(ncols));

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

    fn apply_affine(&mut self, alpha: T, beta: T) {
        let values = self.matrix.val_mut();
        for (dst, &src) in values.iter_mut().zip(self.base_values.iter()) {
            *dst = alpha * src;
        }
        for &diag_idx in &self.diag_positions {
            values[diag_idx] += beta;
        }
    }

    fn as_ref(&self) -> SparseColMatRef<'_, I, T> {
        self.matrix.as_ref()
    }
}

/// Solves the dense discrete-time Stein equation `X - A X A^H = Q`.
///
/// This is the dense direct reference path for modest problem sizes where
/// explicitly building the `n^2 × n^2` Kronecker system is acceptable. The
/// result is projected back onto the Hermitian subspace with `(X + X^H) / 2`,
/// since exact discrete Gramians are Hermitian and the direct solve can pick up
/// small asymmetry from finite precision.
///
/// Compared with the continuous-time Lyapunov equation, the sign pattern
/// changes because the discrete dynamics accumulate through repeated powers of
/// the one-step transition matrix. That is why the dense operator here is
/// `I - conj(A) ⊗ A` rather than a Kronecker sum.
pub fn solve_discrete_stein_dense<T>(
    a: MatRef<'_, T>,
    q: MatRef<'_, T>,
) -> Result<DenseSteinSolve<T>, SteinError>
where
    T: CompensatedField,
    T::Real: Float,
{
    validate_square("a", a.nrows(), a.ncols())?;
    validate_dims("q", q.nrows(), q.ncols(), a.nrows(), a.ncols())?;

    let n = a.nrows();
    if n == 0 {
        return Ok(DenseSteinSolve {
            solution: Mat::zeros(0, 0),
            residual_norm: <T::Real as Zero>::zero(),
        });
    }

    let operator = build_discrete_operator(a);
    let rhs = Mat::from_fn(n * n, 1, |index, _| {
        let row = index % n;
        let col = index / n;
        q[(row, col)]
    });

    let vectorized = operator.full_piv_lu().solve(rhs.as_ref());
    if !vectorized.as_ref().is_all_finite() {
        return Err(SteinError::SolveFailed);
    }

    let mut solution = unvectorize_square(vectorized.as_ref(), n);
    // Exact discrete Gramians are Hermitian. This projection removes the small
    // skew-Hermitian component introduced by the finite-precision dense solve
    // without changing the intended solution materially.
    hermitian_project_in_place(&mut solution);

    let residual = discrete_residual(a, solution.as_ref(), q);
    let residual_norm = frobenius_norm(residual.as_ref());
    if !residual_norm.is_finite() {
        return Err(SteinError::SolveFailed);
    }

    Ok(DenseSteinSolve {
        solution,
        residual_norm,
    })
}

/// Computes the dense discrete-time controllability Gramian.
///
/// For a stable system `x[k + 1] = A x[k] + B u[k]`, the controllability
/// Gramian satisfies
///
/// `Wc - A Wc A^H = B B^H`
///
/// Intuitively, `B B^H` measures how the inputs inject energy into the state,
/// and the Stein solve accumulates that effect across repeated applications of
/// the one-step transition `A`.
///
/// This is the discrete-time analogue of the continuous controllability
/// Gramian, with `B B^H` acting as the per-step input energy injection term.
pub fn controllability_gramian_discrete_dense<T>(
    a: MatRef<'_, T>,
    b: MatRef<'_, T>,
) -> Result<DenseSteinSolve<T>, SteinError>
where
    T: CompensatedField,
    T::Real: Float,
{
    validate_square("a", a.nrows(), a.ncols())?;
    validate_dims("b", b.nrows(), b.ncols(), a.nrows(), b.ncols())?;

    // `B B^H` is the natural Gramian forcing term in the controllability
    // equation. It stays Hermitian and positive semidefinite in exact
    // arithmetic even for complex-valued systems.
    let q = dense_mul_adjoint_rhs(b, b);
    solve_discrete_stein_dense(a, q.as_ref())
}

/// Computes the dense discrete-time observability Gramian.
///
/// For a stable system `y[k] = C x[k]`, the observability Gramian satisfies
///
/// `Wo - A^H Wo A = C^H C`
///
/// This is the dual controllability problem. The implementation reuses the same
/// Stein core by solving with `A_obs = A^H` and `Q = C^H C`.
///
/// Writing the observability equation this way keeps one dense Stein solver at
/// the center of the implementation instead of maintaining two nearly
/// identical direct paths.
pub fn observability_gramian_discrete_dense<T>(
    a: MatRef<'_, T>,
    c: MatRef<'_, T>,
) -> Result<DenseSteinSolve<T>, SteinError>
where
    T: CompensatedField,
    T::Real: Float,
{
    validate_square("a", a.nrows(), a.ncols())?;
    validate_dims("c", c.nrows(), c.ncols(), c.nrows(), a.ncols())?;

    // `C^H C` is the dual forcing term: it measures how strongly each state
    // direction is exposed through the outputs.
    let q = dense_mul_adjoint_lhs(c, c);
    let a_adjoint = a.adjoint().to_owned();
    solve_discrete_stein_dense(a_adjoint.as_ref(), q.as_ref())
}

/// Computes a sparse low-rank discrete-time controllability Gramian factor.
///
/// For a stable sparse system `x[k + 1] = A x[k] + B u[k]`, this computes a
/// factor `Z` such that `Wc ≈ Z Z^H` satisfies
///
/// `Wc - A Wc A^H = B B^H`.
///
/// The implementation uses a Cayley transform to map the discrete Stein
/// equation to a continuous-time Lyapunov equation, then runs the same style of
/// low-rank ADI iteration used by the sparse continuous solver.
///
/// The `shifts` parameter therefore refers to ADI shifts for the transformed
/// continuous operator, not directly to the original discrete-time matrix `A`.
/// This keeps the sparse discrete path aligned with the existing Lyapunov ADI
/// machinery instead of introducing a second shift policy.
pub fn controllability_gramian_discrete_low_rank<I, T, ViewT>(
    a: SparseColMatRef<'_, I, ViewT>,
    b: MatRef<'_, T>,
    shifts: &ShiftStrategy<T>,
    params: LyapunovParams<T::Real>,
) -> Result<LowRankLyapunovSolve<T>, SteinError>
where
    I: Index,
    T: CompensatedField,
    T::Real: Float,
    ViewT: Conjugate<Canonical = T>,
{
    validate_square("a", a.nrows().unbound(), a.ncols().unbound())?;
    validate_dims("b", b.nrows(), b.ncols(), a.nrows().unbound(), b.ncols())?;
    low_rank_discrete_core(a.canonical(), b, shifts, params)
}

/// Computes a sparse low-rank discrete-time observability Gramian factor.
///
/// This is the dual controllability solve on `(A^H, C^H)`, producing a factor
/// `Z` such that `Wo ≈ Z Z^H`.
///
/// As in the controllability path, the user-provided shifts live in the
/// transformed continuous-time ADI problem after the Cayley map.
pub fn observability_gramian_discrete_low_rank<I, T, ViewT>(
    a: SparseColMatRef<'_, I, ViewT>,
    c: MatRef<'_, T>,
    shifts: &ShiftStrategy<T>,
    params: LyapunovParams<T::Real>,
) -> Result<LowRankLyapunovSolve<T>, SteinError>
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
    low_rank_discrete_core(a_adjoint.as_ref(), c_adjoint.as_ref(), shifts, params)
}

fn low_rank_discrete_core<I, T>(
    a: SparseColMatRef<'_, I, T>,
    b: MatRef<'_, T>,
    shifts: &ShiftStrategy<T>,
    params: LyapunovParams<T::Real>,
) -> Result<LowRankLyapunovSolve<T>, SteinError>
where
    I: Index,
    T: CompensatedField,
    T::Real: Float,
{
    let shifts = shifts.as_slice();
    if shifts.is_empty() {
        return Err(SteinError::NoShifts);
    }
    for (index, &shift) in shifts.iter().enumerate() {
        if shift.real() >= <T::Real as Zero>::zero() {
            return Err(SteinError::InvalidShift { index });
        }
    }

    let n = a.nrows().unbound();
    let block_cols = b.ncols();

    if block_cols == 0 {
        return Ok(LowRankLyapunovSolve {
            factor: super::lyapunov::LowRankFactor {
                z: Mat::zeros(n, 0),
            },
            residual_upper_bound: <T::Real as Zero>::zero(),
            iterations: 0,
            converged: true,
        });
    }

    // The transformed operator is expressed through `(A + I)` and affine
    // shifted systems built from `A`. We never form the dense Cayley operator
    // `Ac = (A - I)(A + I)^(-1)` explicitly.
    let mut plus_identity = AffineCscMatrix::from_matrix(a)?;
    plus_identity.apply_affine(T::one_impl(), T::one_impl());
    let plus_lu = SparseLu::<I, T>::factorize(
        plus_identity.as_ref(),
        Par::Seq,
        LuSymbolicParams::default(),
        Spec::<PartialPivLuParams, T>::default(),
    )
    .map_err(map_transform_error)?;

    let sqrt_two = (<T::Real as One>::one() + <T::Real as One>::one()).sqrt();
    // The Cayley transform maps the discrete Stein equation to a continuous
    // Lyapunov equation with `Bc = sqrt(2) (A + I)^(-1) B`.
    let mut residual_factor =
        solve_block_with_lu(&plus_lu, b.to_owned(), Par::Seq).map_err(map_transform_error)?;
    if !residual_factor.as_ref().is_all_finite() {
        return Err(SteinError::SingularTransform);
    }
    scale_block_in_place(&mut residual_factor, sqrt_two);

    let mut residual_upper_bound = residual_factor_norm_upper_bound(residual_factor.as_ref());
    if !residual_upper_bound.is_finite() {
        return Err(SteinError::SingularTransform);
    }
    if residual_upper_bound <= params.tol {
        return Ok(LowRankLyapunovSolve {
            factor: super::lyapunov::LowRankFactor {
                z: Mat::zeros(n, 0),
            },
            residual_upper_bound,
            iterations: 0,
            converged: true,
        });
    }

    let mut z = Mat::<T>::zeros(n, block_cols * params.max_iters);
    let mut used_cols = 0usize;
    let mut shifted = AffineCscMatrix::from_matrix(a)?;
    shifted.apply_affine(T::one_impl() + shifts[0], shifts[0] - T::one_impl());
    let mut shifted_lu = SparseLu::<I, T>::analyze(shifted.as_ref(), LuSymbolicParams::default())?;

    for iter in 0..params.max_iters {
        if residual_upper_bound <= params.tol {
            let mut z_final = z;
            z_final.resize_with(n, used_cols, |_, _| T::zero_impl());
            return Ok(LowRankLyapunovSolve {
                factor: super::lyapunov::LowRankFactor { z: z_final },
                residual_upper_bound,
                iterations: iter,
                converged: true,
            });
        }

        let shift = shifts[iter % shifts.len()];
        shifted.apply_affine(T::one_impl() + shift, shift - T::one_impl());
        shifted_lu.refactor(
            shifted.as_ref(),
            Par::Seq,
            Spec::<PartialPivLuParams, T>::default(),
        )?;

        // `(Ac + p I)^(-1)` can be applied without forming `Ac` explicitly:
        // solve with `((1 + p) A + (p - 1) I)`, then multiply by `(A + I)`.
        let y = solve_block_with_lu(&shifted_lu, residual_factor.to_owned(), Par::Seq)?;
        if !y.as_ref().is_all_finite() {
            return Err(SteinError::SolveFailed);
        }
        let v = sparse_matmul_dense(plus_identity.as_ref(), y.as_ref());
        if !v.as_ref().is_all_finite() {
            return Err(SteinError::SolveFailed);
        }

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
        if !residual_upper_bound.is_finite() {
            return Err(SteinError::SolveFailed);
        }
    }

    let mut z_final = z;
    z_final.resize_with(n, used_cols, |_, _| T::zero_impl());
    Ok(LowRankLyapunovSolve {
        factor: super::lyapunov::LowRankFactor { z: z_final },
        residual_upper_bound,
        iterations: params.max_iters,
        converged: residual_upper_bound <= params.tol,
    })
}

fn map_transform_error(err: SparseLuError) -> SteinError {
    match err {
        // LU numeric failure on `(A + I)` or an affine shifted system is the
        // operational signature of a bad Cayley transform in this backend.
        SparseLuError::Numeric(_) => SteinError::SingularTransform,
        other => SteinError::SparseLu(other),
    }
}

fn solve_block_with_lu<I, T>(
    lu: &SparseLu<I, T>,
    mut rhs: Mat<T>,
    par: Par,
) -> Result<Mat<T>, SparseLuError>
where
    I: Index,
    T: ComplexField,
{
    lu.solve_in_place(rhs.as_mut(), par)?;
    Ok(rhs)
}

fn scale_block_in_place<T>(matrix: &mut Mat<T>, scale: T::Real)
where
    T: ComplexField + Copy,
{
    for col in 0..matrix.ncols() {
        for row in 0..matrix.nrows() {
            matrix[(row, col)] = matrix[(row, col)].mul_real(&scale);
        }
    }
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
        positions.push(found.expect("affine matrix must contain an explicit diagonal"));
    }
    positions
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

    // This is the one sparse-matrix / dense-block multiply needed in the
    // transformed solve path. Keeping it local here makes the Cayley backend
    // explicit and avoids introducing a more general sparse-dense kernel layer
    // before it is justified elsewhere in the crate.
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
                acc[row_idx[idx].zx()].add(values[idx] * rhs_value);
            }
        }
        for row in 0..nrows {
            out[(row, out_col)] = acc[row].finish();
        }
    }

    out
}

fn residual_factor_norm_upper_bound<T>(factor: MatRef<'_, T>) -> T::Real
where
    T: CompensatedField,
    T::Real: Float,
{
    // LR-ADI tracks a residual factor `W_k`, so `||W_k||_F^2` is the cheap
    // upper bound already used by the sparse continuous solver. Reusing the
    // same stopping quantity keeps the sparse continuous and discrete paths
    // behaviorally aligned.
    let w_norm = frobenius_norm(factor);
    w_norm * w_norm
}

fn validate_square(which: &'static str, nrows: usize, ncols: usize) -> Result<(), SteinError> {
    if nrows != ncols {
        if which == "a" {
            return Err(SteinError::NonSquare { nrows, ncols });
        }
        return Err(SteinError::DimensionMismatch {
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
) -> Result<(), SteinError> {
    if actual_nrows != expected_nrows || actual_ncols != expected_ncols {
        return Err(SteinError::DimensionMismatch {
            which,
            expected_nrows,
            expected_ncols,
            actual_nrows,
            actual_ncols,
        });
    }
    Ok(())
}

fn build_discrete_operator<T>(a: MatRef<'_, T>) -> Mat<T>
where
    T: ComplexField + Copy,
{
    let n = a.nrows();
    let mut operator = Mat::<T>::zeros(n * n, n * n);

    // Column-major vectorization turns `A X A^H` into
    // `(conj(A) ⊗ A) vec(X)`. The Stein equation is then
    // `(I - conj(A) ⊗ A) vec(X) = vec(Q)`.
    //
    // The implementation writes the dense operator entry-by-entry rather than
    // relying on a more opaque Kronecker helper so the exact indexing contract
    // stays visible in this reference path.
    for col in 0..n {
        for row in 0..n {
            let eq = vec_index(row, col, n);
            operator[(eq, eq)] = T::one_impl();
            for right_col in 0..n {
                for left_row in 0..n {
                    operator[(eq, vec_index(left_row, right_col, n))] -=
                        a[(row, left_row)] * a[(col, right_col)].conj();
                }
            }
        }
    }

    operator
}

fn unvectorize_square<T: ComplexField + Copy>(values: MatRef<'_, T>, n: usize) -> Mat<T> {
    Mat::from_fn(n, n, |row, col| values[(vec_index(row, col, n), 0)])
}

fn discrete_residual<T>(a: MatRef<'_, T>, x: MatRef<'_, T>, q: MatRef<'_, T>) -> Mat<T>
where
    T: CompensatedField,
    T::Real: Float,
{
    let ax = dense_mul(a, x);
    let axah = dense_mul_adjoint_rhs(ax.as_ref(), a);
    Mat::from_fn(x.nrows(), x.ncols(), |row, col| {
        // Recompute the residual in the original matrix form instead of
        // recycling the vectorized solve state. That gives a more meaningful
        // post-solve accuracy check for downstream control code.
        x[(row, col)] - axah[(row, col)] - q[(row, col)]
    })
}

#[cfg(test)]
mod test {
    use super::{
        SteinError, controllability_gramian_discrete_dense,
        controllability_gramian_discrete_low_rank, discrete_residual,
        observability_gramian_discrete_dense, observability_gramian_discrete_low_rank,
        solve_discrete_stein_dense,
    };
    use crate::control::LyapunovParams;
    use crate::control::dense_ops::{dense_mul_adjoint_lhs, dense_mul_adjoint_rhs, frobenius_norm};
    use crate::control::matrix_equations::lyapunov::ShiftStrategy;
    use faer::sparse::{SparseColMat, Triplet};
    use faer::{Mat, c64};
    use faer_traits::ext::ComplexFieldExt;

    fn diagonal_solution_from_q<T>(diag: &[T], q: &Mat<T>) -> Mat<T>
    where
        T: super::CompensatedField,
        T::Real: num_traits::Float,
    {
        Mat::from_fn(diag.len(), diag.len(), |row, col| {
            q[(row, col)] / (T::one_impl() - diag[row] * diag[col].conj())
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

    fn assert_factor_close<T>(
        factor: &crate::control::matrix_equations::lyapunov::LowRankFactor<T>,
        expected: &Mat<T>,
        tol: T::Real,
    ) where
        T: super::CompensatedField,
        T::Real: num_traits::Float,
    {
        let dense = factor.to_dense();
        assert_close(&dense, expected, tol);
    }

    #[test]
    fn dense_discrete_stein_matches_closed_form_real_diagonal_case() {
        let diag = [0.25f64, -0.5];
        let a = Mat::from_fn(2, 2, |row, col| if row == col { diag[row] } else { 0.0 });
        let q = Mat::from_fn(2, 2, |row, col| match (row, col) {
            (0, 0) => 2.0,
            (0, 1) => -1.0,
            (1, 0) => -1.0,
            _ => 4.0,
        });

        let solve = solve_discrete_stein_dense(a.as_ref(), q.as_ref()).unwrap();
        let expected = diagonal_solution_from_q(&diag, &q);
        assert_close(&solve.solution, &expected, 1.0e-12);
        assert!(solve.residual_norm <= 1.0e-12);
    }

    #[test]
    fn controllability_gramian_matches_closed_form_complex_diagonal_case() {
        let diag = [c64::new(0.25, 0.1), c64::new(-0.35, -0.2)];
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
        let solve = controllability_gramian_discrete_dense(a.as_ref(), b.as_ref()).unwrap();

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
        let diag = [c64::new(0.4, 0.15), c64::new(-0.2, -0.3)];
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
        let solve = observability_gramian_discrete_dense(a.as_ref(), c.as_ref()).unwrap();

        assert_close(&solve.solution, &expected, 1.0e-11);
        assert!(solve.residual_norm <= 1.0e-11);
    }

    #[test]
    fn residual_recomputation_is_small_for_nondiagonal_real_case() {
        let a = Mat::from_fn(3, 3, |row, col| match (row, col) {
            (0, 0) => 0.4,
            (0, 1) => 0.1,
            (0, 2) => 0.0,
            (1, 0) => -0.2,
            (1, 1) => -0.3,
            (1, 2) => 0.15,
            (2, 0) => 0.0,
            (2, 1) => -0.1,
            _ => 0.2,
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

        let solve = solve_discrete_stein_dense(a.as_ref(), q.as_ref()).unwrap();
        let residual = discrete_residual(a.as_ref(), solve.solution.as_ref(), q.as_ref());
        let residual_norm = frobenius_norm(residual.as_ref());
        assert!(residual_norm <= 1.0e-11);
    }

    #[test]
    fn rejects_dimension_mismatches() {
        let a = Mat::<f64>::identity(2, 2);
        let q = Mat::<f64>::identity(3, 3);
        let err = solve_discrete_stein_dense(a.as_ref(), q.as_ref()).unwrap_err();
        assert!(matches!(
            err,
            SteinError::DimensionMismatch {
                which: "q",
                expected_nrows: 2,
                expected_ncols: 2,
                actual_nrows: 3,
                actual_ncols: 3,
            }
        ));
    }

    #[test]
    fn sparse_controllability_low_rank_matches_dense_reference() {
        let a = SparseColMat::<usize, f64>::try_new_from_triplets(
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
        let shifts = ShiftStrategy::user_provided(vec![-0.25, -0.5, -1.0, -2.0]);
        let params = LyapunovParams {
            tol: 1.0e-10,
            max_iters: 24,
        };

        let sparse =
            controllability_gramian_discrete_low_rank(a.as_ref(), b.as_ref(), &shifts, params)
                .unwrap();
        let a_dense = a.as_ref().to_dense();
        let dense = controllability_gramian_discrete_dense(a_dense.as_ref(), b.as_ref()).unwrap();

        assert!(
            sparse.converged,
            "residual_upper_bound={:?}",
            sparse.residual_upper_bound
        );
        assert!(sparse.residual_upper_bound <= 1.0e-10);
        assert_factor_close(&sparse.factor, &dense.solution, 5.0e-8);
    }

    #[test]
    fn sparse_observability_low_rank_matches_dense_reference() {
        let a = SparseColMat::<usize, f64>::try_new_from_triplets(
            3,
            3,
            &[
                Triplet::new(0, 0, 0.2),
                Triplet::new(1, 0, 0.04),
                Triplet::new(0, 1, -0.08),
                Triplet::new(1, 1, -0.35),
                Triplet::new(2, 1, 0.02),
                Triplet::new(1, 2, 0.06),
                Triplet::new(2, 2, 0.1),
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
        let shifts = ShiftStrategy::user_provided(vec![-0.25, -0.5, -1.0, -2.0]);
        let params = LyapunovParams {
            tol: 1.0e-10,
            max_iters: 24,
        };

        let sparse =
            observability_gramian_discrete_low_rank(a.as_ref(), c.as_ref(), &shifts, params)
                .unwrap();
        let a_dense = a.as_ref().to_dense();
        let dense = observability_gramian_discrete_dense(a_dense.as_ref(), c.as_ref()).unwrap();

        assert!(
            sparse.converged,
            "residual_upper_bound={:?}",
            sparse.residual_upper_bound
        );
        assert!(sparse.residual_upper_bound <= 1.0e-10);
        assert_factor_close(&sparse.factor, &dense.solution, 5.0e-8);
    }

    #[test]
    fn sparse_controllability_handles_complex_system() {
        let a = SparseColMat::<usize, c64>::try_new_from_triplets(
            2,
            2,
            &[
                Triplet::new(0, 0, c64::new(0.25, 0.1)),
                Triplet::new(1, 0, c64::new(0.04, -0.03)),
                Triplet::new(0, 1, c64::new(-0.08, 0.02)),
                Triplet::new(1, 1, c64::new(-0.35, -0.2)),
            ],
        )
        .unwrap();
        let b = Mat::from_fn(2, 1, |row, _| match row {
            0 => c64::new(1.0, -0.5),
            _ => c64::new(-0.25, 0.75),
        });
        let shifts = ShiftStrategy::user_provided(vec![c64::new(-0.25, 0.0), c64::new(-1.0, 0.0)]);
        let params = LyapunovParams {
            tol: 1.0e-10,
            max_iters: 24,
        };

        let sparse =
            controllability_gramian_discrete_low_rank(a.as_ref(), b.as_ref(), &shifts, params)
                .unwrap();
        let a_dense = a.as_ref().to_dense();
        let dense = controllability_gramian_discrete_dense(a_dense.as_ref(), b.as_ref()).unwrap();

        assert!(
            sparse.converged,
            "residual_upper_bound={:?}",
            sparse.residual_upper_bound
        );
        assert_factor_close(&sparse.factor, &dense.solution, 1.0e-7);
    }

    #[test]
    fn sparse_low_rank_rejects_singular_cayley_transform() {
        let a =
            SparseColMat::<usize, f64>::try_new_from_triplets(1, 1, &[Triplet::new(0, 0, -1.0)])
                .unwrap();
        let b = Mat::from_fn(1, 1, |_, _| 1.0);
        let err = controllability_gramian_discrete_low_rank(
            a.as_ref(),
            b.as_ref(),
            &ShiftStrategy::user_provided(vec![-0.5]),
            LyapunovParams {
                tol: 1.0e-12,
                max_iters: 1,
            },
        )
        .unwrap_err();
        assert!(matches!(err, SteinError::SingularTransform));
    }
}
