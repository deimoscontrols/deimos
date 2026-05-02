//! Schur-complement-based 2x2 block preconditioner.
//!
//! This module applies the exact block-LU inverse action for a 2x2 system
//! using:
//!
//! - an inverse application for the `A` block
//! - an inverse application for the Schur block `S`
//! - off-diagonal block operators `B` and `C`
//!
//! Its `conj_apply` and `conj_apply_in_place` methods follow `faer`'s
//! conjugate-operator contract. They do not implement transpose or adjoint
//! block solves; those belong on `BiLinOp` / `BiPrecond`.
//!
//! # Glossary
//!
//! - **Schur complement:** Reduced trailing block `S = D - C A^{-1} B`.
//! - **Block-LU inverse action:** Solve induced by factoring a 2x2 block matrix
//!   into lower and upper block factors.
//! - **Inverse application:** Applying a preconditioner as if it were the
//!   inverse of an operator.
//! - **Conjugate application:** `faer` contract for applying the conjugated
//!   operator, distinct from transpose or adjoint solves.

use faer::Par;
use faer::dyn_stack::{MemStack, StackReq};
use faer::linalg::{temp_mat_scratch, temp_mat_zeroed};
use faer::mat::AsMatMut;
use faer::matrix_free::LinOp;
use faer::prelude::{Reborrow, ReborrowMut};
use faer::{MatMut, MatRef};
use faer_traits::ComplexField;

use super::{BlockPrecondError, BlockSplit2, Precond};

/// Schur-complement-based preconditioner for a 2x2 block system.
///
/// For a block matrix
///
/// ```text
/// [ A  B ]
/// [ C  D ]
/// ```
///
/// with Schur complement `S = D - C A^{-1} B`, this preconditioner applies the
/// exact block-LU inverse action
///
/// ```text
/// M^-1
/// = [ I        -A^-1 B ] [ A^-1   0 ] [ I   0 ]
///   [ 0             I   ] [  0    S^-1] [ -C A^-1  I ]
/// ```
///
/// to a full vector. In practice that means:
///
/// 1. compute `z0 = A^-1 rhs0`
/// 2. update `rhs1 <- rhs1 - C z0`
/// 3. compute `z1 = S^-1 rhs1`
/// 4. update `z0 <- z0 - A^-1 (B z1)`
///
/// The same structure is used for the conjugate action.
#[derive(Debug)]
pub struct SchurPrecond2<AInv, SInv, B, C> {
    split: BlockSplit2,
    ainv: AInv,
    sinv: SInv,
    b: B,
    c: C,
}

impl<AInv, SInv, B, C> SchurPrecond2<AInv, SInv, B, C> {
    /// Builds a Schur-complement preconditioner from block components.
    pub fn new<T>(
        split: BlockSplit2,
        ainv: AInv,
        sinv: SInv,
        b: B,
        c: C,
    ) -> Result<Self, BlockPrecondError>
    where
        T: ComplexField,
        AInv: Precond<T>,
        SInv: Precond<T>,
        B: LinOp<T>,
        C: LinOp<T>,
    {
        validate_dims("ainv", ainv.nrows(), ainv.ncols(), split.n0, split.n0)?;
        validate_dims("sinv", sinv.nrows(), sinv.ncols(), split.n1, split.n1)?;
        validate_dims("b", b.nrows(), b.ncols(), split.n0, split.n1)?;
        validate_dims("c", c.nrows(), c.ncols(), split.n1, split.n0)?;
        Ok(Self {
            split,
            ainv,
            sinv,
            b,
            c,
        })
    }

    /// Returns the block partition used by this preconditioner.
    #[inline]
    #[must_use]
    pub fn split(&self) -> BlockSplit2 {
        self.split
    }
}

impl<T, AInv, SInv, B, C> LinOp<T> for SchurPrecond2<AInv, SInv, B, C>
where
    T: ComplexField + Copy,
    AInv: Precond<T>,
    SInv: Precond<T>,
    B: LinOp<T>,
    C: LinOp<T>,
{
    fn apply_scratch(&self, rhs_ncols: usize, par: Par) -> StackReq {
        StackReq::all_of(&[
            temp_mat_scratch::<T>(self.split.n1, rhs_ncols),
            temp_mat_scratch::<T>(self.split.n0, rhs_ncols),
            self.ainv.apply_in_place_scratch(rhs_ncols, par),
            self.sinv.apply_in_place_scratch(rhs_ncols, par),
            self.b.apply_scratch(rhs_ncols, par),
            self.c.apply_scratch(rhs_ncols, par),
        ])
    }

    fn nrows(&self) -> usize {
        self.split.total_dim()
    }

    fn ncols(&self) -> usize {
        self.split.total_dim()
    }

    fn apply(&self, mut out: MatMut<'_, T>, rhs: MatRef<'_, T>, par: Par, stack: &mut MemStack) {
        assert_eq!(rhs.nrows(), self.ncols());
        assert_eq!(out.nrows(), self.nrows());
        assert_eq!(out.ncols(), rhs.ncols());

        out.rb_mut().copy_from(rhs);
        self.apply_in_place(out, par, stack);
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
        self.conj_apply_in_place(out, par, stack);
    }
}

impl<T, AInv, SInv, B, C> Precond<T> for SchurPrecond2<AInv, SInv, B, C>
where
    T: ComplexField + Copy,
    AInv: Precond<T>,
    SInv: Precond<T>,
    B: LinOp<T>,
    C: LinOp<T>,
{
    fn apply_in_place_scratch(&self, rhs_ncols: usize, par: Par) -> StackReq {
        <Self as LinOp<T>>::apply_scratch(self, rhs_ncols, par)
    }

    fn apply_in_place(&self, mut rhs: MatMut<'_, T>, par: Par, stack: &mut MemStack) {
        assert_eq!(rhs.nrows(), self.nrows());
        let rhs_ncols = rhs.ncols();

        {
            let (mut rhs0, mut rhs1) = rhs.rb_mut().split_at_row_mut(self.split.n0);
            self.ainv.apply_in_place(rhs0.rb_mut(), par, stack);

            let (mut tmp_c, stack) = temp_mat_zeroed::<T, _, _>(self.split.n1, rhs_ncols, stack);
            self.c.apply(tmp_c.as_mat_mut(), rhs0.rb(), par, stack);
            subtract_in_place(rhs1.rb_mut(), tmp_c.as_mat_mut().as_ref());
            self.sinv.apply_in_place(rhs1.rb_mut(), par, stack);
        }

        {
            let (_, rhs1) = rhs.rb_mut().split_at_row_mut(self.split.n0);
            let (mut tmp_b, stack) = temp_mat_zeroed::<T, _, _>(self.split.n0, rhs_ncols, stack);
            self.b.apply(tmp_b.as_mat_mut(), rhs1.rb(), par, stack);
            self.ainv.apply_in_place(tmp_b.as_mat_mut(), par, stack);

            let (mut rhs0, _) = rhs.rb_mut().split_at_row_mut(self.split.n0);
            subtract_in_place(rhs0.rb_mut(), tmp_b.as_mat_mut().as_ref());
        }
    }

    fn conj_apply_in_place(&self, mut rhs: MatMut<'_, T>, par: Par, stack: &mut MemStack) {
        assert_eq!(rhs.nrows(), self.nrows());
        let rhs_ncols = rhs.ncols();

        {
            let (mut rhs0, mut rhs1) = rhs.rb_mut().split_at_row_mut(self.split.n0);
            self.ainv.conj_apply_in_place(rhs0.rb_mut(), par, stack);

            let (mut tmp_c, stack) = temp_mat_zeroed::<T, _, _>(self.split.n1, rhs_ncols, stack);
            self.c.conj_apply(tmp_c.as_mat_mut(), rhs0.rb(), par, stack);
            subtract_in_place(rhs1.rb_mut(), tmp_c.as_mat_mut().as_ref());
            self.sinv.conj_apply_in_place(rhs1.rb_mut(), par, stack);
        }

        {
            let (_, rhs1) = rhs.rb_mut().split_at_row_mut(self.split.n0);
            let (mut tmp_b, stack) = temp_mat_zeroed::<T, _, _>(self.split.n0, rhs_ncols, stack);
            self.b.conj_apply(tmp_b.as_mat_mut(), rhs1.rb(), par, stack);
            self.ainv
                .conj_apply_in_place(tmp_b.as_mat_mut(), par, stack);

            let (mut rhs0, _) = rhs.rb_mut().split_at_row_mut(self.split.n0);
            subtract_in_place(rhs0.rb_mut(), tmp_b.as_mat_mut().as_ref());
        }
    }
}

fn validate_dims(
    which: &'static str,
    actual_nrows: usize,
    actual_ncols: usize,
    expected_nrows: usize,
    expected_ncols: usize,
) -> Result<(), BlockPrecondError> {
    if actual_nrows != expected_nrows || actual_ncols != expected_ncols {
        return Err(BlockPrecondError::DimensionMismatch {
            which,
            expected_nrows,
            expected_ncols,
            actual_nrows,
            actual_ncols,
        });
    }
    Ok(())
}

fn subtract_in_place<T: ComplexField + Copy>(mut lhs: MatMut<'_, T>, rhs: MatRef<'_, T>) {
    assert_eq!(lhs.nrows(), rhs.nrows());
    assert_eq!(lhs.ncols(), rhs.ncols());
    for col in 0..lhs.ncols() {
        for row in 0..lhs.nrows() {
            lhs[(row, col)] -= rhs[(row, col)];
        }
    }
}

#[cfg(test)]
mod test {
    use super::SchurPrecond2;
    use crate::sparse::precond::DiagonalPrecond;
    use crate::sparse::{BlockPrecondError, BlockSplit2, Precond};
    use faer::dyn_stack::{MemBuffer, MemStack, StackReq};
    use faer::matrix_free::LinOp;
    use faer::{Mat, MatMut, MatRef, Par};

    #[derive(Clone, Debug)]
    struct DenseBlockOp {
        data: Mat<f64>,
    }

    impl DenseBlockOp {
        fn new(nrows: usize, ncols: usize, values: &[f64]) -> Self {
            assert_eq!(values.len(), nrows * ncols);
            let data = Mat::from_fn(nrows, ncols, |i, j| values[i + nrows * j]);
            Self { data }
        }
    }

    impl LinOp<f64> for DenseBlockOp {
        fn apply_scratch(&self, _rhs_ncols: usize, _par: Par) -> StackReq {
            StackReq::EMPTY
        }

        fn nrows(&self) -> usize {
            self.data.nrows()
        }

        fn ncols(&self) -> usize {
            self.data.ncols()
        }

        fn apply(
            &self,
            mut out: MatMut<'_, f64>,
            rhs: MatRef<'_, f64>,
            _par: Par,
            _stack: &mut MemStack,
        ) {
            for col in 0..out.ncols() {
                for row in 0..out.nrows() {
                    out[(row, col)] = 0.0;
                }
            }
            for col in 0..rhs.ncols() {
                for k in 0..self.ncols() {
                    let rhs_value = rhs[(k, col)];
                    for row in 0..self.nrows() {
                        out[(row, col)] += self.data[(row, k)] * rhs_value;
                    }
                }
            }
        }

        fn conj_apply(
            &self,
            out: MatMut<'_, f64>,
            rhs: MatRef<'_, f64>,
            par: Par,
            stack: &mut MemStack,
        ) {
            self.apply(out, rhs, par, stack);
        }
    }

    #[test]
    fn schur_preconditioner_matches_exact_block_solve() {
        let split = BlockSplit2::new(2, 1);
        let ainv = DiagonalPrecond::from_inverse_diagonal(&[0.5, 1.0 / 3.0]);
        let sinv = DiagonalPrecond::from_inverse_diagonal(&[0.25]);
        let b = DenseBlockOp::new(2, 1, &[1.0, 2.0]);
        let c = DenseBlockOp::new(1, 2, &[2.0, 3.0]);
        let precond = SchurPrecond2::new::<f64>(split, ainv, sinv, b, c).unwrap();

        let mut rhs = Mat::from_fn(3, 1, |i, _| [5.0, 7.0, 8.0][i]);
        let mut buffer = MemBuffer::new(precond.apply_in_place_scratch(1, Par::Seq));
        let stack = MemStack::new(&mut buffer);
        precond.apply_in_place(rhs.as_mut(), Par::Seq, stack);

        assert!((rhs[(0, 0)] - 3.0).abs() < 1.0e-12);
        assert!((rhs[(1, 0)] - 3.0).abs() < 1.0e-12);
        assert!((rhs[(2, 0)] + 1.0).abs() < 1.0e-12);
    }

    #[test]
    fn schur_preconditioner_conjugate_matches_forward_apply_for_real_nonscalar_blocks() {
        let split = BlockSplit2::new(2, 2);
        let ainv = DiagonalPrecond::from_inverse_diagonal(&[0.5, 0.25]);
        let sinv = DiagonalPrecond::from_inverse_diagonal(&[0.2, 0.5]);
        let b = DenseBlockOp::new(2, 2, &[1.0, 3.0, 2.0, 4.0]);
        let c = DenseBlockOp::new(2, 2, &[2.0, 1.0, 0.0, 5.0]);
        let precond = SchurPrecond2::new::<f64>(split, ainv, sinv, b, c).unwrap();

        let rhs = Mat::from_fn(4, 1, |i, _| [1.0, -2.0, 3.0, 4.0][i]);
        let mut expected = rhs.clone();
        let mut out = rhs.clone();
        let mut buffer = MemBuffer::new(precond.apply_in_place_scratch(1, Par::Seq));
        let stack = MemStack::new(&mut buffer);
        precond.apply_in_place(expected.as_mut(), Par::Seq, stack);
        let stack = MemStack::new(&mut buffer);
        precond.conj_apply_in_place(out.as_mut(), Par::Seq, stack);

        for row in 0..4 {
            assert!((out[(row, 0)] - expected[(row, 0)]).abs() < 1.0e-12);
        }
    }

    #[test]
    fn schur_preconditioner_rejects_dimension_mismatch() {
        let split = BlockSplit2::new(2, 1);
        let ainv = DiagonalPrecond::from_inverse_diagonal(&[0.5, 1.0 / 3.0]);
        let sinv = DiagonalPrecond::from_inverse_diagonal(&[0.25]);
        let b = DenseBlockOp::new(3, 1, &[1.0, 2.0, 3.0]);
        let c = DenseBlockOp::new(1, 2, &[2.0, 3.0]);

        assert!(matches!(
            SchurPrecond2::new::<f64>(split, ainv, sinv, b, c),
            Err(BlockPrecondError::DimensionMismatch { which: "b", .. })
        ));
    }
}
