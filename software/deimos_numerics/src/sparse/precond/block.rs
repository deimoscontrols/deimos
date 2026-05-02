//! Composable 2x2 block preconditioners.
//!
//! This module intentionally starts small: it provides the block partition and
//! two concrete 2x2 compositions that are useful on their own and also serve
//! as building blocks for later Schur-complement preconditioners.
//!
//! The `conj_apply` / `conj_apply_in_place` paths follow `faer`'s conjugate
//! operator contract. They are not transpose or adjoint block solves; those
//! would belong on `BiLinOp` / `BiPrecond`.
//!
//! # Glossary
//!
//! - **Block split:** Partition of a vector or operator into leading and
//!   trailing sub-blocks.
//! - **Block diagonal:** Preconditioner that inverts only the diagonal blocks.
//! - **Upper triangular block preconditioner:** Preconditioner that keeps the
//!   diagonal and upper off-diagonal blocks of a 2x2 block factor.
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

use super::Precond;

/// Dense-vector partition for a 2x2 block system.
///
/// A vector of length `n0 + n1` is interpreted as:
///
/// ```text
/// [ x0 ]
/// [ x1 ]
/// ```
///
/// where `x0.len() == n0` and `x1.len() == n1`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BlockSplit2 {
    /// Size of the leading block.
    pub n0: usize,
    /// Size of the trailing block.
    pub n1: usize,
}

impl BlockSplit2 {
    /// Creates a new 2-way block split.
    #[inline]
    #[must_use]
    pub fn new(n0: usize, n1: usize) -> Self {
        Self { n0, n1 }
    }

    /// Total dimension of the full vector or matrix acted on by the split.
    #[inline]
    #[must_use]
    pub fn total_dim(self) -> usize {
        self.n0 + self.n1
    }
}

/// Construction-time errors for block preconditioners.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlockPrecondError {
    /// The supplied child operator does not match the expected block dimensions.
    DimensionMismatch {
        /// Identifies the child operator that failed validation.
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
}

/// Block-diagonal preconditioner for a 2x2 system.
///
/// This applies:
///
/// ```text
/// [ M0^-1   0   ]
/// [   0    M1^-1]
/// ```
///
/// to a full vector. It is the simplest useful composition: the two block
/// actions are independent, so the implementation just splits the vector and
/// applies each child preconditioner to its own block.
#[derive(Debug)]
pub struct BlockDiagonalPrecond2<P0, P1> {
    split: BlockSplit2,
    p0: P0,
    p1: P1,
}

impl<P0, P1> BlockDiagonalPrecond2<P0, P1> {
    /// Builds a block-diagonal preconditioner from two child preconditioners.
    pub fn new<T>(split: BlockSplit2, p0: P0, p1: P1) -> Result<Self, BlockPrecondError>
    where
        T: ComplexField,
        P0: Precond<T>,
        P1: Precond<T>,
    {
        validate_block_dims("p0", p0.nrows(), p0.ncols(), split.n0, split.n0)?;
        validate_block_dims("p1", p1.nrows(), p1.ncols(), split.n1, split.n1)?;
        Ok(Self { split, p0, p1 })
    }

    /// Returns the block partition used by this preconditioner.
    #[inline]
    #[must_use]
    pub fn split(&self) -> BlockSplit2 {
        self.split
    }

    /// Borrows the leading child preconditioner.
    #[inline]
    #[must_use]
    pub fn leading(&self) -> &P0 {
        &self.p0
    }

    /// Borrows the trailing child preconditioner.
    #[inline]
    #[must_use]
    pub fn trailing(&self) -> &P1 {
        &self.p1
    }
}

impl<T, P0, P1> LinOp<T> for BlockDiagonalPrecond2<P0, P1>
where
    T: ComplexField,
    P0: Precond<T>,
    P1: Precond<T>,
{
    fn apply_scratch(&self, rhs_ncols: usize, par: Par) -> StackReq {
        StackReq::any_of(&[
            self.p0.apply_in_place_scratch(rhs_ncols, par),
            self.p1.apply_in_place_scratch(rhs_ncols, par),
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

impl<T, P0, P1> Precond<T> for BlockDiagonalPrecond2<P0, P1>
where
    T: ComplexField,
    P0: Precond<T>,
    P1: Precond<T>,
{
    fn apply_in_place_scratch(&self, rhs_ncols: usize, par: Par) -> StackReq {
        <Self as LinOp<T>>::apply_scratch(self, rhs_ncols, par)
    }

    fn apply_in_place(&self, rhs: MatMut<'_, T>, par: Par, stack: &mut MemStack) {
        assert_eq!(rhs.nrows(), self.nrows());

        let (rhs0, rhs1) = rhs.split_at_row_mut(self.split.n0);
        self.p0.apply_in_place(rhs0, par, stack);
        self.p1.apply_in_place(rhs1, par, stack);
    }

    fn conj_apply_in_place(&self, rhs: MatMut<'_, T>, par: Par, stack: &mut MemStack) {
        assert_eq!(rhs.nrows(), self.nrows());

        let (rhs0, rhs1) = rhs.split_at_row_mut(self.split.n0);
        self.p0.conj_apply_in_place(rhs0, par, stack);
        self.p1.conj_apply_in_place(rhs1, par, stack);
    }
}

/// Upper-triangular block preconditioner for a 2x2 system.
///
/// This represents the inverse action of:
///
/// ```text
/// [ A  B ]
/// [ 0  D ]
/// ```
///
/// by applying the exact block solve order:
///
/// 1. solve `D x1 = rhs1`
/// 2. form `rhs0 - B x1`
/// 3. solve `A x0 = rhs0 - B x1`
///
/// The same logic applies for the conjugate action, but using the conjugate
/// block solve and conjugate block operator paths.
#[derive(Debug)]
pub struct BlockUpperTriangularPrecond2<P0, P1, B01> {
    split: BlockSplit2,
    p0: P0,
    p1: P1,
    b01: B01,
}

impl<P0, P1, B01> BlockUpperTriangularPrecond2<P0, P1, B01> {
    /// Builds an upper-triangular block preconditioner.
    pub fn new<T>(split: BlockSplit2, p0: P0, p1: P1, b01: B01) -> Result<Self, BlockPrecondError>
    where
        T: ComplexField,
        P0: Precond<T>,
        P1: Precond<T>,
        B01: LinOp<T>,
    {
        validate_block_dims("p0", p0.nrows(), p0.ncols(), split.n0, split.n0)?;
        validate_block_dims("p1", p1.nrows(), p1.ncols(), split.n1, split.n1)?;
        validate_block_dims("b01", b01.nrows(), b01.ncols(), split.n0, split.n1)?;
        Ok(Self { split, p0, p1, b01 })
    }

    /// Returns the block partition used by this preconditioner.
    #[inline]
    #[must_use]
    pub fn split(&self) -> BlockSplit2 {
        self.split
    }
}

impl<T, P0, P1, B01> LinOp<T> for BlockUpperTriangularPrecond2<P0, P1, B01>
where
    T: ComplexField + Copy,
    P0: Precond<T>,
    P1: Precond<T>,
    B01: LinOp<T>,
{
    fn apply_scratch(&self, rhs_ncols: usize, par: Par) -> StackReq {
        StackReq::all_of(&[
            self.p1.apply_in_place_scratch(rhs_ncols, par),
            temp_mat_scratch::<T>(self.split.n0, rhs_ncols),
            self.b01.apply_scratch(rhs_ncols, par),
            self.p0.apply_in_place_scratch(rhs_ncols, par),
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

impl<T, P0, P1, B01> Precond<T> for BlockUpperTriangularPrecond2<P0, P1, B01>
where
    T: ComplexField + Copy,
    P0: Precond<T>,
    P1: Precond<T>,
    B01: LinOp<T>,
{
    fn apply_in_place_scratch(&self, rhs_ncols: usize, par: Par) -> StackReq {
        <Self as LinOp<T>>::apply_scratch(self, rhs_ncols, par)
    }

    fn apply_in_place(&self, mut rhs: MatMut<'_, T>, par: Par, stack: &mut MemStack) {
        assert_eq!(rhs.nrows(), self.nrows());

        {
            let (_, rhs1) = rhs.rb_mut().split_at_row_mut(self.split.n0);
            self.p1.apply_in_place(rhs1, par, stack);
        }

        let rhs_ncols = rhs.ncols();
        let (mut tmp, stack) = temp_mat_zeroed::<T, _, _>(self.split.n0, rhs_ncols, stack);
        {
            let (_, rhs1) = rhs.rb_mut().split_at_row_mut(self.split.n0);
            self.b01.apply(tmp.as_mat_mut(), rhs1.rb(), par, stack);
        }
        {
            let (mut rhs0, _) = rhs.rb_mut().split_at_row_mut(self.split.n0);
            subtract_in_place(rhs0.rb_mut(), tmp.as_mat_mut().as_ref());
            self.p0.apply_in_place(rhs0, par, stack);
        }
    }

    fn conj_apply_in_place(&self, mut rhs: MatMut<'_, T>, par: Par, stack: &mut MemStack) {
        assert_eq!(rhs.nrows(), self.nrows());

        {
            let (_, rhs1) = rhs.rb_mut().split_at_row_mut(self.split.n0);
            self.p1.conj_apply_in_place(rhs1, par, stack);
        }

        let rhs_ncols = rhs.ncols();
        let (mut tmp, stack) = temp_mat_zeroed::<T, _, _>(self.split.n0, rhs_ncols, stack);
        {
            let (_, rhs1) = rhs.rb_mut().split_at_row_mut(self.split.n0);
            self.b01.conj_apply(tmp.as_mat_mut(), rhs1.rb(), par, stack);
        }
        {
            let (mut rhs0, _) = rhs.rb_mut().split_at_row_mut(self.split.n0);
            subtract_in_place(rhs0.rb_mut(), tmp.as_mat_mut().as_ref());
            self.p0.conj_apply_in_place(rhs0, par, stack);
        }
    }
}

fn validate_block_dims(
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
    let nrows = lhs.nrows();
    let ncols = lhs.ncols();
    for col in 0..ncols {
        for row in 0..nrows {
            lhs[(row, col)] -= rhs[(row, col)];
        }
    }
}

#[cfg(test)]
mod test {
    use super::{
        BlockDiagonalPrecond2, BlockPrecondError, BlockSplit2, BlockUpperTriangularPrecond2,
    };
    use crate::sparse::Precond;
    use crate::sparse::precond::DiagonalPrecond;
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
            assert_eq!(rhs.nrows(), self.ncols());
            assert_eq!(out.nrows(), self.nrows());
            assert_eq!(out.ncols(), rhs.ncols());
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
    fn block_diagonal_applies_child_preconditioners() {
        let split = BlockSplit2::new(2, 1);
        let p0 = DiagonalPrecond::from_inverse_diagonal(&[0.5, 0.25]);
        let p1 = DiagonalPrecond::from_inverse_diagonal(&[2.0]);
        let precond = BlockDiagonalPrecond2::new::<f64>(split, p0, p1).unwrap();

        let mut rhs = Mat::from_fn(3, 1, |i, _| [4.0, 8.0, -3.0][i]);
        let mut buffer = MemBuffer::new(precond.apply_in_place_scratch(1, Par::Seq));
        let stack = MemStack::new(&mut buffer);
        precond.apply_in_place(rhs.as_mut(), Par::Seq, stack);

        assert_eq!(rhs[(0, 0)], 2.0);
        assert_eq!(rhs[(1, 0)], 2.0);
        assert_eq!(rhs[(2, 0)], -6.0);
    }

    #[test]
    fn block_upper_triangular_matches_exact_block_solve() {
        let split = BlockSplit2::new(2, 1);
        let p0 = DiagonalPrecond::from_inverse_diagonal(&[0.5, 1.0 / 3.0]);
        let p1 = DiagonalPrecond::from_inverse_diagonal(&[0.25]);
        // B = [[1], [-2]]
        let b01 = DenseBlockOp::new(2, 1, &[1.0, -2.0]);
        let precond = BlockUpperTriangularPrecond2::new::<f64>(split, p0, p1, b01).unwrap();

        let mut rhs = Mat::from_fn(3, 1, |i, _| [5.0, 7.0, 8.0][i]);
        let mut buffer = MemBuffer::new(precond.apply_in_place_scratch(1, Par::Seq));
        let stack = MemStack::new(&mut buffer);
        precond.apply_in_place(rhs.as_mut(), Par::Seq, stack);

        // Solve:
        // [2 0 1] [x0]   [5]
        // [0 3 -2][x1] = [7]
        // [0 0 4] [x2]   [8]
        // x2 = 2, x1 = (7 + 4) / 3 = 11/3, x0 = (5 - 2) / 2 = 3/2
        assert!((rhs[(0, 0)] - 1.5).abs() < 1.0e-12);
        assert!((rhs[(1, 0)] - 11.0 / 3.0).abs() < 1.0e-12);
        assert!((rhs[(2, 0)] - 2.0).abs() < 1.0e-12);
    }

    #[test]
    fn block_upper_triangular_conjugate_matches_forward_apply_for_real_nonscalar_blocks() {
        let split = BlockSplit2::new(2, 2);
        let p0 = DiagonalPrecond::from_inverse_diagonal(&[0.5, 0.25]);
        let p1 = DiagonalPrecond::from_inverse_diagonal(&[0.2, 0.5]);
        let b01 = DenseBlockOp::new(2, 2, &[1.0, 3.0, 2.0, 4.0]);
        let precond = BlockUpperTriangularPrecond2::new::<f64>(split, p0, p1, b01).unwrap();
        let rhs = Mat::from_fn(4, 1, |i, _| [1.0, -1.0, 2.0, 3.0][i]);
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
    fn constructor_rejects_dimension_mismatch() {
        let split = BlockSplit2::new(2, 1);
        let p0 = DiagonalPrecond::from_inverse_diagonal(&[1.0, 2.0]);
        let p1 = DiagonalPrecond::from_inverse_diagonal(&[3.0, 4.0]);

        assert!(matches!(
            BlockDiagonalPrecond2::new::<f64>(split, p0, p1),
            Err(BlockPrecondError::DimensionMismatch { which: "p1", .. })
        ));
    }
}
