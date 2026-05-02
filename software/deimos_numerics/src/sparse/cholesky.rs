//! Sparse Cholesky wrappers around `faer`'s staged symbolic and numeric APIs.
//!
//! These wrappers intentionally mirror [`crate::sparse::SparseLu`] at the API
//! level:
//!
//! - symbolic analysis is performed once
//! - numeric values are refactorized in place when the CSC pattern stays fixed
//! - the same stored factors can be used either as direct solvers or as
//!   `Precond` implementations inside iterative methods
//!
//! The main difference from LU is that `faer`'s sparse Cholesky routines expect
//! the caller to own the numeric factor storage directly, so this module keeps
//! an owned `Vec<T>` of factor values and rebuilds short-lived `LltRef` /
//! `LdltRef` views when solving.
//!
//! # Two Intuitions
//!
//! 1. **Factorization view.** Cholesky solves a sparse linear system by
//!    factoring a structured positive-definite or symmetric-indefinite matrix
//!    once and then reusing the factors.
//! 2. **Staged-wrapper view.** This module is also about preserving `faer`'s
//!    symbolic/numeric split so repeated solves on the same sparsity pattern do
//!    not redo the expensive symbolic work.
//!
//! # Glossary
//!
//! - **LLT / LDLT:** Cholesky-style factorizations for SPD and symmetric
//!   indefinite systems.
//! - **Symbolic analysis:** Pattern-only phase that allocates and orders the
//!   factorization.
//! - **Numeric refactorization:** Reuse of the symbolic analysis with new
//!   numeric values.
//!
//! # Mathematical Formulation
//!
//! The solver factors the sparse system matrix into triangular pieces and then
//! applies forward/back substitution, optionally reused as a preconditioner.
//!
//! # Implementation Notes
//!
//! - Numeric values are owned in a Rust `Vec<T>` so refactorization can happen
//!   in place.
//! - The same wrapper acts as both direct solver and iterative preconditioner.

use super::col::col_from_slice;
use super::precond::Precond;
use alloc::vec::Vec;
use core::fmt;
use faer::dyn_stack::{MemBuffer, MemStack};
use faer::linalg::cholesky::ldlt::factor::{LdltError, LdltParams, LdltRegularization};
use faer::linalg::cholesky::llt::factor::{LltError, LltParams, LltRegularization};
use faer::matrix_free::LinOp;
use faer::prelude::ReborrowMut;
use faer::sparse::FaerError;
use faer::sparse::SparseColMatRef;
use faer::sparse::linalg::cholesky::{
    CholeskySymbolicParams, LdltRef, LltRef, SymbolicCholesky, SymmetricOrdering,
    factorize_symbolic_cholesky,
};
use faer::{Col, Conj, Index, MatMut, MatRef, Par, Side, Spec, Unbind};
use faer_traits::ComplexField;
use faer_traits::Conjugate;
use faer_traits::math_utils::zero;

/// Errors that can occur while analyzing, factorizing, or solving through [`SparseLlt`].
#[derive(Clone, Copy, Debug)]
pub enum SparseLltError {
    /// Sparse LLT is only defined here for square systems.
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
    /// Numeric LLT factorization failed.
    Numeric(LltError),
}

impl fmt::Display for SparseLltError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

impl core::error::Error for SparseLltError {}

impl From<FaerError> for SparseLltError {
    fn from(value: FaerError) -> Self {
        Self::Symbolic(value)
    }
}

impl From<LltError> for SparseLltError {
    fn from(value: LltError) -> Self {
        Self::Numeric(value)
    }
}

/// Errors that can occur while analyzing, factorizing, or solving through [`SparseLdlt`].
#[derive(Clone, Copy, Debug)]
pub enum SparseLdltError {
    /// Sparse LDLT is only defined here for square systems.
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
    /// Numeric LDLT factorization failed.
    Numeric(LdltError),
}

impl fmt::Display for SparseLdltError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

impl core::error::Error for SparseLdltError {}

impl From<FaerError> for SparseLdltError {
    fn from(value: FaerError) -> Self {
        Self::Symbolic(value)
    }
}

impl From<LdltError> for SparseLdltError {
    fn from(value: LdltError) -> Self {
        Self::Numeric(value)
    }
}

/// Sparse symmetric positive-definite factorization wrapper.
///
/// This stores:
///
/// - the symbolic Cholesky structure chosen during analysis
/// - the exact CSC symbolic pattern used for that analysis
/// - the caller-owned numeric `L` values filled by `faer`
///
/// The wrapper can then:
///
/// - refactor quickly when only the matrix values change
/// - solve directly with the stored factorization
/// - act as a right or left preconditioner through `Precond<T>`
///
/// All solve entry points expect right-hand sides with `n = nrows() = ncols()`
/// rows.
#[derive(Debug)]
pub struct SparseLlt<I: Index, T> {
    symbolic: SymbolicCholesky<I>,
    l_values: Vec<T>,
    pattern_col_ptr: Vec<I>,
    pattern_row_idx: Vec<I>,
    side: Side,
    ready: bool,
}

impl<I: Index, T: ComplexField> SparseLlt<I, T> {
    /// Performs symbolic analysis for a CSC self-adjoint matrix pattern.
    ///
    /// Args:
    ///   matrix: Sparse CSC self-adjoint matrix with shape `(n, n)`.
    ///   side: Which triangle of `matrix` is interpreted as self-adjoint data.
    ///   ordering: Symbolic ordering strategy used during analysis.
    ///   symbolic_params: Backend symbolic-analysis parameters.
    ///
    /// Returns:
    ///   A wrapper containing symbolic analysis and empty numeric storage.
    pub fn analyze<ViewT>(
        matrix: SparseColMatRef<'_, I, ViewT>,
        side: Side,
        ordering: SymmetricOrdering<'_, I>,
        symbolic_params: CholeskySymbolicParams<'_>,
    ) -> Result<Self, SparseLltError>
    where
        ViewT: Conjugate<Canonical = T>,
    {
        let matrix = matrix.canonical();
        let nrows = matrix.nrows().unbound();
        let ncols = matrix.ncols().unbound();
        if nrows != ncols {
            return Err(SparseLltError::NonSquare { nrows, ncols });
        }

        let symbolic =
            factorize_symbolic_cholesky(matrix.symbolic(), side, ordering, symbolic_params)?;
        let len_val = symbolic.len_val();

        Ok(Self {
            symbolic,
            l_values: core::iter::repeat_with(zero::<T>).take(len_val).collect(),
            pattern_col_ptr: matrix.col_ptr().to_vec(),
            pattern_row_idx: matrix.row_idx().to_vec(),
            side,
            ready: false,
        })
    }

    /// Performs symbolic analysis and the first numeric factorization.
    ///
    /// Args:
    ///   matrix: Sparse CSC self-adjoint matrix with shape `(n, n)`.
    ///   side: Which triangle of `matrix` is interpreted as self-adjoint data.
    ///   ordering: Symbolic ordering strategy used during analysis.
    ///   symbolic_params: Backend symbolic-analysis parameters.
    ///   regularization: Numeric regularization policy for the LLT factorization.
    ///   par: Parallelism setting for backend kernels.
    ///   numeric_params: Backend numeric-factorization parameters.
    ///
    /// Returns:
    ///   A numerically ready sparse LLT factorization wrapper.
    pub fn factorize<ViewT>(
        matrix: SparseColMatRef<'_, I, ViewT>,
        side: Side,
        ordering: SymmetricOrdering<'_, I>,
        symbolic_params: CholeskySymbolicParams<'_>,
        regularization: LltRegularization<T::Real>,
        par: Par,
        numeric_params: Spec<LltParams, T>,
    ) -> Result<Self, SparseLltError>
    where
        ViewT: Conjugate<Canonical = T>,
    {
        let mut llt = Self::analyze(matrix, side, ordering, symbolic_params)?;
        llt.refactor(matrix, regularization, par, numeric_params)?;
        Ok(llt)
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

    /// Which triangle of the input matrix is interpreted as self-adjoint data.
    #[inline]
    #[must_use]
    pub fn side(&self) -> Side {
        self.side
    }

    /// Borrow the stored symbolic Cholesky structure.
    #[inline]
    #[must_use]
    pub fn symbolic(&self) -> &SymbolicCholesky<I> {
        &self.symbolic
    }

    /// Borrow the stored numeric factor values in the backend storage layout.
    #[inline]
    #[must_use]
    pub fn values(&self) -> &[T] {
        &self.l_values
    }

    /// Recomputes the numeric LLT factors for a matrix with the same CSC pattern.
    ///
    /// Args:
    ///   matrix: Sparse CSC self-adjoint matrix with shape `(n, n)` and the
    ///     exact same CSC symbolic pattern used at analysis time.
    ///   regularization: Numeric regularization policy for the LLT factorization.
    ///   par: Parallelism setting for backend kernels.
    ///   numeric_params: Backend numeric-factorization parameters.
    ///
    /// Returns:
    ///   Success after refreshing the stored numeric factors.
    pub fn refactor<ViewT>(
        &mut self,
        matrix: SparseColMatRef<'_, I, ViewT>,
        regularization: LltRegularization<T::Real>,
        par: Par,
        numeric_params: Spec<LltParams, T>,
    ) -> Result<(), SparseLltError>
    where
        ViewT: Conjugate<Canonical = T>,
    {
        let matrix = matrix.canonical();
        self.ensure_compatible_pattern(matrix)?;

        let req = self
            .symbolic
            .factorize_numeric_llt_scratch::<T>(par, numeric_params);
        let mut buffer = MemBuffer::new(req);
        let stack = MemStack::new(&mut buffer);
        self.symbolic.factorize_numeric_llt(
            &mut self.l_values,
            matrix,
            self.side,
            regularization,
            par,
            stack,
            numeric_params,
        )?;
        self.ready = true;
        Ok(())
    }

    /// Alias for [`refactor`](Self::refactor).
    #[inline]
    pub fn update<ViewT>(
        &mut self,
        matrix: SparseColMatRef<'_, I, ViewT>,
        regularization: LltRegularization<T::Real>,
        par: Par,
        numeric_params: Spec<LltParams, T>,
    ) -> Result<(), SparseLltError>
    where
        ViewT: Conjugate<Canonical = T>,
    {
        self.refactor(matrix, regularization, par, numeric_params)
    }

    /// Solves `A x = rhs` in place using the stored LLT factors.
    ///
    /// Args:
    ///   rhs: Dense right-hand side matrix with shape `(n, nrhs)`. It is
    ///     overwritten in place with the solution.
    ///   par: Parallelism setting for backend kernels.
    pub fn solve_in_place(&self, rhs: MatMut<'_, T>, par: Par) -> Result<(), SparseLltError> {
        self.solve_in_place_with_conj(Conj::No, rhs, par)
    }

    /// Solves `conj(A) x = rhs` in place using the stored LLT factors.
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
    ) -> Result<(), SparseLltError> {
        if rhs.nrows() != self.nrows() {
            return Err(SparseLltError::DimensionMismatch {
                which: "rhs rows",
                expected: self.nrows(),
                actual: rhs.nrows(),
            });
        }

        let rhs_ncols = rhs.ncols();
        let req = self.symbolic.solve_in_place_scratch::<T>(rhs_ncols, par);
        let mut buffer = MemBuffer::new(req);
        let stack = MemStack::new(&mut buffer);
        self.try_llt_ref()?
            .solve_in_place_with_conj(conj, rhs, par, stack);
        Ok(())
    }

    /// Solves `A x = rhs` in place for a single dense column.
    ///
    /// Args:
    ///   rhs: Dense right-hand side column with shape `(n, 1)`. It is
    ///     overwritten in place with the solution.
    ///   par: Parallelism setting for backend kernels.
    pub fn solve_col_in_place(&self, rhs: &mut Col<T>, par: Par) -> Result<(), SparseLltError> {
        self.solve_in_place(rhs.as_mat_mut(), par)
    }

    /// Solves `A x = rhs` and returns the result in a fresh dense column.
    ///
    /// Args:
    ///   rhs: Dense right-hand side slice with length `n`.
    ///   par: Parallelism setting for backend kernels.
    ///
    /// Returns:
    ///   The solution column with shape `(n, 1)`.
    pub fn solve_rhs(&self, rhs: &[T], par: Par) -> Result<Col<T>, SparseLltError>
    where
        T: Copy,
    {
        if rhs.len() != self.nrows() {
            return Err(SparseLltError::DimensionMismatch {
                which: "rhs length",
                expected: self.nrows(),
                actual: rhs.len(),
            });
        }

        let mut out = col_from_slice(rhs);
        self.solve_col_in_place(&mut out, par)?;
        Ok(out)
    }

    fn ensure_compatible_pattern(
        &self,
        matrix: SparseColMatRef<'_, I, T>,
    ) -> Result<(), SparseLltError> {
        let nrows = matrix.nrows().unbound();
        let ncols = matrix.ncols().unbound();
        if nrows != self.nrows() || ncols != self.ncols() {
            return Err(SparseLltError::PatternMismatch);
        }
        if !same_index_slices(matrix.col_ptr(), &self.pattern_col_ptr)
            || !same_index_slices(matrix.row_idx(), &self.pattern_row_idx)
        {
            return Err(SparseLltError::PatternMismatch);
        }

        Ok(())
    }

    fn try_llt_ref(&self) -> Result<LltRef<'_, I, T>, SparseLltError> {
        if !self.ready {
            return Err(SparseLltError::NotReady);
        }
        Ok(LltRef::new(&self.symbolic, &self.l_values))
    }

    fn llt_ref_for_precond(&self) -> LltRef<'_, I, T> {
        self.try_llt_ref()
            .expect("SparseLlt must be numerically factorized before solve/preconditioner use")
    }
}

impl<I: Index, T: ComplexField> LinOp<T> for SparseLlt<I, T> {
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
        out.rb_mut().copy_from(rhs);
        self.llt_ref_for_precond()
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
        out.rb_mut().copy_from(rhs);
        self.llt_ref_for_precond()
            .solve_in_place_with_conj(Conj::Yes, out, par, stack);
    }
}

impl<I: Index, T: ComplexField> Precond<T> for SparseLlt<I, T> {
    fn apply_in_place_scratch(&self, rhs_ncols: usize, par: Par) -> faer::dyn_stack::StackReq {
        self.symbolic.solve_in_place_scratch::<T>(rhs_ncols, par)
    }

    fn apply_in_place(&self, rhs: MatMut<'_, T>, par: Par, stack: &mut MemStack) {
        assert_eq!(rhs.nrows(), self.nrows());
        self.llt_ref_for_precond()
            .solve_in_place_with_conj(Conj::No, rhs, par, stack);
    }

    fn conj_apply_in_place(&self, rhs: MatMut<'_, T>, par: Par, stack: &mut MemStack) {
        assert_eq!(rhs.nrows(), self.nrows());
        self.llt_ref_for_precond()
            .solve_in_place_with_conj(Conj::Yes, rhs, par, stack);
    }
}

/// Sparse symmetric / Hermitian LDLT factorization wrapper.
///
/// Compared with [`SparseLlt`], this path is more flexible: it can factorize
/// indefinite self-adjoint systems and accepts an explicit dynamic
/// regularization policy through [`LdltRegularization`].
///
/// All solve entry points expect right-hand sides with `n = nrows() = ncols()`
/// rows.
#[derive(Debug)]
pub struct SparseLdlt<I: Index, T> {
    symbolic: SymbolicCholesky<I>,
    l_values: Vec<T>,
    pattern_col_ptr: Vec<I>,
    pattern_row_idx: Vec<I>,
    side: Side,
    ready: bool,
}

impl<I: Index, T: ComplexField> SparseLdlt<I, T> {
    /// Performs symbolic analysis for a CSC self-adjoint matrix pattern.
    ///
    /// Args:
    ///   matrix: Sparse CSC self-adjoint matrix with shape `(n, n)`.
    ///   side: Which triangle of `matrix` is interpreted as self-adjoint data.
    ///   ordering: Symbolic ordering strategy used during analysis.
    ///   symbolic_params: Backend symbolic-analysis parameters.
    ///
    /// Returns:
    ///   A wrapper containing symbolic analysis and empty numeric storage.
    pub fn analyze<ViewT>(
        matrix: SparseColMatRef<'_, I, ViewT>,
        side: Side,
        ordering: SymmetricOrdering<'_, I>,
        symbolic_params: CholeskySymbolicParams<'_>,
    ) -> Result<Self, SparseLdltError>
    where
        ViewT: Conjugate<Canonical = T>,
    {
        let matrix = matrix.canonical();
        let nrows = matrix.nrows().unbound();
        let ncols = matrix.ncols().unbound();
        if nrows != ncols {
            return Err(SparseLdltError::NonSquare { nrows, ncols });
        }

        let symbolic =
            factorize_symbolic_cholesky(matrix.symbolic(), side, ordering, symbolic_params)?;
        let len_val = symbolic.len_val();

        Ok(Self {
            symbolic,
            l_values: core::iter::repeat_with(zero::<T>).take(len_val).collect(),
            pattern_col_ptr: matrix.col_ptr().to_vec(),
            pattern_row_idx: matrix.row_idx().to_vec(),
            side,
            ready: false,
        })
    }

    /// Performs symbolic analysis and the first numeric LDLT factorization.
    ///
    /// Args:
    ///   matrix: Sparse CSC self-adjoint matrix with shape `(n, n)`.
    ///   side: Which triangle of `matrix` is interpreted as self-adjoint data.
    ///   ordering: Symbolic ordering strategy used during analysis.
    ///   symbolic_params: Backend symbolic-analysis parameters.
    ///   regularization: Dynamic regularization policy for the LDLT factorization.
    ///   par: Parallelism setting for backend kernels.
    ///   numeric_params: Backend numeric-factorization parameters.
    ///
    /// Returns:
    ///   A numerically ready sparse LDLT factorization wrapper.
    pub fn factorize<ViewT>(
        matrix: SparseColMatRef<'_, I, ViewT>,
        side: Side,
        ordering: SymmetricOrdering<'_, I>,
        symbolic_params: CholeskySymbolicParams<'_>,
        regularization: LdltRegularization<'_, T::Real>,
        par: Par,
        numeric_params: Spec<LdltParams, T>,
    ) -> Result<Self, SparseLdltError>
    where
        ViewT: Conjugate<Canonical = T>,
    {
        let mut ldlt = Self::analyze(matrix, side, ordering, symbolic_params)?;
        ldlt.refactor(matrix, regularization, par, numeric_params)?;
        Ok(ldlt)
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

    /// Which triangle of the input matrix is interpreted as self-adjoint data.
    #[inline]
    #[must_use]
    pub fn side(&self) -> Side {
        self.side
    }

    /// Borrow the stored symbolic Cholesky structure.
    #[inline]
    #[must_use]
    pub fn symbolic(&self) -> &SymbolicCholesky<I> {
        &self.symbolic
    }

    /// Borrow the stored numeric factor values in the backend storage layout.
    #[inline]
    #[must_use]
    pub fn values(&self) -> &[T] {
        &self.l_values
    }

    /// Recomputes the numeric LDLT factors for a matrix with the same CSC pattern.
    ///
    /// Args:
    ///   matrix: Sparse CSC self-adjoint matrix with shape `(n, n)` and the
    ///     exact same CSC symbolic pattern used at analysis time.
    ///   regularization: Dynamic regularization policy for the LDLT factorization.
    ///   par: Parallelism setting for backend kernels.
    ///   numeric_params: Backend numeric-factorization parameters.
    ///
    /// Returns:
    ///   Success after refreshing the stored numeric factors.
    pub fn refactor<ViewT>(
        &mut self,
        matrix: SparseColMatRef<'_, I, ViewT>,
        regularization: LdltRegularization<'_, T::Real>,
        par: Par,
        numeric_params: Spec<LdltParams, T>,
    ) -> Result<(), SparseLdltError>
    where
        ViewT: Conjugate<Canonical = T>,
    {
        let matrix = matrix.canonical();
        self.ensure_compatible_pattern(matrix)?;

        let req = self
            .symbolic
            .factorize_numeric_ldlt_scratch::<T>(par, numeric_params);
        let mut buffer = MemBuffer::new(req);
        let stack = MemStack::new(&mut buffer);
        self.symbolic.factorize_numeric_ldlt(
            &mut self.l_values,
            matrix,
            self.side,
            regularization,
            par,
            stack,
            numeric_params,
        )?;
        self.ready = true;
        Ok(())
    }

    /// Alias for [`refactor`](Self::refactor).
    #[inline]
    pub fn update<ViewT>(
        &mut self,
        matrix: SparseColMatRef<'_, I, ViewT>,
        regularization: LdltRegularization<'_, T::Real>,
        par: Par,
        numeric_params: Spec<LdltParams, T>,
    ) -> Result<(), SparseLdltError>
    where
        ViewT: Conjugate<Canonical = T>,
    {
        self.refactor(matrix, regularization, par, numeric_params)
    }

    /// Solves `A x = rhs` in place using the stored LDLT factors.
    ///
    /// Args:
    ///   rhs: Dense right-hand side matrix with shape `(n, nrhs)`. It is
    ///     overwritten in place with the solution.
    ///   par: Parallelism setting for backend kernels.
    pub fn solve_in_place(&self, rhs: MatMut<'_, T>, par: Par) -> Result<(), SparseLdltError> {
        self.solve_in_place_with_conj(Conj::No, rhs, par)
    }

    /// Solves `conj(A) x = rhs` in place using the stored LDLT factors.
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
    ) -> Result<(), SparseLdltError> {
        if rhs.nrows() != self.nrows() {
            return Err(SparseLdltError::DimensionMismatch {
                which: "rhs rows",
                expected: self.nrows(),
                actual: rhs.nrows(),
            });
        }

        let rhs_ncols = rhs.ncols();
        let req = self.symbolic.solve_in_place_scratch::<T>(rhs_ncols, par);
        let mut buffer = MemBuffer::new(req);
        let stack = MemStack::new(&mut buffer);
        self.try_ldlt_ref()?
            .solve_in_place_with_conj(conj, rhs, par, stack);
        Ok(())
    }

    /// Solves `A x = rhs` in place for a single dense column.
    ///
    /// Args:
    ///   rhs: Dense right-hand side column with shape `(n, 1)`. It is
    ///     overwritten in place with the solution.
    ///   par: Parallelism setting for backend kernels.
    pub fn solve_col_in_place(&self, rhs: &mut Col<T>, par: Par) -> Result<(), SparseLdltError> {
        self.solve_in_place(rhs.as_mat_mut(), par)
    }

    /// Solves `A x = rhs` and returns the result in a fresh dense column.
    ///
    /// Args:
    ///   rhs: Dense right-hand side slice with length `n`.
    ///   par: Parallelism setting for backend kernels.
    ///
    /// Returns:
    ///   The solution column with shape `(n, 1)`.
    pub fn solve_rhs(&self, rhs: &[T], par: Par) -> Result<Col<T>, SparseLdltError>
    where
        T: Copy,
    {
        if rhs.len() != self.nrows() {
            return Err(SparseLdltError::DimensionMismatch {
                which: "rhs length",
                expected: self.nrows(),
                actual: rhs.len(),
            });
        }

        let mut out = col_from_slice(rhs);
        self.solve_col_in_place(&mut out, par)?;
        Ok(out)
    }

    fn ensure_compatible_pattern(
        &self,
        matrix: SparseColMatRef<'_, I, T>,
    ) -> Result<(), SparseLdltError> {
        let nrows = matrix.nrows().unbound();
        let ncols = matrix.ncols().unbound();
        if nrows != self.nrows() || ncols != self.ncols() {
            return Err(SparseLdltError::PatternMismatch);
        }
        if !same_index_slices(matrix.col_ptr(), &self.pattern_col_ptr)
            || !same_index_slices(matrix.row_idx(), &self.pattern_row_idx)
        {
            return Err(SparseLdltError::PatternMismatch);
        }

        Ok(())
    }

    fn try_ldlt_ref(&self) -> Result<LdltRef<'_, I, T>, SparseLdltError> {
        if !self.ready {
            return Err(SparseLdltError::NotReady);
        }
        Ok(LdltRef::new(&self.symbolic, &self.l_values))
    }

    fn ldlt_ref_for_precond(&self) -> LdltRef<'_, I, T> {
        self.try_ldlt_ref()
            .expect("SparseLdlt must be numerically factorized before solve/preconditioner use")
    }
}

impl<I: Index, T: ComplexField> LinOp<T> for SparseLdlt<I, T> {
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
        out.rb_mut().copy_from(rhs);
        self.ldlt_ref_for_precond()
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
        out.rb_mut().copy_from(rhs);
        self.ldlt_ref_for_precond()
            .solve_in_place_with_conj(Conj::Yes, out, par, stack);
    }
}

impl<I: Index, T: ComplexField> Precond<T> for SparseLdlt<I, T> {
    fn apply_in_place_scratch(&self, rhs_ncols: usize, par: Par) -> faer::dyn_stack::StackReq {
        self.symbolic.solve_in_place_scratch::<T>(rhs_ncols, par)
    }

    fn apply_in_place(&self, rhs: MatMut<'_, T>, par: Par, stack: &mut MemStack) {
        assert_eq!(rhs.nrows(), self.nrows());
        self.ldlt_ref_for_precond()
            .solve_in_place_with_conj(Conj::No, rhs, par, stack);
    }

    fn conj_apply_in_place(&self, rhs: MatMut<'_, T>, par: Par, stack: &mut MemStack) {
        assert_eq!(rhs.nrows(), self.nrows());
        self.ldlt_ref_for_precond()
            .solve_in_place_with_conj(Conj::Yes, rhs, par, stack);
    }
}

#[inline]
fn same_index_slices<I: Index>(lhs: &[I], rhs: &[I]) -> bool {
    lhs.len() == rhs.len()
        && lhs
            .iter()
            .zip(rhs.iter())
            .all(|(&lhs, &rhs)| lhs.zx() == rhs.zx())
}

#[cfg(test)]
mod test {
    use super::{SparseLdlt, SparseLdltError, SparseLlt, SparseLltError};
    use crate::sparse::BiCGSTAB;
    use crate::sparse::col::{col_slice, col_slice_mut, zero_col};
    use crate::sparse::compensated::norm2;
    use crate::sparse::matvec::SparseMatVec;
    use faer::linalg::cholesky::ldlt::factor::LdltRegularization;
    use faer::linalg::cholesky::llt::factor::LltRegularization;
    use faer::sparse::linalg::cholesky::{CholeskySymbolicParams, SymmetricOrdering};
    use faer::sparse::{SparseColMat, Triplet};
    use faer::{Par, Side, Spec, c64};

    fn residual_norm<T, A>(a: A, x: &[T], b: &[T]) -> T::Real
    where
        T: crate::sparse::CompensatedField,
        T::Real: num_traits::Float,
        A: SparseMatVec<T>,
    {
        let mut ax = vec![faer_traits::math_utils::zero::<T>(); a.nrows()];
        a.apply_compensated(&mut ax, x);
        let mut residual = vec![faer_traits::math_utils::zero::<T>(); b.len()];
        for ((dst, &lhs), &rhs) in residual.iter_mut().zip(ax.iter()).zip(b.iter()) {
            *dst = rhs - lhs;
        }
        norm2::<T>(&residual)
    }

    fn full_spd_matrix() -> SparseColMat<usize, f64> {
        SparseColMat::<usize, f64>::try_new_from_triplets(
            4,
            4,
            &[
                Triplet::new(0, 0, 10.0),
                Triplet::new(0, 1, 2.0),
                Triplet::new(1, 0, 2.0),
                Triplet::new(1, 1, 9.0),
                Triplet::new(1, 2, -1.0),
                Triplet::new(2, 1, -1.0),
                Triplet::new(2, 2, 7.0),
                Triplet::new(2, 3, 1.5),
                Triplet::new(3, 2, 1.5),
                Triplet::new(3, 3, 8.0),
            ],
        )
        .unwrap()
    }

    fn upper_spd_matrix() -> SparseColMat<usize, f64> {
        SparseColMat::<usize, f64>::try_new_from_triplets(
            4,
            4,
            &[
                Triplet::new(0, 0, 10.0),
                Triplet::new(0, 1, 2.0),
                Triplet::new(1, 1, 9.0),
                Triplet::new(1, 2, -1.0),
                Triplet::new(2, 2, 7.0),
                Triplet::new(2, 3, 1.5),
                Triplet::new(3, 3, 8.0),
            ],
        )
        .unwrap()
    }

    fn full_hermitian_matrix() -> SparseColMat<usize, c64> {
        SparseColMat::<usize, c64>::try_new_from_triplets(
            3,
            3,
            &[
                Triplet::new(0, 0, c64::new(6.0, 0.0)),
                Triplet::new(0, 1, c64::new(1.0, -2.0)),
                Triplet::new(1, 0, c64::new(1.0, 2.0)),
                Triplet::new(1, 1, c64::new(7.0, 0.0)),
                Triplet::new(1, 2, c64::new(-0.5, 1.0)),
                Triplet::new(2, 1, c64::new(-0.5, -1.0)),
                Triplet::new(2, 2, c64::new(5.0, 0.0)),
            ],
        )
        .unwrap()
    }

    fn upper_hermitian_matrix() -> SparseColMat<usize, c64> {
        SparseColMat::<usize, c64>::try_new_from_triplets(
            3,
            3,
            &[
                Triplet::new(0, 0, c64::new(6.0, 0.0)),
                Triplet::new(0, 1, c64::new(1.0, -2.0)),
                Triplet::new(1, 1, c64::new(7.0, 0.0)),
                Triplet::new(1, 2, c64::new(-0.5, 1.0)),
                Triplet::new(2, 2, c64::new(5.0, 0.0)),
            ],
        )
        .unwrap()
    }

    fn full_indefinite_matrix() -> SparseColMat<usize, f64> {
        SparseColMat::<usize, f64>::try_new_from_triplets(
            3,
            3,
            &[
                Triplet::new(0, 0, 4.0),
                Triplet::new(0, 1, 1.0),
                Triplet::new(1, 0, 1.0),
                Triplet::new(1, 1, -3.0),
                Triplet::new(1, 2, 0.5),
                Triplet::new(2, 1, 0.5),
                Triplet::new(2, 2, 2.0),
            ],
        )
        .unwrap()
    }

    fn upper_indefinite_matrix() -> SparseColMat<usize, f64> {
        SparseColMat::<usize, f64>::try_new_from_triplets(
            3,
            3,
            &[
                Triplet::new(0, 0, 4.0),
                Triplet::new(0, 1, 1.0),
                Triplet::new(1, 1, -3.0),
                Triplet::new(1, 2, 0.5),
                Triplet::new(2, 2, 2.0),
            ],
        )
        .unwrap()
    }

    #[test]
    fn llt_factorizes_and_solves_real_spd_system() {
        let a_full = full_spd_matrix();
        let a_tri = upper_spd_matrix();
        let x_true = [1.0, -2.0, 0.5, 3.0];
        let mut b = zero_col::<f64>(a_full.nrows());
        a_full
            .as_ref()
            .apply_compensated(col_slice_mut(&mut b), &x_true);

        let llt = SparseLlt::<usize, f64>::factorize(
            a_tri.as_ref(),
            Side::Upper,
            SymmetricOrdering::Identity,
            CholeskySymbolicParams::default(),
            LltRegularization::default(),
            Par::Seq,
            Spec::default(),
        )
        .unwrap();

        let x = llt.solve_rhs(col_slice(&b), Par::Seq).unwrap();
        assert!(residual_norm(a_full.as_ref(), col_slice(&x), col_slice(&b)) < 1.0e-12);
    }

    #[test]
    fn ldlt_factorizes_and_solves_real_indefinite_system() {
        let a_full = full_indefinite_matrix();
        let a_tri = upper_indefinite_matrix();
        let x_true = [1.0, -1.5, 0.75];
        let mut b = zero_col::<f64>(a_full.nrows());
        a_full
            .as_ref()
            .apply_compensated(col_slice_mut(&mut b), &x_true);

        let ldlt = SparseLdlt::<usize, f64>::factorize(
            a_tri.as_ref(),
            Side::Upper,
            SymmetricOrdering::Identity,
            CholeskySymbolicParams::default(),
            LdltRegularization::default(),
            Par::Seq,
            Spec::default(),
        )
        .unwrap();

        let x = ldlt.solve_rhs(col_slice(&b), Par::Seq).unwrap();
        assert!(residual_norm(a_full.as_ref(), col_slice(&x), col_slice(&b)) < 1.0e-12);
    }

    #[test]
    fn llt_solves_complex_hermitian_system() {
        let a_full = full_hermitian_matrix();
        let a_tri = upper_hermitian_matrix();
        let x_true = [
            c64::new(1.0, -0.25),
            c64::new(-0.5, 0.75),
            c64::new(0.125, -1.0),
        ];
        let mut b = zero_col::<c64>(a_full.nrows());
        a_full
            .as_ref()
            .apply_compensated(col_slice_mut(&mut b), &x_true);

        let llt = SparseLlt::<usize, c64>::factorize(
            a_tri.as_ref(),
            Side::Upper,
            SymmetricOrdering::Identity,
            CholeskySymbolicParams::default(),
            LltRegularization::default(),
            Par::Seq,
            Spec::default(),
        )
        .unwrap();

        let x = llt.solve_rhs(col_slice(&b), Par::Seq).unwrap();
        assert!(residual_norm(a_full.as_ref(), col_slice(&x), col_slice(&b)) < 1.0e-11);
    }

    #[test]
    fn llt_refactors_same_pattern_with_new_values() {
        let a0 = upper_spd_matrix();
        let a1 = SparseColMat::<usize, f64>::try_new_from_triplets(
            4,
            4,
            &[
                Triplet::new(0, 0, 11.0),
                Triplet::new(0, 1, 1.5),
                Triplet::new(1, 1, 8.5),
                Triplet::new(1, 2, -0.75),
                Triplet::new(2, 2, 6.5),
                Triplet::new(2, 3, 1.25),
                Triplet::new(3, 3, 9.0),
            ],
        )
        .unwrap();

        let mut llt = SparseLlt::<usize, f64>::analyze(
            a0.as_ref(),
            Side::Upper,
            SymmetricOrdering::Identity,
            CholeskySymbolicParams::default(),
        )
        .unwrap();
        llt.refactor(
            a0.as_ref(),
            LltRegularization::default(),
            Par::Seq,
            Spec::default(),
        )
        .unwrap();
        llt.refactor(
            a1.as_ref(),
            LltRegularization::default(),
            Par::Seq,
            Spec::default(),
        )
        .unwrap();
        assert!(llt.is_ready());
    }

    #[test]
    fn ldlt_rejects_pattern_mismatch_on_refactor() {
        let a0 = upper_indefinite_matrix();
        let a1 = SparseColMat::<usize, f64>::try_new_from_triplets(
            3,
            3,
            &[
                Triplet::new(0, 0, 4.0),
                Triplet::new(1, 0, 1.0),
                Triplet::new(1, 1, -3.0),
                Triplet::new(1, 2, 0.5),
                Triplet::new(2, 2, 2.0),
            ],
        )
        .unwrap();

        let mut ldlt = SparseLdlt::<usize, f64>::analyze(
            a0.as_ref(),
            Side::Upper,
            SymmetricOrdering::Identity,
            CholeskySymbolicParams::default(),
        )
        .unwrap();

        assert!(matches!(
            ldlt.refactor(
                a1.as_ref(),
                LdltRegularization::default(),
                Par::Seq,
                Spec::default()
            ),
            Err(SparseLdltError::PatternMismatch)
        ));
    }

    #[test]
    fn llt_can_be_used_as_bicgstab_preconditioner() {
        let a_full = full_spd_matrix();
        let a_tri = upper_spd_matrix();
        let x_true = [1.0, -2.0, 0.5, 3.0];
        let mut b = zero_col::<f64>(a_full.nrows());
        a_full
            .as_ref()
            .apply_compensated(col_slice_mut(&mut b), &x_true);

        let llt = SparseLlt::<usize, f64>::factorize(
            a_tri.as_ref(),
            Side::Upper,
            SymmetricOrdering::Identity,
            CholeskySymbolicParams::default(),
            LltRegularization::default(),
            Par::Seq,
            Spec::default(),
        )
        .unwrap();

        let solve = BiCGSTAB::solve_with_precond(
            a_full.as_ref(),
            llt,
            &[0.0; 4],
            col_slice(&b),
            1.0e-10,
            50,
        )
        .unwrap();

        assert!(residual_norm(a_full.as_ref(), col_slice(solve.x()), col_slice(&b)) < 1.0e-10);
    }

    #[test]
    fn llt_rejects_non_square_analysis() {
        let a = SparseColMat::<usize, f64>::try_new_from_triplets(
            2,
            3,
            &[Triplet::new(0, 0, 1.0), Triplet::new(1, 1, 2.0)],
        )
        .unwrap();

        assert!(matches!(
            SparseLlt::<usize, f64>::analyze(
                a.as_ref(),
                Side::Upper,
                SymmetricOrdering::Identity,
                CholeskySymbolicParams::default()
            ),
            Err(SparseLltError::NonSquare { .. })
        ));
    }
}
