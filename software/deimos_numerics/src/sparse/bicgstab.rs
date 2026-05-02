//! Stabilized bi-conjugate-gradient sparse solver.
//!
//! # Two Intuitions
//!
//! 1. **Krylov view.** BiCGSTAB iteratively builds better approximations to the
//!    solution of `A x = b` using only matrix-vector products and
//!    preconditioner applications.
//! 2. **Residual-polishing view.** The "STAB" part is the method's attempt to
//!    smooth the sometimes erratic convergence of BiCG by minimizing the
//!    residual over a short scalar correction step each iteration.
//!
//! # Glossary
//!
//! - **Preconditioner:** Approximate inverse applied inside each iteration.
//! - **Residual:** Current defect `b - A x`.
//! - **Soft restart:** Heuristic reset used here to recover from loss of
//!   numerical progress.
//!
//! # Mathematical Formulation
//!
//! BiCGSTAB updates a Krylov iterate using coupled bi-orthogonal search
//! directions plus a scalar stabilization step chosen from the current
//! residual-polishing subproblem.
//!
//! # Implementation Notes
//!
//! - The implementation uses compensated reductions for the sensitive inner
//!   products and residual norms it owns, while keeping sparse matrix-vector
//!   products on the plain accumulation path to avoid per-iteration scratch
//!   allocation and preserve throughput.
//! - Any `Precond` implementation can be dropped in, including lagged sparse LU
//!   and Cholesky factorizations from this crate.

use super::col::{col_from_slice, col_slice, col_slice_mut, copy_col, zero_col};
use super::compensated::{CompensatedField, dotc, norm2, norm2_sq};
use super::matvec::SparseMatVec;
use super::precond::{IdentityPrecond, Precond, apply_precond_to_col, precond_buffer};
use core::fmt;
use faer::Col;
use faer::dyn_stack::MemBuffer;
use faer_traits::ComplexField;
use faer_traits::ext::ComplexFieldExt;
use faer_traits::math_utils::{eps, from_f64, zero};
use num_traits::Float;

/// Input-validation failures for [`BiCGSTAB`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BiCGSTABError {
    /// The system matrix is not square.
    NonSquareMatrix {
        /// Actual row count.
        nrows: usize,
        /// Actual column count.
        ncols: usize,
    },
    /// The initial guess length does not match the system dimension.
    WrongInitialGuessLen {
        /// Expected vector length.
        expected: usize,
        /// Actual vector length.
        actual: usize,
    },
    /// The right-hand side length does not match the system dimension.
    WrongRhsLen {
        /// Expected vector length.
        expected: usize,
        /// Actual vector length.
        actual: usize,
    },
    /// The preconditioner output dimension does not match the system dimension.
    WrongPreconditionerOutputDim {
        /// Expected output dimension.
        expected: usize,
        /// Actual output dimension.
        actual: usize,
    },
    /// The preconditioner input dimension does not match the system dimension.
    WrongPreconditionerInputDim {
        /// Expected input dimension.
        expected: usize,
        /// Actual input dimension.
        actual: usize,
    },
}

impl fmt::Display for BiCGSTABError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NonSquareMatrix { nrows, ncols } => {
                write!(f, "BiCGSTAB requires a square matrix, got {nrows}x{ncols}")
            }
            Self::WrongInitialGuessLen { expected, actual } => {
                write!(
                    f,
                    "BiCGSTAB initial guess has length {actual}, expected {expected}"
                )
            }
            Self::WrongRhsLen { expected, actual } => {
                write!(
                    f,
                    "BiCGSTAB right-hand side has length {actual}, expected {expected}"
                )
            }
            Self::WrongPreconditionerOutputDim { expected, actual } => {
                write!(
                    f,
                    "BiCGSTAB preconditioner output dimension is {actual}, expected {expected}"
                )
            }
            Self::WrongPreconditionerInputDim { expected, actual } => {
                write!(
                    f,
                    "BiCGSTAB preconditioner input dimension is {actual}, expected {expected}"
                )
            }
        }
    }
}

impl core::error::Error for BiCGSTABError {}

/// Errors returned by the one-shot [`BiCGSTAB::solve`] helpers.
#[derive(Debug)]
pub enum BiCGSTABSolveError<
    T: ComplexField + Copy,
    A: SparseMatVec<T>,
    P: Precond<T> = IdentityPrecond,
> {
    /// The caller supplied invalid matrix, vector, or preconditioner dimensions.
    InvalidInput(BiCGSTABError),
    /// The iteration reached its limit before satisfying the requested tolerance.
    NoConvergence(BiCGSTAB<T, A, P>),
}

impl<T: ComplexField + Copy, A: SparseMatVec<T>, P: Precond<T>> fmt::Display
    for BiCGSTABSolveError<T, A, P>
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidInput(err) => err.fmt(f),
            Self::NoConvergence(_) => {
                write!(f, "BiCGSTAB did not converge within the iteration limit")
            }
        }
    }
}

impl<T, A, P> core::error::Error for BiCGSTABSolveError<T, A, P>
where
    T: ComplexField + Copy,
    A: SparseMatVec<T> + fmt::Debug,
    P: Precond<T> + fmt::Debug,
{
}

impl<T: ComplexField + Copy, A: SparseMatVec<T>, P: Precond<T>> BiCGSTABSolveError<T, A, P> {
    /// Returns the underlying solver when the solve failed due to non-convergence.
    ///
    /// Returns:
    ///   `Some(solver)` when the solve stopped at the iteration limit, or
    ///   `None` when the failure was an input-validation error.
    pub fn into_solver(self) -> Option<BiCGSTAB<T, A, P>> {
        match self {
            Self::NoConvergence(solver) => Some(solver),
            Self::InvalidInput(_) => None,
        }
    }
}

fn validate_bicgstab_dimensions<T: ComplexField + Copy, A: SparseMatVec<T>, P: Precond<T>>(
    a: &A,
    preconditioner: &P,
    x0: &[T],
    b: &[T],
) -> Result<(), BiCGSTABError> {
    if a.nrows() != a.ncols() {
        return Err(BiCGSTABError::NonSquareMatrix {
            nrows: a.nrows(),
            ncols: a.ncols(),
        });
    }
    if x0.len() != a.ncols() {
        return Err(BiCGSTABError::WrongInitialGuessLen {
            expected: a.ncols(),
            actual: x0.len(),
        });
    }
    if b.len() != a.nrows() {
        return Err(BiCGSTABError::WrongRhsLen {
            expected: a.nrows(),
            actual: b.len(),
        });
    }
    if preconditioner.nrows() != a.ncols() {
        return Err(BiCGSTABError::WrongPreconditionerOutputDim {
            expected: a.ncols(),
            actual: preconditioner.nrows(),
        });
    }
    if preconditioner.ncols() != a.ncols() {
        return Err(BiCGSTABError::WrongPreconditionerInputDim {
            expected: a.ncols(),
            actual: preconditioner.ncols(),
        });
    }
    Ok(())
}

/// Stabilized bi-conjugate gradient solver.
///
/// The internal vectors all have length `n = a.ncols() = a.nrows()`.
pub struct BiCGSTAB<T: ComplexField + Copy, A: SparseMatVec<T>, P: Precond<T> = IdentityPrecond> {
    iteration_count: usize,
    soft_restart_threshold: T::Real,
    soft_restart_count: usize,
    hard_restart_count: usize,
    err: T::Real,
    a: A,
    b: Col<T>,
    x: Col<T>,
    r: Col<T>,
    rhat: Col<T>,
    p: Col<T>,
    v: Col<T>,
    s: Col<T>,
    t: Col<T>,
    scratch: Col<T>,
    z: Col<T>,
    preconditioner: P,
    precond_buffer: MemBuffer,
    rho: T,
}

impl<T: ComplexField + Copy, A: SparseMatVec<T>, P: Precond<T>> fmt::Debug for BiCGSTAB<T, A, P> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BiCGSTAB")
            .field("iteration_count", &self.iteration_count)
            .field("soft_restart_threshold", &self.soft_restart_threshold)
            .field("soft_restart_count", &self.soft_restart_count)
            .field("hard_restart_count", &self.hard_restart_count)
            .field("err", &self.err)
            .field("a", &self.a)
            .field("b", &self.b)
            .field("x", &self.x)
            .field("r", &self.r)
            .field("rhat", &self.rhat)
            .field("p", &self.p)
            .field("v", &self.v)
            .field("s", &self.s)
            .field("t", &self.t)
            .field("scratch", &self.scratch)
            .field("z", &self.z)
            .field("rho", &self.rho)
            .field("preconditioner", &self.preconditioner)
            .finish_non_exhaustive()
    }
}

impl<T: CompensatedField, A: SparseMatVec<T>> BiCGSTAB<T, A, IdentityPrecond>
where
    T::Real: Float,
{
    /// Initializes a solver with a fresh residual estimate.
    ///
    /// Args:
    ///   a: Sparse linear operator with shape `(n, n)`.
    ///   x0: Initial guess vector with length `n`.
    ///   b: Right-hand side vector with length `n`.
    ///
    /// Returns:
    ///   A BiCGSTAB solver initialized at `x0` with residual `b - A x0`.
    #[inline]
    pub fn new(a: A, x0: &[T], b: &[T]) -> Result<Self, BiCGSTABError> {
        let dim = a.ncols();
        Self::new_with_precond(a, IdentityPrecond { dim }, x0, b)
    }

    /// Attempts to solve the system to the given absolute residual tolerance.
    ///
    /// Args:
    ///   a: Sparse linear operator with shape `(n, n)`.
    ///   x0: Initial guess vector with length `n`.
    ///   b: Right-hand side vector with length `n`.
    ///   tol: Absolute residual tolerance in right-hand-side units.
    ///   max_iter: Maximum number of BiCGSTAB iterations.
    ///
    /// Returns:
    ///   The converged solver state, or a wrapper carrying the unfinished
    ///   solver on non-convergence.
    pub fn solve(
        a: A,
        x0: &[T],
        b: &[T],
        tol: T::Real,
        max_iter: usize,
    ) -> Result<Self, BiCGSTABSolveError<T, A>> {
        let mut solver = Self::new(a, x0, b).map_err(BiCGSTABSolveError::InvalidInput)?;
        if solver.err() < tol {
            return Ok(solver);
        }

        for _ in 0..max_iter {
            solver.step();
            if solver.err() < tol {
                solver.hard_restart();
                if solver.err() < tol {
                    return Ok(solver);
                }
            }
        }

        Err(BiCGSTABSolveError::NoConvergence(solver))
    }
}

impl<T: CompensatedField, A: SparseMatVec<T>, P: Precond<T>> BiCGSTAB<T, A, P>
where
    T::Real: Float,
{
    /// Initializes a solver with a fresh residual estimate and preconditioner.
    ///
    /// This implementation uses `preconditioner` on the right. In other
    /// words, the iteration builds directions in an internally rescaled
    /// variable `y` and maps them back through `x = M^{-1} y` before each
    /// matrix-vector product. For a diagonal preconditioner, that means the
    /// search directions are scaled componentwise by the inverse diagonal
    /// before `A` sees them.
    ///
    /// Args:
    ///   a: Sparse linear operator with shape `(n, n)`.
    ///   preconditioner: Preconditioner with input and output dimensions `n`.
    ///   x0: Initial guess vector with length `n`.
    ///   b: Right-hand side vector with length `n`.
    ///
    /// Returns:
    ///   A BiCGSTAB solver initialized at `x0` with the supplied right
    ///   preconditioner.
    #[inline]
    pub fn new_with_precond(
        a: A,
        preconditioner: P,
        x0: &[T],
        b: &[T],
    ) -> Result<Self, BiCGSTABError> {
        validate_bicgstab_dimensions(&a, &preconditioner, x0, b)?;

        let n = a.nrows();
        let b_col = col_from_slice(b);
        let x = col_from_slice(x0);
        let mut scratch = zero_col::<T>(n);
        let mut r = zero_col::<T>(n);

        a.apply(col_slice_mut(&mut scratch), col_slice(&x));
        for ((r, &b), &ax) in col_slice_mut(&mut r)
            .iter_mut()
            .zip(col_slice(&b_col).iter())
            .zip(col_slice(&scratch).iter())
        {
            *r = b - ax;
        }

        let err = norm2::<T>(col_slice(&r));
        let rho = dotc::<T>(col_slice(&r), col_slice(&r));
        let rhat = col_from_slice(col_slice(&r));
        let p = col_from_slice(col_slice(&r));
        let v = zero_col::<T>(n);
        let s = zero_col::<T>(n);
        let t = zero_col::<T>(n);
        let z = zero_col::<T>(n);
        let precond_buffer = precond_buffer(&preconditioner);

        Ok(Self {
            iteration_count: 0,
            soft_restart_threshold: from_f64::<T::Real>(0.1),
            soft_restart_count: 0,
            hard_restart_count: 0,
            err,
            a,
            b: b_col,
            x,
            r,
            rhat,
            p,
            v,
            s,
            t,
            scratch,
            z,
            preconditioner,
            precond_buffer,
            rho,
        })
    }

    /// Attempts to solve the system to the given absolute residual tolerance with a preconditioner.
    ///
    /// Intuitively, a good preconditioner changes the geometry of the problem
    /// so that the Krylov iteration sees components on more comparable scales.
    /// In this solver that effect is realized through right preconditioning.
    ///
    /// Args:
    ///   a: Sparse linear operator with shape `(n, n)`.
    ///   preconditioner: Preconditioner with input and output dimensions `n`.
    ///   x0: Initial guess vector with length `n`.
    ///   b: Right-hand side vector with length `n`.
    ///   tol: Absolute residual tolerance in right-hand-side units.
    ///   max_iter: Maximum number of BiCGSTAB iterations.
    ///
    /// Returns:
    ///   The converged solver state, or a wrapper carrying the unfinished
    ///   solver on non-convergence.
    pub fn solve_with_precond(
        a: A,
        preconditioner: P,
        x0: &[T],
        b: &[T],
        tol: T::Real,
        max_iter: usize,
    ) -> Result<Self, BiCGSTABSolveError<T, A, P>> {
        let mut solver = Self::new_with_precond(a, preconditioner, x0, b)
            .map_err(BiCGSTABSolveError::InvalidInput)?;
        if solver.err() < tol {
            return Ok(solver);
        }

        for _ in 0..max_iter {
            solver.step();
            if solver.err() < tol {
                solver.hard_restart();
                if solver.err() < tol {
                    return Ok(solver);
                }
            }
        }

        Err(BiCGSTABSolveError::NoConvergence(solver))
    }

    /// Resets the shadow residual to avoid a singular update.
    pub fn soft_restart(&mut self) {
        self.soft_restart_count += 1;
        copy_col(&mut self.rhat, &self.r);
        self.rho = dotc::<T>(col_slice(&self.r), col_slice(&self.r));
        copy_col(&mut self.p, &self.r);
    }

    /// Recomputes the residual from scratch with compensated accumulation.
    pub fn hard_restart(&mut self) {
        self.hard_restart_count += 1;
        self.a
            .apply(col_slice_mut(&mut self.scratch), col_slice(&self.x));

        for ((r, &b), &ax) in col_slice_mut(&mut self.r)
            .iter_mut()
            .zip(col_slice(&self.b).iter())
            .zip(col_slice(&self.scratch).iter())
        {
            *r = b - ax;
        }

        self.err = norm2::<T>(col_slice(&self.r));
        self.soft_restart();
        self.soft_restart_count -= 1;
    }

    /// Advances the solver by one BiCGSTAB iteration.
    ///
    /// Returns:
    ///   The latest residual norm estimate in right-hand-side units.
    pub fn step(&mut self) -> T::Real {
        self.iteration_count += 1;

        // Right preconditioning rescales the search direction before the
        // matrix-vector product. With a diagonal preconditioner this is
        // column-balancing: large columns are damped and small columns are
        // amplified through the inverse diagonal.
        apply_precond_to_col(
            &self.preconditioner,
            &mut self.z,
            &self.p,
            &mut self.precond_buffer,
        );
        self.a.apply(col_slice_mut(&mut self.v), col_slice(&self.z));

        let denom = dotc::<T>(col_slice(&self.rhat), col_slice(&self.v));
        if denom.abs() <= eps::<T::Real>() {
            self.soft_restart();
            return self.err;
        }

        let alpha = self.rho / denom;

        for ((h, &x), &p) in col_slice_mut(&mut self.scratch)
            .iter_mut()
            .zip(col_slice(&self.x).iter())
            .zip(col_slice(&self.z).iter())
        {
            *h = x + p * alpha;
        }

        for ((s, &r), &v) in col_slice_mut(&mut self.s)
            .iter_mut()
            .zip(col_slice(&self.r).iter())
            .zip(col_slice(&self.v).iter())
        {
            *s = r - v * alpha;
        }

        let s_norm_sq = norm2_sq::<T>(col_slice(&self.s));
        if s_norm_sq <= eps::<T::Real>() {
            copy_col(&mut self.x, &self.scratch);
            copy_col(&mut self.r, &self.s);
            self.err = s_norm_sq.sqrt();
            self.rho = dotc::<T>(col_slice(&self.rhat), col_slice(&self.r));
            return self.err;
        }

        // The same right-preconditioning is applied to the secondary search
        // direction before forming `A * z`.
        apply_precond_to_col(
            &self.preconditioner,
            &mut self.z,
            &self.s,
            &mut self.precond_buffer,
        );
        self.a.apply(col_slice_mut(&mut self.t), col_slice(&self.z));

        let t_norm_sq = norm2_sq::<T>(col_slice(&self.t));
        if t_norm_sq <= eps::<T::Real>() {
            copy_col(&mut self.x, &self.scratch);
            copy_col(&mut self.r, &self.s);
            self.err = norm2::<T>(col_slice(&self.r));
            self.rho = dotc::<T>(col_slice(&self.rhat), col_slice(&self.r));
            return self.err;
        }

        let omega = dotc::<T>(col_slice(&self.t), col_slice(&self.s)).mul_real(t_norm_sq.recip());

        for ((x, &h), &s) in col_slice_mut(&mut self.x)
            .iter_mut()
            .zip(col_slice(&self.scratch).iter())
            .zip(col_slice(&self.z).iter())
        {
            *x = h + s * omega;
        }

        for ((r, &s), &t) in col_slice_mut(&mut self.r)
            .iter_mut()
            .zip(col_slice(&self.s).iter())
            .zip(col_slice(&self.t).iter())
        {
            *r = s - t * omega;
        }

        let err_sq = norm2_sq::<T>(col_slice(&self.r));
        self.err = err_sq.sqrt();

        let rho_prev = self.rho;
        self.rho = dotc::<T>(col_slice(&self.rhat), col_slice(&self.r));

        let should_restart =
            err_sq > zero::<T::Real>() && self.rho.abs() / err_sq < self.soft_restart_threshold;
        if should_restart || rho_prev.abs() <= eps::<T::Real>() || omega.abs() <= eps::<T::Real>() {
            self.soft_restart();
        } else {
            let beta = (self.rho / rho_prev) * (alpha / omega);

            for (((p_new, &r), &p_old), &v) in col_slice_mut(&mut self.scratch)
                .iter_mut()
                .zip(col_slice(&self.r).iter())
                .zip(col_slice(&self.p).iter())
                .zip(col_slice(&self.v).iter())
            {
                *p_new = r + p_old * beta - (v * omega) * beta;
            }

            copy_col(&mut self.p, &self.scratch);
        }

        self.err
    }

    /// Sets the soft-restart threshold.
    ///
    /// Args:
    ///   threshold: Dimensionless restart heuristic threshold based on
    ///     `|rho| / ||r||^2`.
    ///
    /// Returns:
    ///   The updated solver.
    #[must_use]
    pub fn with_restart_threshold(mut self, threshold: T::Real) -> Self {
        self.soft_restart_threshold = threshold;
        self
    }

    /// Current iteration count.
    ///
    /// Returns:
    ///   The number of BiCGSTAB iterations completed.
    pub fn iteration_count(&self) -> usize {
        self.iteration_count
    }

    /// Current soft-restart threshold, a dimensionless restart heuristic.
    pub fn soft_restart_threshold(&self) -> T::Real {
        self.soft_restart_threshold
    }

    /// Number of soft restarts that have been performed.
    pub fn soft_restart_count(&self) -> usize {
        self.soft_restart_count
    }

    /// Number of hard restarts that have been performed.
    pub fn hard_restart_count(&self) -> usize {
        self.hard_restart_count
    }

    /// Latest residual norm estimate in right-hand-side units.
    pub fn err(&self) -> T::Real {
        self.err
    }

    /// Current `rho = dotc(rhat, r)`.
    pub fn rho(&self) -> T {
        self.rho
    }

    /// Problem matrix with shape `(n, n)`.
    pub fn a(&self) -> A {
        self.a
    }

    /// Preconditioner with input and output dimensions `n`.
    pub fn preconditioner(&self) -> &P {
        &self.preconditioner
    }

    /// Latest solution estimate with shape `(n, 1)`.
    pub fn x(&self) -> &Col<T> {
        &self.x
    }

    /// Right-hand side with shape `(n, 1)`.
    pub fn b(&self) -> &Col<T> {
        &self.b
    }

    /// Latest residual vector with shape `(n, 1)`.
    pub fn r(&self) -> &Col<T> {
        &self.r
    }

    /// Shadow residual direction with shape `(n, 1)`.
    pub fn rhat(&self) -> &Col<T> {
        &self.rhat
    }

    /// Current search direction with shape `(n, 1)`.
    pub fn p(&self) -> &Col<T> {
        &self.p
    }
}

#[cfg(test)]
mod test {
    use super::{BiCGSTAB, BiCGSTABError, BiCGSTABSolveError};
    use crate::sparse::col::{col_slice, col_slice_mut};
    use crate::sparse::compensated::{CompensatedField, norm2};
    use crate::sparse::matvec::SparseMatVec;
    use crate::sparse::{DiagonalPrecond, IdentityPrecond};
    use alloc::vec::Vec;
    use faer::dyn_stack::{MemBuffer, MemStack};
    use faer::mat::AsMatRef;
    use faer::matrix_free::InitialGuessStatus;
    use faer::matrix_free::bicgstab::{BicgParams, bicgstab as faer_bicgstab, bicgstab_scratch};
    use faer::sparse::{SparseColMat, SparseRowMat, Triplet};
    use faer::{Col, Mat, Par, c32, c64};
    use num_traits::Float;
    use std::println;
    use std::time::Instant;

    fn apply_to_col<T, A>(a: A, x: &[T]) -> Col<T>
    where
        T: CompensatedField,
        T::Real: Float,
        A: SparseMatVec<T>,
    {
        let mut out = crate::sparse::col::zero_col::<T>(a.nrows());
        a.apply_compensated(col_slice_mut(&mut out), x);
        out
    }

    fn assert_solution_close<T, A>(a: A, x_true: &[T], x0: &[T], tol: T::Real)
    where
        T: CompensatedField,
        T::Real: Float,
        A: SparseMatVec<T>,
    {
        let b = apply_to_col(a, x_true);
        let solver = BiCGSTAB::solve(a, x0, col_slice(&b), tol, 50).unwrap();

        let x = col_slice(solver.x());
        let mut diff = crate::sparse::col::zero_col::<T>(x.len());
        for ((dst, &lhs), &rhs) in col_slice_mut(&mut diff)
            .iter_mut()
            .zip(x.iter())
            .zip(x_true.iter())
        {
            *dst = lhs - rhs;
        }

        assert!(solver.err() < tol);
        assert!(norm2::<T>(col_slice(&diff)) < tol.sqrt());
    }

    fn residual_norm<T, A>(a: A, x: &[T], b: &[T]) -> T::Real
    where
        T: CompensatedField,
        T::Real: Float,
        A: SparseMatVec<T>,
    {
        let ax = apply_to_col(a, x);
        let mut residual = crate::sparse::col::zero_col::<T>(b.len());
        for ((dst, &lhs), &rhs) in col_slice_mut(&mut residual)
            .iter_mut()
            .zip(col_slice(&ax).iter())
            .zip(b.iter())
        {
            *dst = rhs - lhs;
        }

        norm2::<T>(col_slice(&residual))
    }

    #[test]
    fn solves_csr_f64_system() {
        let a = SparseRowMat::<usize, f64>::try_new_from_triplets(
            4,
            4,
            &[
                Triplet::new(0, 0, 4.0),
                Triplet::new(0, 1, -1.0),
                Triplet::new(1, 0, 2.0),
                Triplet::new(1, 1, 5.0),
                Triplet::new(1, 2, 1.0),
                Triplet::new(2, 1, 2.0),
                Triplet::new(2, 2, 4.0),
                Triplet::new(2, 3, -1.0),
                Triplet::new(3, 0, 1.0),
                Triplet::new(3, 3, 3.0),
            ],
        )
        .unwrap();

        let x_true = [1.0, -2.0, 0.5, 3.0];
        let x0 = [0.25, 0.25, 0.25, 0.25];
        assert_solution_close(a.as_ref(), &x_true, &x0, 1.0e-12);
    }

    #[test]
    fn solves_csc_f32_system() {
        let a = SparseColMat::<usize, f32>::try_new_from_triplets(
            4,
            4,
            &[
                Triplet::new(0, 0, 4.0),
                Triplet::new(0, 1, -1.0),
                Triplet::new(1, 0, 2.0),
                Triplet::new(1, 1, 5.0),
                Triplet::new(1, 2, 1.0),
                Triplet::new(2, 1, 2.0),
                Triplet::new(2, 2, 4.0),
                Triplet::new(2, 3, -1.0),
                Triplet::new(3, 0, 1.0),
                Triplet::new(3, 3, 3.0),
            ],
        )
        .unwrap();

        let x_true = [1.0f32, -2.0, 0.5, 3.0];
        let x0 = [0.1f32, 0.1, 0.1, 0.1];
        assert_solution_close(a.as_ref(), &x_true, &x0, 1.0e-4);
    }

    #[test]
    fn solves_with_explicit_identity_preconditioner() {
        let a = SparseRowMat::<usize, f64>::try_new_from_triplets(
            4,
            4,
            &[
                Triplet::new(0, 0, 4.0),
                Triplet::new(0, 1, -1.0),
                Triplet::new(1, 0, 2.0),
                Triplet::new(1, 1, 5.0),
                Triplet::new(1, 2, 1.0),
                Triplet::new(2, 1, 2.0),
                Triplet::new(2, 2, 4.0),
                Triplet::new(2, 3, -1.0),
                Triplet::new(3, 0, 1.0),
                Triplet::new(3, 3, 3.0),
            ],
        )
        .unwrap();

        let x_true = [1.0, -2.0, 0.5, 3.0];
        let b = apply_to_col(a.as_ref(), &x_true);
        let solver = BiCGSTAB::solve_with_precond(
            a.as_ref(),
            crate::sparse::IdentityPrecond { dim: 4 },
            &[0.0; 4],
            col_slice(&b),
            1.0e-12,
            50,
        )
        .unwrap();

        assert!(residual_norm(a.as_ref(), col_slice(solver.x()), col_slice(&b)) < 1.0e-12);
    }

    #[test]
    fn solves_complex_csr_system() {
        let a = SparseRowMat::<usize, c64>::try_new_from_triplets(
            3,
            3,
            &[
                Triplet::new(0, 0, c64::new(4.0, 1.0)),
                Triplet::new(0, 1, c64::new(-1.0, 0.5)),
                Triplet::new(1, 0, c64::new(2.0, -0.5)),
                Triplet::new(1, 1, c64::new(5.0, 0.0)),
                Triplet::new(1, 2, c64::new(1.0, 1.0)),
                Triplet::new(2, 1, c64::new(2.0, -1.0)),
                Triplet::new(2, 2, c64::new(3.0, 0.25)),
            ],
        )
        .unwrap();

        let x_true = [
            c64::new(1.0, -0.5),
            c64::new(-2.0, 1.0),
            c64::new(0.5, 0.25),
        ];
        let x0 = [c64::new(0.0, 0.0); 3];
        assert_solution_close(a.as_ref(), &x_true, &x0, 1.0e-11);
    }

    #[test]
    fn solves_complex_csc_system() {
        let a = SparseColMat::<usize, c32>::try_new_from_triplets(
            3,
            3,
            &[
                Triplet::new(0, 0, c32::new(4.0, 1.0)),
                Triplet::new(0, 1, c32::new(-1.0, 0.5)),
                Triplet::new(1, 0, c32::new(2.0, -0.5)),
                Triplet::new(1, 1, c32::new(5.0, 0.0)),
                Triplet::new(1, 2, c32::new(1.0, 1.0)),
                Triplet::new(2, 1, c32::new(2.0, -1.0)),
                Triplet::new(2, 2, c32::new(3.0, 0.25)),
            ],
        )
        .unwrap();

        let x_true = [
            c32::new(1.0, -0.5),
            c32::new(-2.0, 1.0),
            c32::new(0.5, 0.25),
        ];
        let x0 = [c32::new(0.0, 0.0); 3];
        assert_solution_close(a.as_ref(), &x_true, &x0, 1.0e-3);
    }

    #[test]
    fn reports_failure_when_iteration_limit_is_too_small() {
        let a = SparseRowMat::<usize, f64>::try_new_from_triplets(
            4,
            4,
            &[
                Triplet::new(0, 0, 4.0),
                Triplet::new(0, 1, -1.0),
                Triplet::new(1, 0, 2.0),
                Triplet::new(1, 1, 5.0),
                Triplet::new(1, 2, 1.0),
                Triplet::new(2, 1, 2.0),
                Triplet::new(2, 2, 4.0),
                Triplet::new(2, 3, -1.0),
                Triplet::new(3, 0, 1.0),
                Triplet::new(3, 3, 3.0),
            ],
        )
        .unwrap();

        let x_true = [1.0, -2.0, 0.5, 3.0];
        let b = apply_to_col(a.as_ref(), &x_true);
        let result = BiCGSTAB::solve(a.as_ref(), &[0.0; 4], col_slice(&b), 1.0e-12, 1);

        assert!(result.is_err());
    }

    #[test]
    fn invalid_dimensions_do_not_panic_in_solve() {
        let a = SparseRowMat::<usize, f64>::try_new_from_triplets(
            2,
            3,
            &[
                Triplet::new(0, 0, 1.0),
                Triplet::new(0, 1, 2.0),
                Triplet::new(1, 1, 3.0),
                Triplet::new(1, 2, 4.0),
            ],
        )
        .unwrap();

        let result = std::panic::catch_unwind(|| {
            BiCGSTAB::solve(a.as_ref(), &[0.0, 0.0, 0.0], &[1.0, 2.0], 1.0e-12, 5)
        });

        assert!(matches!(
            result,
            Ok(Err(BiCGSTABSolveError::InvalidInput(
                BiCGSTABError::NonSquareMatrix { nrows: 2, ncols: 3 }
            )))
        ));
    }

    #[test]
    fn diagonal_preconditioner_reduces_iterations_on_column_scaled_system() {
        let n = 12usize;
        let scales: Vec<f64> = (0..n).map(|i| 10.0f64.powi(i as i32 - 6)).collect();
        let mut triplets = Vec::with_capacity(3 * n - 2);
        for row in 0..n {
            triplets.push(Triplet::new(row, row, scales[row]));
            if row > 0 {
                triplets.push(Triplet::new(row, row - 1, -0.25 * scales[row - 1]));
            }
            if row + 1 < n {
                triplets.push(Triplet::new(row, row + 1, -0.25 * scales[row + 1]));
            }
        }

        let a = SparseRowMat::<usize, f64>::try_new_from_triplets(n, n, &triplets).unwrap();
        let x_true: Vec<f64> = scales.iter().map(|&scale| scale.recip()).collect();
        let x0 = vec![0.0; n];
        let b = apply_to_col(a.as_ref(), &x_true);
        let tol = 1.0e-10;

        let identity = BiCGSTAB::solve(a.as_ref(), &x0, col_slice(&b), tol, 200);
        let diagonal = BiCGSTAB::solve_with_precond(
            a.as_ref(),
            DiagonalPrecond::try_from(a.as_ref()).unwrap(),
            &x0,
            col_slice(&b),
            tol,
            200,
        )
        .unwrap();

        assert!(diagonal.err() < tol);
        match identity {
            Ok(identity) => assert!(diagonal.iteration_count() < identity.iteration_count()),
            Err(BiCGSTABSolveError::NoConvergence(identity)) => {
                assert!(identity.err() >= tol);
                assert_eq!(identity.iteration_count(), 200);
            }
            Err(BiCGSTABSolveError::InvalidInput(err)) => panic!("unexpected invalid input: {err}"),
        }
    }

    #[test]
    fn compensated_bicgstab_matches_or_beats_faer_on_poorly_conditioned_system() {
        let n = 12usize;
        let mut triplets = Vec::with_capacity(n * n);
        for row in 0..n {
            for col in 0..n {
                let hilbert = 1.0 / (row + col + 1) as f64;
                let skew = if row > col {
                    (row - col) as f64 * 1.0e-14
                } else {
                    -((col - row) as f64) * 1.0e-14
                };
                let diag_shift = if row == col {
                    (row + 1) as f64 * 1.0e-12
                } else {
                    0.0
                };
                triplets.push(Triplet::new(row, col, hilbert + skew + diag_shift));
            }
        }

        let a = SparseRowMat::<usize, f64>::try_new_from_triplets(n, n, &triplets).unwrap();
        let x_true: Vec<f64> = (0..n)
            .map(|i| if i % 2 == 0 { 1.0 } else { -1.0 })
            .collect();
        let x0 = vec![0.0; n];
        let b = apply_to_col(a.as_ref(), &x_true);
        let tol = 1.0e-4;

        let started = Instant::now();
        let ours = match BiCGSTAB::solve(a.as_ref(), &x0, col_slice(&b), tol, 400) {
            Ok(solver) => solver,
            Err(BiCGSTABSolveError::NoConvergence(solver)) => solver,
            Err(BiCGSTABSolveError::InvalidInput(err)) => panic!("unexpected invalid input: {err}"),
        };
        let elapsed = started.elapsed();
        println!(
            "compensated hilbert solve: tol={tol:.1e}, iterations={}, hard_restarts={}, soft_restarts={}, err={:.6e}, elapsed={:?}",
            ours.iteration_count(),
            ours.hard_restart_count(),
            ours.soft_restart_count(),
            ours.err(),
            elapsed,
        );
        let ours_residual = residual_norm(a.as_ref(), col_slice(ours.x()), col_slice(&b));
        let ours_error = norm2::<f64>(
            &col_slice(ours.x())
                .iter()
                .zip(x_true.iter())
                .map(|(&lhs, &rhs)| lhs - rhs)
                .collect::<Vec<_>>(),
        );

        let mut faer_out = Mat::<f64>::zeros(n, 1);
        let identity = IdentityPrecond { dim: n };
        let params = BicgParams {
            initial_guess: InitialGuessStatus::Zero,
            rel_tolerance: tol,
            max_iters: 400,
            ..Default::default()
        };

        let faer_result = faer_bicgstab(
            faer_out.as_mut(),
            identity,
            identity,
            a.as_ref(),
            b.as_mat_ref().as_dyn(),
            params,
            |_| {},
            Par::Seq,
            MemStack::new(&mut MemBuffer::new(bicgstab_scratch(
                identity,
                identity,
                a.as_ref(),
                1,
                Par::Seq,
            ))),
        );

        let faer_x: Vec<f64> = (0..n).map(|i| faer_out[(i, 0)]).collect();
        let faer_residual = residual_norm(a.as_ref(), &faer_x, col_slice(&b));
        let faer_error = norm2::<f64>(
            &faer_x
                .iter()
                .zip(x_true.iter())
                .map(|(&lhs, &rhs)| lhs - rhs)
                .collect::<Vec<_>>(),
        );

        assert!(ours_residual.is_finite());
        assert!(ours_error.is_finite());
        assert!(faer_result.is_err() || (faer_residual.is_finite() && faer_error.is_finite()));
        assert!(faer_residual.is_nan() || ours_residual <= faer_residual);
        assert!(faer_error.is_nan() || ours_error <= faer_error);
    }
}
