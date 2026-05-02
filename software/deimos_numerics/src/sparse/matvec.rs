use super::compensated::{CompensatedField, CompensatedSum};
use core::fmt::Debug;
use faer::sparse::{SparseColMatRef, SparseRowMatRef};
use faer::{Index, Unbind};
use faer_traits::ComplexField;
use faer_traits::Conjugate;
use faer_traits::math_utils::zero;
use num_traits::Float;

/// Sparse matrix-vector product interface used by iterative solvers.
pub trait SparseMatVec<T: ComplexField + Copy>: Copy + Debug {
    /// Number of rows.
    fn nrows(&self) -> usize;

    /// Number of columns.
    fn ncols(&self) -> usize;

    /// Computes `out = self * rhs`.
    fn apply(&self, out: &mut [T], rhs: &[T]);

    /// Computes `out = self * rhs` with compensated accumulation.
    fn apply_compensated(&self, out: &mut [T], rhs: &[T]);
}

impl<T, I, ViewT> SparseMatVec<T> for SparseRowMatRef<'_, I, ViewT>
where
    T: CompensatedField,
    T::Real: Float,
    I: Index,
    ViewT: Conjugate<Canonical = T>,
{
    #[inline]
    fn nrows(&self) -> usize {
        self.symbolic().nrows().unbound()
    }

    #[inline]
    fn ncols(&self) -> usize {
        self.symbolic().ncols().unbound()
    }

    #[inline]
    fn apply(&self, out: &mut [T], rhs: &[T]) {
        let matrix = self.canonical();
        let nrows = matrix.nrows().unbound();
        let ncols = matrix.ncols().unbound();

        assert_eq!(out.len(), nrows);
        assert_eq!(rhs.len(), ncols);

        let row_ptr = matrix.row_ptr();
        let col_idx = matrix.col_idx();
        let values = matrix.val();

        out.fill(zero::<T>());
        for row in 0..nrows {
            let start = row_ptr[row].zx();
            let end = row_ptr[row + 1].zx();
            let mut sum = zero::<T>();

            for idx in start..end {
                sum += values[idx] * rhs[col_idx[idx].zx()];
            }

            out[row] = sum;
        }
    }

    #[inline]
    fn apply_compensated(&self, out: &mut [T], rhs: &[T]) {
        let matrix = self.canonical();
        let nrows = matrix.nrows().unbound();
        let ncols = matrix.ncols().unbound();

        assert_eq!(out.len(), nrows);
        assert_eq!(rhs.len(), ncols);

        let row_ptr = matrix.row_ptr();
        let col_idx = matrix.col_idx();
        let values = matrix.val();

        out.fill(zero::<T>());
        for row in 0..nrows {
            let start = row_ptr[row].zx();
            let end = row_ptr[row + 1].zx();
            let mut acc = CompensatedSum::<T>::default();

            for idx in start..end {
                acc.add(values[idx] * rhs[col_idx[idx].zx()]);
            }

            out[row] = acc.finish();
        }
    }
}

impl<T, I, ViewT> SparseMatVec<T> for SparseColMatRef<'_, I, ViewT>
where
    T: CompensatedField,
    T::Real: Float,
    I: Index,
    ViewT: Conjugate<Canonical = T>,
{
    #[inline]
    fn nrows(&self) -> usize {
        self.symbolic().nrows().unbound()
    }

    #[inline]
    fn ncols(&self) -> usize {
        self.symbolic().ncols().unbound()
    }

    #[inline]
    fn apply(&self, out: &mut [T], rhs: &[T]) {
        let matrix = self.canonical();
        let nrows = matrix.nrows().unbound();
        let ncols = matrix.ncols().unbound();

        assert_eq!(out.len(), nrows);
        assert_eq!(rhs.len(), ncols);

        let col_ptr = matrix.col_ptr();
        let row_idx = matrix.row_idx();
        let values = matrix.val();

        out.fill(zero::<T>());
        for col in 0..ncols {
            let rhs_value = rhs[col];
            let start = col_ptr[col].zx();
            let end = col_ptr[col + 1].zx();

            for idx in start..end {
                out[row_idx[idx].zx()] += values[idx] * rhs_value;
            }
        }
    }

    #[inline]
    fn apply_compensated(&self, out: &mut [T], rhs: &[T]) {
        let matrix = self.canonical();
        let nrows = matrix.nrows().unbound();
        let ncols = matrix.ncols().unbound();

        assert_eq!(out.len(), nrows);
        assert_eq!(rhs.len(), ncols);

        let col_ptr = matrix.col_ptr();
        let row_idx = matrix.row_idx();
        let values = matrix.val();
        let mut acc = vec![CompensatedSum::<T>::default(); nrows];

        for col in 0..ncols {
            let rhs_value = rhs[col];
            let start = col_ptr[col].zx();
            let end = col_ptr[col + 1].zx();

            for idx in start..end {
                acc[row_idx[idx].zx()].add(values[idx] * rhs_value);
            }
        }

        for (dst, acc) in out.iter_mut().zip(acc.into_iter()) {
            *dst = acc.finish();
        }
    }
}
