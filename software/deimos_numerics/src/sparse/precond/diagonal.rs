//! Diagonal sparse preconditioner.
//!
//! This is the standard Jacobi-style preconditioner for sparse linear solves:
//! keep only the diagonal of the matrix and apply its inverse during the
//! iteration.
//!
//! # Glossary
//!
//! - **Jacobi preconditioner:** Preconditioner built from the inverse diagonal
//!   of the matrix.
//! - **Inverse diagonal:** Vector of reciprocals of the diagonal entries.
//! - **Left preconditioning:** Forming `M^{-1} A x = M^{-1} b`.
//! - **Right preconditioning:** Forming `A M^{-1} y = b` and recovering
//!   `x = M^{-1} y`.

use crate::sparse::col::col_slice;
use faer::Par;
use faer::dyn_stack::{MemStack, StackReq};
use faer::matrix_free::LinOp;
use faer::sparse::{SparseColMatRef, SparseRowMatRef};
use faer::{Col, Index, MatMut, MatRef, Unbind};
use faer_traits::ComplexField;
use faer_traits::Conjugate;
use faer_traits::ext::ComplexFieldExt;
use faer_traits::math_utils::zero;

use super::Precond;

/// Diagonal sparse preconditioner.
///
/// This stores `M^{-1}` for the diagonal matrix `M = diag(A)`, with entries
/// chosen as
///
/// `inv_diag[i] = 1 / A[i, i]`.
///
/// That is the cheapest useful approximation to `A`: it ignores all coupling
/// between unknowns, but it preserves the per-variable scale carried by the
/// diagonal. For real matrices this is the usual Jacobi preconditioner; for
/// complex matrices the same idea applies using the exact complex reciprocal of
/// each diagonal entry.
///
/// When a solver applies this preconditioner from the left, it forms
/// `M^{-1} A x = M^{-1} b`, which normalizes equation scales row-by-row. When a
/// solver applies it from the right, it forms `A M^{-1} y = b` with
/// `x = M^{-1} y`, which normalizes variable or column scales instead. The
/// current `BiCGSTAB` implementation uses this preconditioner from the right.
#[derive(Clone, Debug, PartialEq)]
pub struct DiagonalPrecond<T: ComplexField + Copy> {
    inv_diag: Col<T>,
}

/// Errors that can occur while building a [`DiagonalPrecond`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiagonalPrecondError {
    /// Diagonal preconditioning only makes sense for square systems.
    NonSquare {
        /// Actual row count.
        nrows: usize,
        /// Actual column count.
        ncols: usize,
    },
    /// The matrix has no stored entry on the diagonal at `index`.
    MissingDiagonal {
        /// Index of the missing diagonal entry.
        index: usize,
    },
    /// The diagonal entry at `index` is zero, so it cannot be inverted.
    ZeroDiagonal {
        /// Index of the zero diagonal entry.
        index: usize,
    },
}

impl<T: ComplexField + Copy> DiagonalPrecond<T> {
    /// Builds a preconditioner from an already-inverted diagonal.
    ///
    /// Use this when the caller has already decided how the diagonal should be
    /// regularized or inverted. The values are stored as-is and later applied
    /// by pointwise multiplication.
    #[inline]
    #[must_use]
    pub fn from_inverse_diagonal(inv_diag: &[T]) -> Self {
        Self {
            inv_diag: Col::from_fn(inv_diag.len(), |i| inv_diag[i.unbound()]),
        }
    }

    /// Builds a preconditioner from diagonal entries by inverting them.
    ///
    /// Technically, this forms `M^{-1}` with `M = diag(diag)`. Intuitively, a
    /// large diagonal entry means that component is already strongly scaled, so
    /// its inverse is small; a small diagonal entry means the iteration should
    /// boost that component more aggressively.
    ///
    /// A zero diagonal entry is rejected because there is no meaningful inverse
    /// to apply without introducing an additional regularization policy.
    pub fn try_from_diagonal(diag: &[T]) -> Result<Self, DiagonalPrecondError> {
        let zero_value = zero::<T>();
        let mut inv_diag = Col::from_fn(diag.len(), |_| zero_value);
        for (i, (&value, dst)) in diag.iter().zip(inv_diag.iter_mut()).enumerate() {
            if value == zero_value {
                return Err(DiagonalPrecondError::ZeroDiagonal { index: i });
            }
            *dst = value.recip();
        }

        Ok(Self { inv_diag })
    }

    /// Dimension of the preconditioner.
    #[inline]
    #[must_use]
    pub fn dim(&self) -> usize {
        self.inv_diag.nrows()
    }

    /// Returns the stored inverse diagonal values.
    #[inline]
    #[must_use]
    pub fn inverse_diagonal(&self) -> &Col<T> {
        &self.inv_diag
    }
}

impl<'a, I, ViewT, T> TryFrom<SparseRowMatRef<'a, I, ViewT>> for DiagonalPrecond<T>
where
    T: ComplexField + Copy,
    I: Index,
    ViewT: Conjugate<Canonical = T>,
{
    type Error = DiagonalPrecondError;

    /// Extracts the diagonal from a sparse row matrix and inverts it.
    ///
    /// Only the diagonal entries are used; all off-diagonal structure is
    /// ignored. That is exactly what makes diagonal preconditioning cheap to
    /// construct compared with factorization-based preconditioners.
    fn try_from(matrix: SparseRowMatRef<'a, I, ViewT>) -> Result<Self, Self::Error> {
        let matrix = matrix.canonical();
        let nrows = matrix.nrows().unbound();
        let ncols = matrix.ncols().unbound();
        if nrows != ncols {
            return Err(DiagonalPrecondError::NonSquare { nrows, ncols });
        }

        let zero_value = zero::<T>();
        let row_ptr = matrix.row_ptr();
        let col_idx = matrix.col_idx();
        let values = matrix.val();
        let mut diag = vec![zero_value; nrows];
        let mut found = vec![false; nrows];

        for row in 0..nrows {
            for idx in row_ptr[row].zx()..row_ptr[row + 1].zx() {
                if col_idx[idx].zx() == row {
                    diag[row] = values[idx];
                    found[row] = true;
                    break;
                }
            }
        }

        for row in 0..nrows {
            if !found[row] {
                return Err(DiagonalPrecondError::MissingDiagonal { index: row });
            }
            if diag[row] == zero_value {
                return Err(DiagonalPrecondError::ZeroDiagonal { index: row });
            }
        }

        Self::try_from_diagonal(&diag)
    }
}

impl<'a, I, ViewT, T> TryFrom<SparseColMatRef<'a, I, ViewT>> for DiagonalPrecond<T>
where
    T: ComplexField + Copy,
    I: Index,
    ViewT: Conjugate<Canonical = T>,
{
    type Error = DiagonalPrecondError;

    /// Extracts the diagonal from a sparse column matrix and inverts it.
    fn try_from(matrix: SparseColMatRef<'a, I, ViewT>) -> Result<Self, Self::Error> {
        let matrix = matrix.canonical();
        let nrows = matrix.nrows().unbound();
        let ncols = matrix.ncols().unbound();
        if nrows != ncols {
            return Err(DiagonalPrecondError::NonSquare { nrows, ncols });
        }

        let zero_value = zero::<T>();
        let col_ptr = matrix.col_ptr();
        let row_idx = matrix.row_idx();
        let values = matrix.val();
        let mut diag = vec![zero_value; nrows];
        let mut found = vec![false; nrows];

        for col in 0..ncols {
            for idx in col_ptr[col].zx()..col_ptr[col + 1].zx() {
                if row_idx[idx].zx() == col {
                    diag[col] = values[idx];
                    found[col] = true;
                    break;
                }
            }
        }

        for col in 0..ncols {
            if !found[col] {
                return Err(DiagonalPrecondError::MissingDiagonal { index: col });
            }
            if diag[col] == zero_value {
                return Err(DiagonalPrecondError::ZeroDiagonal { index: col });
            }
        }

        Self::try_from_diagonal(&diag)
    }
}

impl<T: ComplexField + Copy> LinOp<T> for DiagonalPrecond<T> {
    #[inline]
    fn apply_scratch(&self, _rhs_ncols: usize, _par: Par) -> StackReq {
        StackReq::EMPTY
    }

    #[inline]
    fn nrows(&self) -> usize {
        self.dim()
    }

    #[inline]
    fn ncols(&self) -> usize {
        self.dim()
    }

    fn apply(&self, mut out: MatMut<'_, T>, rhs: MatRef<'_, T>, _par: Par, _stack: &mut MemStack) {
        // Applying the preconditioner is just a row-wise scale by the stored
        // inverse diagonal. Each row corresponds to one component of the
        // unknown or residual being rescaled.
        let inv_diag = col_slice(&self.inv_diag);
        let nrows = rhs.nrows().unbound();
        let ncols = rhs.ncols().unbound();
        assert_eq!(out.nrows().unbound(), nrows);
        assert_eq!(out.ncols().unbound(), ncols);
        assert_eq!(inv_diag.len(), nrows);

        for col in 0..ncols {
            for row in 0..nrows {
                out[(row, col)] = inv_diag[row] * rhs[(row, col)];
            }
        }
    }

    fn conj_apply(
        &self,
        mut out: MatMut<'_, T>,
        rhs: MatRef<'_, T>,
        _par: Par,
        _stack: &mut MemStack,
    ) {
        // For complex scalars, the conjugate application is the same diagonal
        // scaling with conjugated coefficients.
        let inv_diag = col_slice(&self.inv_diag);
        let nrows = rhs.nrows().unbound();
        let ncols = rhs.ncols().unbound();
        assert_eq!(out.nrows().unbound(), nrows);
        assert_eq!(out.ncols().unbound(), ncols);
        assert_eq!(inv_diag.len(), nrows);

        for col in 0..ncols {
            for row in 0..nrows {
                out[(row, col)] = inv_diag[row].conj() * rhs[(row, col)];
            }
        }
    }
}

impl<T: ComplexField + Copy> Precond<T> for DiagonalPrecond<T> {
    #[inline]
    fn apply_in_place_scratch(&self, _rhs_ncols: usize, _par: Par) -> StackReq {
        StackReq::EMPTY
    }

    fn apply_in_place(&self, mut rhs: MatMut<'_, T>, _par: Par, _stack: &mut MemStack) {
        // In-place application is the hot path used by iterative solvers.
        // Using the diagonal keeps it branch-free and allocation-free.
        let inv_diag = col_slice(&self.inv_diag);
        let nrows = rhs.nrows().unbound();
        let ncols = rhs.ncols().unbound();
        assert_eq!(inv_diag.len(), nrows);

        for col in 0..ncols {
            for row in 0..nrows {
                rhs[(row, col)] = inv_diag[row] * rhs[(row, col)];
            }
        }
    }

    fn conj_apply_in_place(&self, mut rhs: MatMut<'_, T>, _par: Par, _stack: &mut MemStack) {
        let inv_diag = col_slice(&self.inv_diag);
        let nrows = rhs.nrows().unbound();
        let ncols = rhs.ncols().unbound();
        assert_eq!(inv_diag.len(), nrows);

        for col in 0..ncols {
            for row in 0..nrows {
                rhs[(row, col)] = inv_diag[row].conj() * rhs[(row, col)];
            }
        }
    }
}

#[cfg(test)]
mod test {
    use super::{DiagonalPrecond, DiagonalPrecondError};
    use crate::sparse::col::{col_slice, zero_col};
    use crate::sparse::precond::{apply_precond_to_col, precond_buffer};
    use faer::c64;
    use faer::sparse::{SparseColMat, SparseRowMat, Triplet};

    #[test]
    fn builds_from_sparse_row_matrix_and_applies_inverse_diagonal() {
        let a = SparseRowMat::<usize, f64>::try_new_from_triplets(
            3,
            3,
            &[
                Triplet::new(0, 0, 4.0),
                Triplet::new(0, 1, -1.0),
                Triplet::new(1, 0, 2.0),
                Triplet::new(1, 1, 5.0),
                Triplet::new(2, 2, 8.0),
            ],
        )
        .unwrap();

        let precond = DiagonalPrecond::try_from(a.as_ref()).unwrap();
        let mut out = zero_col::<f64>(3);
        let rhs = crate::sparse::col::col_from_slice(&[8.0, 15.0, 16.0]);
        let mut buffer = precond_buffer(&precond);
        apply_precond_to_col(&precond, &mut out, &rhs, &mut buffer);

        assert_eq!(col_slice(precond.inverse_diagonal()), &[0.25, 0.2, 0.125]);
        assert_eq!(col_slice(&out), &[2.0, 3.0, 2.0]);
    }

    #[test]
    fn errors_on_missing_or_zero_diagonal() {
        let missing = SparseColMat::<usize, f64>::try_new_from_triplets(
            2,
            2,
            &[Triplet::new(0, 1, 1.0), Triplet::new(1, 0, 1.0)],
        )
        .unwrap();
        assert_eq!(
            DiagonalPrecond::try_from(missing.as_ref()),
            Err(DiagonalPrecondError::MissingDiagonal { index: 0 })
        );

        let zero = SparseRowMat::<usize, f64>::try_new_from_triplets(
            2,
            2,
            &[Triplet::new(0, 0, 1.0), Triplet::new(1, 1, 0.0)],
        )
        .unwrap();
        assert_eq!(
            DiagonalPrecond::try_from(zero.as_ref()),
            Err(DiagonalPrecondError::ZeroDiagonal { index: 1 })
        );
    }

    #[test]
    fn applies_complex_inverse_diagonal() {
        let precond =
            DiagonalPrecond::from_inverse_diagonal(&[c64::new(2.0, 1.0), c64::new(-1.0, 0.5)]);
        let rhs = crate::sparse::col::col_from_slice(&[c64::new(1.0, -1.0), c64::new(2.0, 3.0)]);
        let mut out = zero_col::<c64>(2);
        let mut buffer = precond_buffer::<c64, _>(&precond);
        apply_precond_to_col(&precond, &mut out, &rhs, &mut buffer);

        assert_eq!(col_slice(&out)[0], c64::new(2.0, 1.0) * c64::new(1.0, -1.0));
        assert_eq!(col_slice(&out)[1], c64::new(-1.0, 0.5) * c64::new(2.0, 3.0));
    }
}
