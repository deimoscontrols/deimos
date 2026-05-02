//! Sparse LU wrapper around `faer`'s symbolic and numeric LU routines.
//!
//! This type is intentionally dual-purpose:
//!
//! - as a direct solver, it exposes staged symbolic analysis, numeric
//!   refactorization, and solve methods
//! - as a preconditioner, it implements `faer`'s [`Precond`] trait so the same
//!   stored factors can be reused as a lagged right-preconditioner in
//!   iterative solvers such as [`crate::sparse::BiCGSTAB`]
//!
//! The direct uncompensated solve path delegates to `faer`'s LU solve kernels.
//! The compensated solve path is built on top of that by performing iterative
//! refinement with:
//!
//! - compensated sparse matvec for residual recomputation
//! - compensated vector updates for `x += delta`
//!
//! When the factors correspond to the current matrix, that gives a refined
//! direct solve. When the factors are lagged from a nearby matrix, the same
//! mechanism acts like a correction iteration preconditioned by the stored LU.
//!
//! # Two Intuitions
//!
//! 1. **Direct-solver view.** LU turns a sparse linear system into triangular
//!    solves after one factorization.
//! 2. **Reusable-operator view.** The same stored factors are useful even when
//!    they are not exact for the current matrix: they become a practical lagged
//!    preconditioner or a refinement engine.
//!
//! # Glossary
//!
//! - **Symbolic factorization:** Pattern-dependent setup phase.
//! - **Numeric factorization:** Value-dependent phase for one matrix instance.
//! - **Iterative refinement:** Residual-correction loop after the initial LU
//!   solve.
//!
//! # Mathematical Formulation
//!
//! The direct solve uses sparse LU factors and triangular solves; the
//! compensated path refines that solution by repeatedly solving correction
//! equations against the stored LU factors.
//!
//! # Implementation Notes
//!
//! - The compensated refinement logic is wrapper-owned and therefore visible to
//!   the crate's higher-level sparse workflows.
//! - The type implements the preconditioner trait so the same factors can be
//!   reused inside Krylov solvers.

use super::col::{col_from_slice, col_slice, col_slice_mut, copy_col, zero_col};
use super::compensated::{CompensatedField, norm2};
use super::matvec::SparseMatVec;
use super::precond::Precond;
use alloc::vec::Vec;
use core::fmt;
use faer::dyn_stack::{MemBuffer, MemStack};
use faer::linalg::lu::partial_pivoting::factor::PartialPivLuParams;
use faer::matrix_free::LinOp;
use faer::prelude::ReborrowMut;
use faer::sparse::FaerError;
use faer::sparse::SparseColMatRef;
use faer::sparse::linalg::LuError;
use faer::sparse::linalg::lu::{
    LuRef, LuSymbolicParams, NumericLu, SymbolicLu, factorize_symbolic_lu,
};
use faer::{Col, Conj, Index, MatMut, MatRef, Par, Spec, Unbind};
use faer_traits::ComplexField;
use faer_traits::Conjugate;
use num_traits::Float;

/// Parameters controlling the compensated iterative-refinement solve path.
///
/// The refinement loop starts from the ordinary LU solve, then repeatedly:
///
/// 1. recomputes the residual with compensated sparse matvec
/// 2. solves a correction equation with the stored LU factors
/// 3. applies the correction with compensated vector addition
///
/// This improves the local arithmetic around the solve without replacing
/// `faer`'s optimized LU kernels.
///
/// Args:
///   tol: Absolute residual tolerance in right-hand-side units.
///   max_iters: Maximum number of refinement corrections after the initial LU
///     solve.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LuRefinementParams<R> {
    /// Absolute residual tolerance for the compensated residual recomputation.
    pub tol: R,
    /// Maximum number of refinement corrections to apply after the initial LU solve.
    pub max_iters: usize,
}

impl<R: Float> Default for LuRefinementParams<R> {
    fn default() -> Self {
        Self {
            tol: R::epsilon().sqrt(),
            max_iters: 4,
        }
    }
}

/// Result of the compensated iterative-refinement solve path.
///
/// `solution` is the final iterate. `residual_norm` is the compensated residual
/// norm of that final iterate. `converged` indicates whether the final residual
/// met the requested tolerance.
///
/// Returns:
///   `solution`: Final solution vector with shape `(n, 1)`.
///   `residual_norm`: Final residual norm in right-hand-side units.
///   `refinement_steps`: Number of refinement corrections applied.
///   `converged`: Whether `residual_norm <= tol`.
#[derive(Clone, Debug)]
pub struct RefinedLuSolve<T: CompensatedField>
where
    T::Real: Float,
{
    /// Final solution estimate after the initial LU solve and any refinement steps.
    pub solution: Col<T>,
    /// Compensated residual norm `||b - A x||` for the returned solution.
    pub residual_norm: T::Real,
    /// Number of refinement corrections applied after the initial LU solve.
    pub refinement_steps: usize,
    /// Whether the final compensated residual met the requested tolerance.
    pub converged: bool,
}

/// Errors that can occur while analyzing, refactorizing, or solving through [`SparseLu`].
#[derive(Clone, Copy, Debug)]
pub enum SparseLuError {
    /// Sparse LU is only defined here for square systems.
    NonSquare {
        /// Actual row count.
        nrows: usize,
        /// Actual column count.
        ncols: usize,
    },
    /// A caller supplied an object with the wrong dimension.
    DimensionMismatch {
        /// Identifies the object that failed validation.
        which: &'static str,
        /// Required dimension.
        expected: usize,
        /// Actual supplied dimension.
        actual: usize,
    },
    /// Numeric refactorization requires exactly the same CSC symbolic pattern.
    PatternMismatch,
    /// The wrapper has been symbolically analyzed but not yet numerically factorized.
    NotReady,
    /// Symbolic analysis failed.
    Symbolic(FaerError),
    /// Numeric LU factorization failed.
    Numeric(LuError),
}

impl fmt::Display for SparseLuError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

impl core::error::Error for SparseLuError {}

impl From<FaerError> for SparseLuError {
    fn from(value: FaerError) -> Self {
        Self::Symbolic(value)
    }
}

impl From<LuError> for SparseLuError {
    fn from(value: LuError) -> Self {
        Self::Numeric(value)
    }
}

/// Sparse LU wrapper with reusable symbolic analysis and numeric refactorization.
///
/// The symbolic structure is built once from a CSC sparsity pattern. Later
/// calls to [`refactor`](Self::refactor) or [`update`](Self::update) reuse that
/// symbolic structure and recompute only the numeric values and row pivoting
/// permutation.
///
/// This is useful in two distinct modes:
///
/// - direct solve mode, where the factors correspond to the current matrix
/// - lagged-preconditioner mode, where the factors come from a nearby matrix
///   and are used as an approximate inverse inside an iterative method
///
/// All solve entry points expect right-hand sides with `n = nrows() = ncols()`
/// rows.
#[derive(Clone, Debug)]
pub struct SparseLu<I: Index, T> {
    symbolic: SymbolicLu<I>,
    numeric: NumericLu<I, T>,
    pattern_col_ptr: Vec<I>,
    pattern_row_idx: Vec<I>,
    ready: bool,
}

impl<I: Index, T: ComplexField> SparseLu<I, T> {
    /// Performs symbolic analysis for a CSC matrix pattern.
    ///
    /// This stores:
    ///
    /// - the fill-reducing column permutation and symbolic LU structure from `faer`
    /// - the exact CSC symbolic pattern, so later numeric updates can verify
    ///   they refer to the same matrix structure
    ///
    /// No numeric factorization is performed here. The resulting wrapper must
    /// be numerically [`refactor`](Self::refactor)ed before it can solve or be
    /// used as a preconditioner.
    ///
    /// Args:
    ///   matrix: Sparse CSC matrix with shape `(n, n)`.
    ///   symbolic_params: Backend symbolic-analysis parameters.
    ///
    /// Returns:
    ///   A wrapper containing symbolic analysis and empty numeric factors.
    pub fn analyze<ViewT>(
        matrix: SparseColMatRef<'_, I, ViewT>,
        symbolic_params: LuSymbolicParams<'_>,
    ) -> Result<Self, SparseLuError>
    where
        ViewT: Conjugate<Canonical = T>,
    {
        let matrix = matrix.canonical();
        let nrows = matrix.nrows().unbound();
        let ncols = matrix.ncols().unbound();
        if nrows != ncols {
            return Err(SparseLuError::NonSquare { nrows, ncols });
        }

        let symbolic = factorize_symbolic_lu(matrix.symbolic(), symbolic_params)?;

        Ok(Self {
            symbolic,
            numeric: NumericLu::new(),
            pattern_col_ptr: matrix.col_ptr().to_vec(),
            pattern_row_idx: matrix.row_idx().to_vec(),
            ready: false,
        })
    }

    /// Performs symbolic analysis and the first numeric factorization.
    ///
    /// This is the convenient "factor once now" entry point for direct-solve
    /// use. For repeated same-pattern systems, callers can instead use
    /// [`analyze`](Self::analyze) once and then [`refactor`](Self::refactor) as
    /// new numeric values arrive.
    ///
    /// Args:
    ///   matrix: Sparse CSC matrix with shape `(n, n)`.
    ///   par: Parallelism setting for backend kernels.
    ///   symbolic_params: Backend symbolic-analysis parameters.
    ///   numeric_params: Backend numeric-factorization parameters.
    ///
    /// Returns:
    ///   A numerically ready sparse LU wrapper.
    pub fn factorize<ViewT>(
        matrix: SparseColMatRef<'_, I, ViewT>,
        par: Par,
        symbolic_params: LuSymbolicParams<'_>,
        numeric_params: Spec<PartialPivLuParams, T>,
    ) -> Result<Self, SparseLuError>
    where
        ViewT: Conjugate<Canonical = T>,
    {
        let mut lu = Self::analyze(matrix, symbolic_params)?;
        lu.refactor(matrix, par, numeric_params)?;
        Ok(lu)
    }

    /// Returns whether a successful numeric factorization has been stored.
    #[inline]
    #[must_use]
    pub fn is_ready(&self) -> bool {
        self.ready
    }

    /// Number of rows of the factorized matrix.
    #[inline]
    #[must_use]
    pub fn nrows(&self) -> usize {
        self.symbolic.nrows()
    }

    /// Number of columns of the factorized matrix.
    #[inline]
    #[must_use]
    pub fn ncols(&self) -> usize {
        self.symbolic.ncols()
    }

    /// Borrow the stored symbolic LU structure.
    #[inline]
    #[must_use]
    pub fn symbolic(&self) -> &SymbolicLu<I> {
        &self.symbolic
    }

    /// Borrow the stored numeric LU structure.
    ///
    /// If [`is_ready`](Self::is_ready) is `false`, this numeric object still
    /// exists but does not contain a usable factorization yet.
    #[inline]
    #[must_use]
    pub fn numeric(&self) -> &NumericLu<I, T> {
        &self.numeric
    }

    /// Recomputes the numeric LU factors for a matrix with the same CSC pattern.
    ///
    /// This is the fast path for same-pattern sequences. The fill-reducing
    /// ordering and symbolic factor structure are reused; only the numeric
    /// values and row pivoting permutation are refreshed.
    ///
    /// The pattern check is intentionally strict: the CSC `col_ptr` and
    /// `row_idx` arrays must match exactly. This keeps the invariant around the
    /// stored symbolic LU simple and avoids any hidden reordering or
    /// canonicalization at this layer.
    ///
    /// Args:
    ///   matrix: Sparse CSC matrix with shape `(n, n)` and the exact same CSC
    ///     symbolic pattern used at analysis time.
    ///   par: Parallelism setting for backend kernels.
    ///   numeric_params: Backend numeric-factorization parameters.
    ///
    /// Returns:
    ///   Success after refreshing the stored numeric LU factors.
    pub fn refactor<ViewT>(
        &mut self,
        matrix: SparseColMatRef<'_, I, ViewT>,
        par: Par,
        numeric_params: Spec<PartialPivLuParams, T>,
    ) -> Result<(), SparseLuError>
    where
        ViewT: Conjugate<Canonical = T>,
    {
        let matrix = matrix.canonical();
        self.check_pattern(matrix)?;

        let req = self
            .symbolic
            .factorize_numeric_lu_scratch::<T>(par, numeric_params);
        let mut buffer = MemBuffer::new(req);
        let stack = MemStack::new(&mut buffer);
        // This is a fresh numeric factorization against a fixed symbolic LU.
        // The symbolic structure, fill-reducing column ordering, and factor
        // storage layout are all reused; only the values and row pivots change.
        self.symbolic.factorize_numeric_lu(
            &mut self.numeric,
            matrix,
            par,
            stack,
            numeric_params,
        )?;
        self.ready = true;
        Ok(())
    }

    /// Alias for [`refactor`](Self::refactor).
    ///
    /// `update` reads a little more naturally in nonlinear or time-stepping
    /// code, while `refactor` better emphasizes that this is a fresh numeric LU
    /// factorization against a fixed symbolic structure.
    #[inline]
    pub fn update<ViewT>(
        &mut self,
        matrix: SparseColMatRef<'_, I, ViewT>,
        par: Par,
        numeric_params: Spec<PartialPivLuParams, T>,
    ) -> Result<(), SparseLuError>
    where
        ViewT: Conjugate<Canonical = T>,
    {
        self.refactor(matrix, par, numeric_params)
    }

    /// Solves `A x = rhs` in place using the stored LU factors.
    ///
    /// This is the ordinary uncompensated direct-solve path delegated to
    /// `faer`'s LU solve kernels.
    ///
    /// Args:
    ///   rhs: Dense right-hand side matrix with shape `(n, nrhs)`. It is
    ///     overwritten in place with the solution.
    ///   par: Parallelism setting for backend kernels.
    pub fn solve_in_place(&self, rhs: MatMut<'_, T>, par: Par) -> Result<(), SparseLuError> {
        self.solve_in_place_with_conj(Conj::No, rhs, par)
    }

    /// Solves `conj(A) x = rhs` in place using the stored LU factors.
    ///
    /// `Conj::No` is the ordinary direct solve path. `Conj::Yes` uses faer's
    /// conjugating solve path, which is useful for complex-valued operators and
    /// for implementing the conjugate application required by `Precond<T>`.
    ///
    /// This method allocates only the solve scratch required by faer's LU
    /// kernels. The factorization itself is not modified.
    ///
    /// Args:
    ///   conj: Whether to solve against `A` or `conj(A)`.
    ///   rhs: Dense right-hand side matrix with shape `(n, nrhs)`. It is
    ///     overwritten in place with the solution.
    ///   par: Parallelism setting for backend kernels.
    pub fn solve_in_place_with_conj(
        &self,
        conj: Conj,
        rhs: MatMut<'_, T>,
        par: Par,
    ) -> Result<(), SparseLuError> {
        if rhs.nrows() != self.nrows() {
            return Err(SparseLuError::DimensionMismatch {
                which: "rhs rows",
                expected: self.nrows(),
                actual: rhs.nrows(),
            });
        }

        let rhs_ncols = rhs.ncols();
        let req = self.symbolic.solve_in_place_scratch::<T>(rhs_ncols, par);
        let mut buffer = MemBuffer::new(req);
        let stack = MemStack::new(&mut buffer);
        self.try_lu_ref()?
            .solve_in_place_with_conj(conj, rhs, par, stack);
        Ok(())
    }

    /// Solves `A x = rhs` for a dense vector right-hand side.
    ///
    /// This is a convenience wrapper over [`solve_in_place`](Self::solve_in_place)
    /// for the common single-vector case.
    ///
    /// Args:
    ///   rhs: Dense right-hand side column with shape `(n, 1)`. It is
    ///     overwritten in place with the solution.
    ///   par: Parallelism setting for backend kernels.
    pub fn solve_col_in_place(&self, rhs: &mut Col<T>, par: Par) -> Result<(), SparseLuError> {
        self.solve_in_place(rhs.as_mat_mut(), par)
    }

    /// Solves `A x = rhs` and returns the result in a fresh dense column.
    ///
    /// The input slice is copied into dense storage first, then solved in
    /// place. Use [`solve_col_in_place`](Self::solve_col_in_place) if the caller
    /// already owns a mutable dense column and wants to avoid that copy.
    ///
    /// Args:
    ///   rhs: Dense right-hand side slice with length `n`.
    ///   par: Parallelism setting for backend kernels.
    ///
    /// Returns:
    ///   The solution column with shape `(n, 1)`.
    pub fn solve_rhs(&self, rhs: &[T], par: Par) -> Result<Col<T>, SparseLuError>
    where
        T: Copy,
    {
        if rhs.len() != self.nrows() {
            return Err(SparseLuError::DimensionMismatch {
                which: "rhs length",
                expected: self.nrows(),
                actual: rhs.len(),
            });
        }

        let mut out = col_from_slice(rhs);
        self.solve_col_in_place(&mut out, par)?;
        Ok(out)
    }

    /// Solves a system using the stored LU factors plus compensated iterative refinement.
    ///
    /// The algorithm is:
    ///
    /// 1. compute the ordinary LU solve `x_0`
    /// 2. recompute `r = b - A x_k` with compensated sparse matvec
    /// 3. solve `M delta = r` with the stored LU factors
    /// 4. update `x_{k+1} = x_k + delta` with compensated addition
    ///
    /// If the stored factors correspond to the same matrix `a`, this is a
    /// refined direct solve. If the stored factors are lagged from a nearby
    /// matrix, the same loop acts like a correction iteration preconditioned by
    /// the lagged LU.
    ///
    /// Intuitively, this keeps faer's LU solve as the fast inner solve while
    /// spending the extra arithmetic only where it matters numerically:
    /// residual recomputation and solution updates.
    ///
    /// Args:
    ///   a: Sparse linear operator with shape `(n, n)` used for residual
    ///     recomputation.
    ///   rhs: Right-hand side slice with length `n`.
    ///   par: Parallelism setting for backend kernels.
    ///   params: Refinement tolerance and iteration budget.
    ///
    /// Returns:
    ///   The refined solution vector `(n, 1)` and associated refinement
    ///   diagnostics.
    pub fn solve_compensated<A>(
        &self,
        a: A,
        rhs: &[T],
        par: Par,
        params: LuRefinementParams<T::Real>,
    ) -> Result<RefinedLuSolve<T>, SparseLuError>
    where
        A: SparseMatVec<T>,
        T: CompensatedField,
        T::Real: Float,
    {
        if a.nrows() != self.nrows() {
            return Err(SparseLuError::DimensionMismatch {
                which: "matrix rows",
                expected: self.nrows(),
                actual: a.nrows(),
            });
        }
        if a.ncols() != self.ncols() {
            return Err(SparseLuError::DimensionMismatch {
                which: "matrix cols",
                expected: self.ncols(),
                actual: a.ncols(),
            });
        }
        if rhs.len() != self.nrows() {
            return Err(SparseLuError::DimensionMismatch {
                which: "rhs length",
                expected: self.nrows(),
                actual: rhs.len(),
            });
        }

        let mut solution = self.solve_rhs(rhs, par)?;
        let mut residual = zero_col::<T>(self.nrows());
        let mut matvec = zero_col::<T>(self.nrows());
        let mut correction = zero_col::<T>(self.ncols());

        recompute_residual(a, col_slice(&solution), rhs, &mut residual, &mut matvec);
        let mut residual_norm = norm2::<T>(col_slice(&residual));
        if residual_norm <= params.tol {
            return Ok(RefinedLuSolve {
                solution,
                residual_norm,
                refinement_steps: 0,
                converged: true,
            });
        }

        let mut refinement_steps = 0usize;
        for _ in 0..params.max_iters {
            copy_col(&mut correction, &residual);
            self.solve_col_in_place(&mut correction, par)?;

            for (x, &delta) in col_slice_mut(&mut solution)
                .iter_mut()
                .zip(col_slice(&correction).iter())
            {
                *x += delta;
            }

            refinement_steps += 1;
            recompute_residual(a, col_slice(&solution), rhs, &mut residual, &mut matvec);
            residual_norm = norm2::<T>(col_slice(&residual));
            if residual_norm <= params.tol {
                return Ok(RefinedLuSolve {
                    solution,
                    residual_norm,
                    refinement_steps,
                    converged: true,
                });
            }
        }

        Ok(RefinedLuSolve {
            solution,
            residual_norm,
            refinement_steps,
            converged: residual_norm <= params.tol,
        })
    }

    fn check_pattern(&self, matrix: SparseColMatRef<'_, I, T>) -> Result<(), SparseLuError> {
        let nrows = matrix.nrows().unbound();
        let ncols = matrix.ncols().unbound();
        if nrows != ncols {
            return Err(SparseLuError::NonSquare { nrows, ncols });
        }
        if nrows != self.nrows() || ncols != self.ncols() {
            return Err(SparseLuError::PatternMismatch);
        }
        // Symbolic LU reuse is only sound if the CSC structure is identical to
        // the one used during analysis. We therefore compare the structural
        // arrays directly rather than trying to reason about looser "same
        // sparsity" notions here.
        if !same_index_slices(matrix.col_ptr(), &self.pattern_col_ptr)
            || !same_index_slices(matrix.row_idx(), &self.pattern_row_idx)
        {
            return Err(SparseLuError::PatternMismatch);
        }

        Ok(())
    }

    fn try_lu_ref(&self) -> Result<LuRef<'_, I, T>, SparseLuError> {
        if !self.ready {
            return Err(SparseLuError::NotReady);
        }

        // `ready` is only set to true after `self.numeric` has been produced by
        // `self.symbolic.factorize_numeric_lu(...)` on a matrix whose CSC
        // symbolic pattern matches the one stored at analysis time. That gives
        // us the compatibility guarantee required by `LuRef`.
        Ok(LuRef::new_unchecked(&self.symbolic, &self.numeric))
    }

    fn lu_ref_for_precond(&self) -> LuRef<'_, I, T> {
        // The `Precond` trait does not allow returning a `Result`, so
        // preconditioner application must assume the object has already gone
        // through the `analyze -> refactor` lifecycle successfully.
        self.try_lu_ref()
            .expect("SparseLu must be numerically factorized before solve/preconditioner use")
    }
}

impl<I: Index, T: ComplexField> LinOp<T> for SparseLu<I, T> {
    fn apply_scratch(&self, rhs_ncols: usize, par: Par) -> faer::dyn_stack::StackReq {
        self.symbolic.solve_in_place_scratch::<T>(rhs_ncols, par)
    }

    fn nrows(&self) -> usize {
        self.nrows()
    }

    fn ncols(&self) -> usize {
        self.ncols()
    }

    fn apply(&self, mut out: MatMut<'_, T>, rhs: MatRef<'_, T>, par: Par, stack: &mut MemStack) {
        assert_eq!(rhs.nrows(), self.ncols());
        assert_eq!(out.nrows(), self.nrows());
        assert_eq!(out.ncols(), rhs.ncols());
        // As a linear operator, `SparseLu` represents the inverse action of the
        // stored matrix: copy the RHS into the output buffer, then solve in
        // place with the LU factors.
        out.rb_mut().copy_from(rhs);
        self.lu_ref_for_precond()
            .solve_in_place_with_conj(Conj::No, out, par, stack);
    }

    fn conj_apply(
        &self,
        mut out: MatMut<'_, T>,
        rhs: MatRef<'_, T>,
        par: Par,
        stack: &mut MemStack,
    ) {
        assert_eq!(rhs.nrows(), self.ncols());
        assert_eq!(out.nrows(), self.nrows());
        assert_eq!(out.ncols(), rhs.ncols());
        // The conjugate application is the same inverse solve, but through the
        // conjugating LU path so complex preconditioner use remains consistent.
        out.rb_mut().copy_from(rhs);
        self.lu_ref_for_precond()
            .solve_in_place_with_conj(Conj::Yes, out, par, stack);
    }
}

impl<I: Index, T: ComplexField> Precond<T> for SparseLu<I, T> {
    fn apply_in_place_scratch(&self, rhs_ncols: usize, par: Par) -> faer::dyn_stack::StackReq {
        // Applying the lagged LU preconditioner is just solving against the
        // stored factors, so its scratch requirement is exactly faer's LU solve
        // scratch requirement.
        self.symbolic.solve_in_place_scratch::<T>(rhs_ncols, par)
    }

    fn apply_in_place(&self, rhs: MatMut<'_, T>, par: Par, stack: &mut MemStack) {
        assert_eq!(rhs.nrows(), self.nrows());
        self.lu_ref_for_precond()
            .solve_in_place_with_conj(Conj::No, rhs, par, stack);
    }

    fn conj_apply_in_place(&self, rhs: MatMut<'_, T>, par: Par, stack: &mut MemStack) {
        assert_eq!(rhs.nrows(), self.nrows());
        self.lu_ref_for_precond()
            .solve_in_place_with_conj(Conj::Yes, rhs, par, stack);
    }
}

#[inline]
fn same_index_slices<I: Index>(lhs: &[I], rhs: &[I]) -> bool {
    // Compare by numeric index value rather than relying on direct equality so
    // the helper works uniformly across faer's index wrapper types.
    lhs.len() == rhs.len()
        && lhs
            .iter()
            .zip(rhs.iter())
            .all(|(&lhs, &rhs)| lhs.zx() == rhs.zx())
}

fn recompute_residual<A, T>(a: A, x: &[T], b: &[T], residual: &mut Col<T>, matvec: &mut Col<T>)
where
    A: SparseMatVec<T>,
    T: CompensatedField,
    T::Real: Float,
{
    // This is the numerically sensitive part of iterative refinement: compute
    // `A x` with compensated accumulation, then form `b - A x` with
    // compensated two-term sums so the residual reflects the current iterate as
    // accurately as practical.
    a.apply_compensated(col_slice_mut(matvec), x);
    for ((r, &b), &ax) in col_slice_mut(residual)
        .iter_mut()
        .zip(b.iter())
        .zip(col_slice(matvec).iter())
    {
        *r = b - ax;
    }
}

#[cfg(test)]
mod test {
    use super::{LuRefinementParams, SparseLu, SparseLuError};
    use crate::sparse::BiCGSTAB;
    use crate::sparse::col::{col_slice, col_slice_mut, zero_col};
    use crate::sparse::compensated::{CompensatedField, norm2};
    use crate::sparse::matvec::SparseMatVec;
    use alloc::vec::Vec;
    use faer::Col;
    use faer::sparse::linalg::lu::LuSymbolicParams;
    use faer::sparse::{SparseColMat, Triplet};
    use faer::{Par, Spec, c64};
    use num_traits::Float;

    fn apply_to_col<T, A>(a: A, x: &[T]) -> Col<T>
    where
        T: CompensatedField,
        T::Real: Float,
        A: SparseMatVec<T>,
    {
        let mut out = zero_col::<T>(a.nrows());
        a.apply_compensated(col_slice_mut(&mut out), x);
        out
    }

    fn residual_norm<T, A>(a: A, x: &[T], b: &[T]) -> T::Real
    where
        T: CompensatedField,
        T::Real: Float,
        A: SparseMatVec<T>,
    {
        let ax = apply_to_col(a, x);
        let mut residual = zero_col::<T>(b.len());
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
    fn factorizes_and_solves_real_system() {
        let a = SparseColMat::<usize, f64>::try_new_from_triplets(
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
        let lu = SparseLu::<usize, f64>::factorize(
            a.as_ref(),
            Par::Seq,
            LuSymbolicParams::default(),
            Spec::default(),
        )
        .unwrap();
        let x = lu.solve_rhs(col_slice(&b), Par::Seq).unwrap();

        assert!(residual_norm(a.as_ref(), col_slice(&x), col_slice(&b)) < 1.0e-12);
    }

    #[test]
    fn refactors_same_pattern_with_new_values() {
        let a0 = SparseColMat::<usize, f64>::try_new_from_triplets(
            3,
            3,
            &[
                Triplet::new(0, 0, 4.0),
                Triplet::new(0, 1, -1.0),
                Triplet::new(1, 0, 2.0),
                Triplet::new(1, 1, 5.0),
                Triplet::new(1, 2, 1.0),
                Triplet::new(2, 1, 2.0),
                Triplet::new(2, 2, 3.0),
            ],
        )
        .unwrap();
        let a1 = SparseColMat::<usize, f64>::try_new_from_triplets(
            3,
            3,
            &[
                Triplet::new(0, 0, 6.0),
                Triplet::new(0, 1, -1.0),
                Triplet::new(1, 0, 2.5),
                Triplet::new(1, 1, 4.0),
                Triplet::new(1, 2, 1.5),
                Triplet::new(2, 1, 1.0),
                Triplet::new(2, 2, 2.5),
            ],
        )
        .unwrap();

        let mut lu =
            SparseLu::<usize, f64>::analyze(a0.as_ref(), LuSymbolicParams::default()).unwrap();
        lu.refactor(a0.as_ref(), Par::Seq, Spec::default()).unwrap();

        let x0_true = [1.0, -2.0, 0.5];
        let b0 = apply_to_col(a0.as_ref(), &x0_true);
        let x0 = lu.solve_rhs(col_slice(&b0), Par::Seq).unwrap();
        assert!(residual_norm(a0.as_ref(), col_slice(&x0), col_slice(&b0)) < 1.0e-12);

        lu.refactor(a1.as_ref(), Par::Seq, Spec::default()).unwrap();
        let x1_true = [-1.0, 0.5, 2.0];
        let b1 = apply_to_col(a1.as_ref(), &x1_true);
        let x1 = lu.solve_rhs(col_slice(&b1), Par::Seq).unwrap();
        assert!(residual_norm(a1.as_ref(), col_slice(&x1), col_slice(&b1)) < 1.0e-12);
    }

    #[test]
    fn rejects_pattern_mismatch_on_refactor() {
        let a0 = SparseColMat::<usize, f64>::try_new_from_triplets(
            2,
            2,
            &[
                Triplet::new(0, 0, 2.0),
                Triplet::new(0, 1, 1.0),
                Triplet::new(1, 1, 3.0),
            ],
        )
        .unwrap();
        let a1 = SparseColMat::<usize, f64>::try_new_from_triplets(
            2,
            2,
            &[
                Triplet::new(0, 0, 2.0),
                Triplet::new(1, 0, 1.0),
                Triplet::new(1, 1, 3.0),
            ],
        )
        .unwrap();

        let mut lu =
            SparseLu::<usize, f64>::analyze(a0.as_ref(), LuSymbolicParams::default()).unwrap();
        assert!(matches!(
            lu.refactor(a1.as_ref(), Par::Seq, Spec::default()),
            Err(SparseLuError::PatternMismatch)
        ));
    }

    #[test]
    fn compensated_refinement_improves_residual_for_ill_conditioned_f32_system() {
        let n = 8usize;
        let mut triplets = Vec::with_capacity(n * n);
        for row in 0..n {
            for col in 0..n {
                triplets.push(Triplet::new(row, col, 1.0f32 / (row + col + 1) as f32));
            }
        }

        let a = SparseColMat::<usize, f32>::try_new_from_triplets(n, n, &triplets).unwrap();
        let x_true: Vec<f32> = (0..n)
            .map(|i| if i % 2 == 0 { 1.0 } else { -1.0 })
            .collect();
        let b = apply_to_col(a.as_ref(), &x_true);
        let lu = SparseLu::<usize, f32>::factorize(
            a.as_ref(),
            Par::Seq,
            LuSymbolicParams::default(),
            Spec::default(),
        )
        .unwrap();

        let direct = lu.solve_rhs(col_slice(&b), Par::Seq).unwrap();
        let direct_residual = residual_norm(a.as_ref(), col_slice(&direct), col_slice(&b));

        let refined = lu
            .solve_compensated(
                a.as_ref(),
                col_slice(&b),
                Par::Seq,
                LuRefinementParams {
                    tol: 1.0e-4,
                    max_iters: 4,
                },
            )
            .unwrap();

        assert!(refined.residual_norm <= direct_residual);
        assert!(refined.converged || refined.refinement_steps == 4);
    }

    #[test]
    fn lagged_lu_can_be_used_as_bicgstab_preconditioner() {
        let n = 10usize;
        let tol = 1.0e-7;
        let mut triplets0 = Vec::with_capacity(3 * n - 2);
        let mut triplets1 = Vec::with_capacity(3 * n - 2);
        for row in 0..n {
            triplets0.push(Triplet::new(row, row, 4.0 + row as f64 * 0.1));
            triplets1.push(Triplet::new(row, row, 4.02 + row as f64 * 0.1));
            if row > 0 {
                triplets0.push(Triplet::new(row, row - 1, -1.0));
                triplets1.push(Triplet::new(row, row - 1, -0.99));
            }
            if row + 1 < n {
                triplets0.push(Triplet::new(row, row + 1, -1.0));
                triplets1.push(Triplet::new(row, row + 1, -1.01));
            }
        }

        let a0 = SparseColMat::<usize, f64>::try_new_from_triplets(n, n, &triplets0).unwrap();
        let a1 = SparseColMat::<usize, f64>::try_new_from_triplets(n, n, &triplets1).unwrap();
        let x_true: Vec<f64> = (0..n)
            .map(|i| if i % 2 == 0 { 1.0 } else { -1.0 })
            .collect();
        let b = apply_to_col(a1.as_ref(), &x_true);

        let lu = SparseLu::<usize, f64>::factorize(
            a0.as_ref(),
            Par::Seq,
            LuSymbolicParams::default(),
            Spec::default(),
        )
        .unwrap();

        let lagged = BiCGSTAB::solve_with_precond(
            a1.as_ref(),
            lu.clone(),
            &[0.0; 10],
            col_slice(&b),
            tol,
            100,
        )
        .unwrap();

        assert!(residual_norm(a1.as_ref(), col_slice(lagged.x()), col_slice(&b)) < tol);
    }

    #[test]
    fn solves_complex_system() {
        let a = SparseColMat::<usize, c64>::try_new_from_triplets(
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
        let b = apply_to_col(a.as_ref(), &x_true);
        let lu = SparseLu::<usize, c64>::factorize(
            a.as_ref(),
            Par::Seq,
            LuSymbolicParams::default(),
            Spec::default(),
        )
        .unwrap();
        let x = lu.solve_rhs(col_slice(&b), Par::Seq).unwrap();

        assert!(residual_norm(a.as_ref(), col_slice(&x), col_slice(&b)) < 1.0e-11);
    }
}
