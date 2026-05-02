//! Compensated operator adapters for `faer`'s matrix-free decomposition
//! routines.
//!
//! The wrappers in this module do not change `faer`'s Arnoldi / Lanczos
//! orthogonalization. They only change the accuracy of operator application for
//! matrix types that this crate knows how to evaluate with compensated sparse
//! accumulation.
//!
//! This is intentionally a narrow boundary. The goal is not to replace `faer`'s
//! decomposition algorithms, but to reduce the local summation error in sparse
//! matrix-vector products before those values enter the Krylov iteration.
//!
//! # Glossary
//!
//! - **LinOp / BiLinOp:** `faer` traits for matrix-free linear and bilinear
//!   operators.
//! - **Compensated accumulation:** Summation that keeps correction terms to
//!   reduce floating-point loss.
//! - **Krylov iteration:** Iterative eigensolver or SVD process built from
//!   repeated operator applications.

use crate::sparse::compensated::{CompensatedField, CompensatedSum};
use faer::dyn_stack::{DynArray, MemStack, StackReq};
use faer::matrix_free::{BiLinOp, LinOp};
use faer::prelude::ReborrowMut;
use faer::sparse::{SparseColMatRef, SparseRowMatRef};
use faer::{Index, MatMut, MatRef, Par, Unbind};
use faer_traits::Conjugate;
use faer_traits::ext::ComplexFieldExt;
use num_traits::Float;

/// Operator applications that can be evaluated with compensated accumulation.
///
/// The `LinOp` traits from `faer` define the algebraic interface expected by
/// its matrix-free eigendecomposition and SVD routines. This companion trait
/// lets an implementation override just the accumulation strategy for the
/// operator application while keeping the same external operator shape.
pub trait CompensatedApply<T: CompensatedField>: LinOp<T>
where
    T::Real: Float,
{
    /// Computes the extra scratch required by compensated application.
    fn compensated_apply_scratch(&self, rhs_ncols: usize, par: Par) -> StackReq;

    /// Computes `out = self * rhs` with compensated accumulation.
    ///
    /// Intuitively, this replaces one naive running sum per output entry with a
    /// compensated reduction so cancellation in long sparse rows or columns
    /// loses less information.
    fn apply_compensated(
        &self,
        out: MatMut<'_, T>,
        rhs: MatRef<'_, T>,
        par: Par,
        stack: &mut MemStack,
    );

    /// Computes `out = conj(self) * rhs` with compensated accumulation.
    fn conj_apply_compensated(
        &self,
        out: MatMut<'_, T>,
        rhs: MatRef<'_, T>,
        par: Par,
        stack: &mut MemStack,
    );
}

/// Bi-directional operator applications that can be evaluated with compensated
/// accumulation.
///
/// This extends [`CompensatedApply`] to operators that also need transpose or
/// adjoint application, as required by matrix-free SVD.
pub trait CompensatedBiApply<T: CompensatedField>: CompensatedApply<T> + BiLinOp<T>
where
    T::Real: Float,
{
    /// Computes the extra scratch required by compensated transpose / adjoint application.
    fn compensated_transpose_apply_scratch(&self, rhs_ncols: usize, par: Par) -> StackReq;

    /// Computes `out = self^T * rhs` with compensated accumulation.
    fn transpose_apply_compensated(
        &self,
        out: MatMut<'_, T>,
        rhs: MatRef<'_, T>,
        par: Par,
        stack: &mut MemStack,
    );

    /// Computes `out = self^H * rhs` with compensated accumulation.
    fn adjoint_apply_compensated(
        &self,
        out: MatMut<'_, T>,
        rhs: MatRef<'_, T>,
        par: Par,
        stack: &mut MemStack,
    );
}

/// Wraps a linear operator and routes `LinOp` application through compensated
/// kernels supplied by [`CompensatedApply`].
///
/// The wrapper does not add new algebraic behavior. It only changes how the
/// wrapped operator is evaluated when `faer` calls `apply` or `conj_apply`.
#[derive(Clone, Copy, Debug)]
pub struct CompensatedLinOp<A> {
    inner: A,
}

impl<A> CompensatedLinOp<A> {
    /// Creates a compensated wrapper around `inner`.
    #[must_use]
    pub fn new(inner: A) -> Self {
        Self { inner }
    }

    /// Returns the wrapped operator.
    #[must_use]
    pub fn inner(&self) -> &A {
        &self.inner
    }
}

impl<T, A> LinOp<T> for CompensatedLinOp<A>
where
    T: CompensatedField,
    T::Real: Float,
    A: CompensatedApply<T> + Sync + core::fmt::Debug,
{
    fn apply_scratch(&self, rhs_ncols: usize, par: Par) -> StackReq {
        self.inner.compensated_apply_scratch(rhs_ncols, par)
    }

    fn nrows(&self) -> usize {
        self.inner.nrows()
    }

    fn ncols(&self) -> usize {
        self.inner.ncols()
    }

    fn apply(&self, out: MatMut<'_, T>, rhs: MatRef<'_, T>, par: Par, stack: &mut MemStack) {
        self.inner.apply_compensated(out, rhs, par, stack);
    }

    fn conj_apply(&self, out: MatMut<'_, T>, rhs: MatRef<'_, T>, par: Par, stack: &mut MemStack) {
        self.inner.conj_apply_compensated(out, rhs, par, stack);
    }
}

/// Wraps a bi-directional linear operator and routes `BiLinOp` application
/// through compensated kernels supplied by [`CompensatedBiApply`].
///
/// This is the natural wrapper to use before calling the matrix-free partial
/// SVD routines, since they need both the forward and adjoint operator action.
#[derive(Clone, Copy, Debug)]
pub struct CompensatedBiLinOp<A> {
    inner: A,
}

impl<A> CompensatedBiLinOp<A> {
    /// Creates a compensated wrapper around `inner`.
    #[must_use]
    pub fn new(inner: A) -> Self {
        Self { inner }
    }

    /// Returns the wrapped operator.
    #[must_use]
    pub fn inner(&self) -> &A {
        &self.inner
    }
}

impl<T, A> LinOp<T> for CompensatedBiLinOp<A>
where
    T: CompensatedField,
    T::Real: Float,
    A: CompensatedBiApply<T> + Sync + core::fmt::Debug,
{
    fn apply_scratch(&self, rhs_ncols: usize, par: Par) -> StackReq {
        self.inner.compensated_apply_scratch(rhs_ncols, par)
    }

    fn nrows(&self) -> usize {
        self.inner.nrows()
    }

    fn ncols(&self) -> usize {
        self.inner.ncols()
    }

    fn apply(&self, out: MatMut<'_, T>, rhs: MatRef<'_, T>, par: Par, stack: &mut MemStack) {
        self.inner.apply_compensated(out, rhs, par, stack);
    }

    fn conj_apply(&self, out: MatMut<'_, T>, rhs: MatRef<'_, T>, par: Par, stack: &mut MemStack) {
        self.inner.conj_apply_compensated(out, rhs, par, stack);
    }
}

impl<T, A> BiLinOp<T> for CompensatedBiLinOp<A>
where
    T: CompensatedField,
    T::Real: Float,
    A: CompensatedBiApply<T> + Sync + core::fmt::Debug,
{
    fn transpose_apply_scratch(&self, rhs_ncols: usize, par: Par) -> StackReq {
        self.inner
            .compensated_transpose_apply_scratch(rhs_ncols, par)
    }

    fn transpose_apply(
        &self,
        out: MatMut<'_, T>,
        rhs: MatRef<'_, T>,
        par: Par,
        stack: &mut MemStack,
    ) {
        self.inner.transpose_apply_compensated(out, rhs, par, stack);
    }

    fn adjoint_apply(
        &self,
        out: MatMut<'_, T>,
        rhs: MatRef<'_, T>,
        par: Par,
        stack: &mut MemStack,
    ) {
        self.inner.adjoint_apply_compensated(out, rhs, par, stack);
    }
}

#[inline]
fn max_scratch_req(lhs: StackReq, rhs: StackReq) -> StackReq {
    StackReq::any_of(&[lhs, rhs])
}

#[inline]
fn col_ref_slice<T>(col: faer::ColRef<'_, T>) -> &[T] {
    col.try_as_col_major().unwrap().as_slice()
}

#[inline]
fn col_mut_slice<T>(col: faer::ColMut<'_, T>) -> &mut [T] {
    col.try_as_col_major_mut().unwrap().as_slice_mut()
}

#[inline]
fn scratch_acc_len<T: CompensatedField>(req_rows: usize) -> StackReq
where
    T::Real: Float,
{
    StackReq::new::<CompensatedSum<T>>(req_rows)
}

#[inline]
fn init_accum_slice<T: CompensatedField>(
    len: usize,
    stack: &mut MemStack,
) -> (DynArray<'_, CompensatedSum<T>>, &mut MemStack)
where
    T::Real: Float,
{
    stack.collect(core::iter::repeat_n(CompensatedSum::<T>::default(), len))
}

impl<T, I, ViewT> CompensatedApply<T> for SparseRowMatRef<'_, I, ViewT>
where
    T: CompensatedField,
    T::Real: Float,
    I: Index,
    ViewT: Conjugate<Canonical = T>,
{
    fn compensated_apply_scratch(&self, rhs_ncols: usize, par: Par) -> StackReq {
        let _ = rhs_ncols;
        max_scratch_req(self.apply_scratch(rhs_ncols, par), StackReq::EMPTY)
    }

    fn apply_compensated(
        &self,
        mut out: MatMut<'_, T>,
        rhs: MatRef<'_, T>,
        _par: Par,
        _stack: &mut MemStack,
    ) {
        let matrix = self.canonical();
        let nrows = matrix.nrows().unbound();
        let ncols = matrix.ncols().unbound();
        assert_eq!(rhs.nrows(), ncols);
        assert_eq!(out.nrows(), nrows);
        assert_eq!(out.ncols(), rhs.ncols());

        let row_ptr = matrix.row_ptr();
        let col_idx = matrix.col_idx();
        let values = matrix.val();

        for j in 0..rhs.ncols() {
            let rhs = col_ref_slice(rhs.col(j));
            let out = col_mut_slice(out.rb_mut().col_mut(j));
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

    fn conj_apply_compensated(
        &self,
        mut out: MatMut<'_, T>,
        rhs: MatRef<'_, T>,
        _par: Par,
        _stack: &mut MemStack,
    ) {
        let matrix = self.canonical();
        let nrows = matrix.nrows().unbound();
        let ncols = matrix.ncols().unbound();
        assert_eq!(rhs.nrows(), ncols);
        assert_eq!(out.nrows(), nrows);
        assert_eq!(out.ncols(), rhs.ncols());

        let row_ptr = matrix.row_ptr();
        let col_idx = matrix.col_idx();
        let values = matrix.val();

        for j in 0..rhs.ncols() {
            let rhs = col_ref_slice(rhs.col(j));
            let out = col_mut_slice(out.rb_mut().col_mut(j));
            for row in 0..nrows {
                let start = row_ptr[row].zx();
                let end = row_ptr[row + 1].zx();
                let mut acc = CompensatedSum::<T>::default();
                for idx in start..end {
                    acc.add(values[idx].conj() * rhs[col_idx[idx].zx()]);
                }
                out[row] = acc.finish();
            }
        }
    }
}

impl<T, I, ViewT> CompensatedBiApply<T> for SparseRowMatRef<'_, I, ViewT>
where
    T: CompensatedField,
    T::Real: Float,
    I: Index,
    ViewT: Conjugate<Canonical = T>,
{
    fn compensated_transpose_apply_scratch(&self, rhs_ncols: usize, par: Par) -> StackReq {
        max_scratch_req(
            self.transpose_apply_scratch(rhs_ncols, par),
            scratch_acc_len::<T>(self.ncols()),
        )
    }

    fn transpose_apply_compensated(
        &self,
        mut out: MatMut<'_, T>,
        rhs: MatRef<'_, T>,
        _par: Par,
        stack: &mut MemStack,
    ) {
        let matrix = self.canonical();
        let nrows = matrix.nrows().unbound();
        let ncols = matrix.ncols().unbound();
        assert_eq!(rhs.nrows(), nrows);
        assert_eq!(out.nrows(), ncols);
        assert_eq!(out.ncols(), rhs.ncols());

        let row_ptr = matrix.row_ptr();
        let col_idx = matrix.col_idx();
        let values = matrix.val();

        for j in 0..rhs.ncols() {
            let rhs = col_ref_slice(rhs.col(j));
            let out_col = out.rb_mut().col_mut(j);
            let out = col_mut_slice(out_col);
            // The transpose of a CSR matrix scatters each row into output
            // columns. Accumulate into temporary compensated sums first so the
            // scatter order does not become the final rounding order.
            let (mut acc, _) = init_accum_slice::<T>(ncols, stack);
            for value in acc.iter_mut() {
                *value = CompensatedSum::default();
            }
            for row in 0..nrows {
                let rhs_value = rhs[row];
                let start = row_ptr[row].zx();
                let end = row_ptr[row + 1].zx();
                for idx in start..end {
                    acc[col_idx[idx].zx()].add(values[idx] * rhs_value);
                }
            }
            for (dst, sum) in out.iter_mut().zip(acc.iter().copied()) {
                *dst = sum.finish();
            }
        }
    }

    fn adjoint_apply_compensated(
        &self,
        mut out: MatMut<'_, T>,
        rhs: MatRef<'_, T>,
        _par: Par,
        stack: &mut MemStack,
    ) {
        let matrix = self.canonical();
        let nrows = matrix.nrows().unbound();
        let ncols = matrix.ncols().unbound();
        assert_eq!(rhs.nrows(), nrows);
        assert_eq!(out.nrows(), ncols);
        assert_eq!(out.ncols(), rhs.ncols());

        let row_ptr = matrix.row_ptr();
        let col_idx = matrix.col_idx();
        let values = matrix.val();

        for j in 0..rhs.ncols() {
            let rhs = col_ref_slice(rhs.col(j));
            let out_col = out.rb_mut().col_mut(j);
            let out = col_mut_slice(out_col);
            // The adjoint path has the same scatter pattern as the transpose
            // path, but with conjugated matrix entries.
            let (mut acc, _) = init_accum_slice::<T>(ncols, stack);
            for value in acc.iter_mut() {
                *value = CompensatedSum::default();
            }
            for row in 0..nrows {
                let rhs_value = rhs[row];
                let start = row_ptr[row].zx();
                let end = row_ptr[row + 1].zx();
                for idx in start..end {
                    acc[col_idx[idx].zx()].add(values[idx].conj() * rhs_value);
                }
            }
            for (dst, sum) in out.iter_mut().zip(acc.iter().copied()) {
                *dst = sum.finish();
            }
        }
    }
}

impl<T, I, ViewT> CompensatedApply<T> for SparseColMatRef<'_, I, ViewT>
where
    T: CompensatedField,
    T::Real: Float,
    I: Index,
    ViewT: Conjugate<Canonical = T>,
{
    fn compensated_apply_scratch(&self, rhs_ncols: usize, par: Par) -> StackReq {
        max_scratch_req(
            self.apply_scratch(rhs_ncols, par),
            scratch_acc_len::<T>(self.nrows()),
        )
    }

    fn apply_compensated(
        &self,
        mut out: MatMut<'_, T>,
        rhs: MatRef<'_, T>,
        _par: Par,
        stack: &mut MemStack,
    ) {
        let matrix = self.canonical();
        let nrows = matrix.nrows().unbound();
        let ncols = matrix.ncols().unbound();
        assert_eq!(rhs.nrows(), ncols);
        assert_eq!(out.nrows(), nrows);
        assert_eq!(out.ncols(), rhs.ncols());

        let col_ptr = matrix.col_ptr();
        let row_idx = matrix.row_idx();
        let values = matrix.val();

        for j in 0..rhs.ncols() {
            let rhs = col_ref_slice(rhs.col(j));
            let out_col = out.rb_mut().col_mut(j);
            let out = col_mut_slice(out_col);
            // CSC application also scatters contributions into output rows.
            // Use one compensated accumulator per row and finalize once per
            // output entry.
            let (mut acc, _) = init_accum_slice::<T>(nrows, stack);
            for value in acc.iter_mut() {
                *value = CompensatedSum::default();
            }
            for col in 0..ncols {
                let rhs_value = rhs[col];
                let start = col_ptr[col].zx();
                let end = col_ptr[col + 1].zx();
                for idx in start..end {
                    acc[row_idx[idx].zx()].add(values[idx] * rhs_value);
                }
            }
            for (dst, sum) in out.iter_mut().zip(acc.iter().copied()) {
                *dst = sum.finish();
            }
        }
    }

    fn conj_apply_compensated(
        &self,
        mut out: MatMut<'_, T>,
        rhs: MatRef<'_, T>,
        _par: Par,
        stack: &mut MemStack,
    ) {
        let matrix = self.canonical();
        let nrows = matrix.nrows().unbound();
        let ncols = matrix.ncols().unbound();
        assert_eq!(rhs.nrows(), ncols);
        assert_eq!(out.nrows(), nrows);
        assert_eq!(out.ncols(), rhs.ncols());

        let col_ptr = matrix.col_ptr();
        let row_idx = matrix.row_idx();
        let values = matrix.val();

        for j in 0..rhs.ncols() {
            let rhs = col_ref_slice(rhs.col(j));
            let out_col = out.rb_mut().col_mut(j);
            let out = col_mut_slice(out_col);
            let (mut acc, _) = init_accum_slice::<T>(nrows, stack);
            for value in acc.iter_mut() {
                *value = CompensatedSum::default();
            }
            for col in 0..ncols {
                let rhs_value = rhs[col];
                let start = col_ptr[col].zx();
                let end = col_ptr[col + 1].zx();
                for idx in start..end {
                    acc[row_idx[idx].zx()].add(values[idx].conj() * rhs_value);
                }
            }
            for (dst, sum) in out.iter_mut().zip(acc.iter().copied()) {
                *dst = sum.finish();
            }
        }
    }
}

impl<T, I, ViewT> CompensatedBiApply<T> for SparseColMatRef<'_, I, ViewT>
where
    T: CompensatedField,
    T::Real: Float,
    I: Index,
    ViewT: Conjugate<Canonical = T>,
{
    fn compensated_transpose_apply_scratch(&self, rhs_ncols: usize, par: Par) -> StackReq {
        max_scratch_req(
            self.transpose_apply_scratch(rhs_ncols, par),
            StackReq::EMPTY,
        )
    }

    fn transpose_apply_compensated(
        &self,
        mut out: MatMut<'_, T>,
        rhs: MatRef<'_, T>,
        _par: Par,
        _stack: &mut MemStack,
    ) {
        let matrix = self.canonical();
        let nrows = matrix.nrows().unbound();
        let ncols = matrix.ncols().unbound();
        assert_eq!(rhs.nrows(), nrows);
        assert_eq!(out.nrows(), ncols);
        assert_eq!(out.ncols(), rhs.ncols());

        let col_ptr = matrix.col_ptr();
        let row_idx = matrix.row_idx();
        let values = matrix.val();

        for j in 0..rhs.ncols() {
            let rhs = col_ref_slice(rhs.col(j));
            let out = col_mut_slice(out.rb_mut().col_mut(j));
            for col in 0..ncols {
                let start = col_ptr[col].zx();
                let end = col_ptr[col + 1].zx();
                // The transpose of CSC is row-like, so we can reduce each
                // output entry directly with a single compensated accumulator.
                let mut acc = CompensatedSum::<T>::default();
                for idx in start..end {
                    acc.add(values[idx] * rhs[row_idx[idx].zx()]);
                }
                out[col] = acc.finish();
            }
        }
    }

    fn adjoint_apply_compensated(
        &self,
        mut out: MatMut<'_, T>,
        rhs: MatRef<'_, T>,
        _par: Par,
        _stack: &mut MemStack,
    ) {
        let matrix = self.canonical();
        let nrows = matrix.nrows().unbound();
        let ncols = matrix.ncols().unbound();
        assert_eq!(rhs.nrows(), nrows);
        assert_eq!(out.nrows(), ncols);
        assert_eq!(out.ncols(), rhs.ncols());

        let col_ptr = matrix.col_ptr();
        let row_idx = matrix.row_idx();
        let values = matrix.val();

        for j in 0..rhs.ncols() {
            let rhs = col_ref_slice(rhs.col(j));
            let out = col_mut_slice(out.rb_mut().col_mut(j));
            for col in 0..ncols {
                let start = col_ptr[col].zx();
                let end = col_ptr[col + 1].zx();
                let mut acc = CompensatedSum::<T>::default();
                for idx in start..end {
                    acc.add(values[idx].conj() * rhs[row_idx[idx].zx()]);
                }
                out[col] = acc.finish();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{CompensatedBiLinOp, CompensatedLinOp};
    use faer::dyn_stack::{MemBuffer, MemStack, StackReq};
    use faer::matrix_free::{BiLinOp, LinOp};
    use faer::sparse::{SparseColMat, SparseRowMat, Triplet};
    use faer::{Mat, Par, Unbind};

    #[test]
    fn compensated_csc_wrapper_matches_plain_apply() {
        let matrix = SparseColMat::<usize, f64>::try_new_from_triplets(
            3,
            3,
            &[
                Triplet::new(0, 0, 2.0),
                Triplet::new(1, 1, -3.0),
                Triplet::new(2, 2, 4.0),
                Triplet::new(0, 2, 1.5),
            ],
        )
        .unwrap();
        let rhs = Mat::from_fn(3, 1, |i, _| [1.0, -2.0, 3.0][i.unbound()]);
        let mut plain = Mat::zeros(3, 1);
        let mut compensated = Mat::zeros(3, 1);
        let op = CompensatedBiLinOp::new(matrix.as_ref());
        let mut scratch = MemBuffer::new(op.apply_scratch(1, Par::Seq));
        let mut plain_mem = MemBuffer::new(StackReq::EMPTY);
        let plain_stack = MemStack::new(&mut plain_mem);
        let compensated_stack = MemStack::new(&mut scratch);
        matrix
            .as_ref()
            .apply(plain.as_mut(), rhs.as_ref(), Par::Seq, plain_stack);
        op.apply(
            compensated.as_mut(),
            rhs.as_ref(),
            Par::Seq,
            compensated_stack,
        );

        assert_eq!(plain, compensated);
    }

    #[test]
    fn compensated_csr_wrapper_matches_plain_adjoint_apply() {
        let matrix = SparseRowMat::<usize, f64>::try_new_from_triplets(
            3,
            3,
            &[
                Triplet::new(0, 0, 2.0),
                Triplet::new(1, 1, -3.0),
                Triplet::new(2, 2, 4.0),
                Triplet::new(0, 2, 1.5),
            ],
        )
        .unwrap();
        let rhs = Mat::from_fn(3, 1, |i, _| [1.0, -2.0, 3.0][i.unbound()]);
        let mut plain = Mat::zeros(3, 1);
        let mut compensated = Mat::zeros(3, 1);
        let op = CompensatedBiLinOp::new(matrix.as_ref());
        let mut scratch = MemBuffer::new(op.transpose_apply_scratch(1, Par::Seq));
        let mut plain_mem = MemBuffer::new(StackReq::EMPTY);
        let plain_stack = MemStack::new(&mut plain_mem);
        let compensated_stack = MemStack::new(&mut scratch);
        matrix
            .as_ref()
            .adjoint_apply(plain.as_mut(), rhs.as_ref(), Par::Seq, plain_stack);
        op.adjoint_apply(
            compensated.as_mut(),
            rhs.as_ref(),
            Par::Seq,
            compensated_stack,
        );

        assert_eq!(plain, compensated);
    }

    #[test]
    fn compensated_lin_wrapper_exposes_dimensions() {
        let matrix = SparseRowMat::<usize, f64>::try_new_from_triplets(
            2,
            3,
            &[Triplet::new(0, 1, 1.0), Triplet::new(1, 2, 2.0)],
        )
        .unwrap();
        let op = CompensatedLinOp::new(matrix.as_ref());
        assert_eq!(op.nrows(), 2);
        assert_eq!(op.ncols(), 3);
    }
}
