//! Implicit 2x2 Schur-complement operator.
//!
//! For a block matrix
//!
//! ```text
//! [ A  B ]
//! [ C  D ]
//! ```
//!
//! this module represents the trailing Schur complement
//!
//! `S = D - C A^{-1} B`
//!
//! as a matrix-free linear operator. The implementation is intentionally
//! implicit: it reuses existing inverse application and matvec machinery rather
//! than assembling a new sparse matrix with potentially heavy fill.
//!
//! The `LinOp::conj_apply` path follows `faer`'s conjugate-operator contract,
//! not a transpose or adjoint contract. Adjoint application belongs on
//! `BiLinOp`, which this type does not implement.
//!
//! # Glossary
//!
//! - **Schur complement:** Reduced operator `S = D - C A^{-1} B`.
//! - **Implicit operator:** Operator applied through sub-operator solves and
//!   matvecs rather than explicit matrix assembly.
//! - **Inverse application:** Applying an already-available solver or
//!   preconditioner for one block.
//! - **Conjugate application:** `faer` contract for applying the conjugated
//!   operator, distinct from transpose or adjoint application.

use faer::Par;
use faer::dyn_stack::{MemStack, StackReq};
use faer::linalg::{temp_mat_scratch, temp_mat_zeroed};
use faer::mat::AsMatMut;
use faer::matrix_free::LinOp;
use faer::prelude::ReborrowMut;
use faer::{MatMut, MatRef};
use faer_traits::ComplexField;

use super::Precond;

/// Construction-time errors for [`SchurComplement2`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SchurComplementError {
    /// A supplied child operator has dimensions incompatible with the Schur layout.
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

/// Implicit Schur-complement operator for a 2x2 block system.
///
/// The `A^{-1}` slot is modeled as a `Precond<T>` because the operator only
/// needs inverse application on dense vectors. That inverse may be exact,
/// lagged, or approximate.
#[derive(Debug)]
pub struct SchurComplement2<AInv, B, C, D> {
    ainv: AInv,
    b: B,
    c: C,
    d: D,
    n_a: usize,
    n_s: usize,
}

impl<AInv, B, C, D> SchurComplement2<AInv, B, C, D> {
    /// Builds an implicit Schur-complement operator.
    pub fn new<T>(ainv: AInv, b: B, c: C, d: D) -> Result<Self, SchurComplementError>
    where
        T: ComplexField,
        AInv: Precond<T>,
        B: LinOp<T>,
        C: LinOp<T>,
        D: LinOp<T>,
    {
        let n_a = ainv.nrows();
        let n_s = d.nrows();
        validate_dims("ainv", ainv.nrows(), ainv.ncols(), n_a, n_a)?;
        validate_dims("b", b.nrows(), b.ncols(), n_a, n_s)?;
        validate_dims("c", c.nrows(), c.ncols(), n_s, n_a)?;
        validate_dims("d", d.nrows(), d.ncols(), n_s, n_s)?;

        Ok(Self {
            ainv,
            b,
            c,
            d,
            n_a,
            n_s,
        })
    }

    /// Dimension of the eliminated `A` block.
    #[inline]
    #[must_use]
    pub fn n_a(&self) -> usize {
        self.n_a
    }

    /// Dimension of the Schur block.
    #[inline]
    #[must_use]
    pub fn n_s(&self) -> usize {
        self.n_s
    }

    /// Borrows the inverse-application object for the `A` block.
    #[inline]
    #[must_use]
    pub fn ainv(&self) -> &AInv {
        &self.ainv
    }
}

impl<T, AInv, B, C, D> LinOp<T> for SchurComplement2<AInv, B, C, D>
where
    T: ComplexField + Copy,
    AInv: Precond<T>,
    B: LinOp<T>,
    C: LinOp<T>,
    D: LinOp<T>,
{
    fn apply_scratch(&self, rhs_ncols: usize, par: Par) -> StackReq {
        StackReq::all_of(&[
            temp_mat_scratch::<T>(self.n_a, rhs_ncols),
            temp_mat_scratch::<T>(self.n_s, rhs_ncols),
            self.b.apply_scratch(rhs_ncols, par),
            self.ainv.apply_in_place_scratch(rhs_ncols, par),
            self.c.apply_scratch(rhs_ncols, par),
            self.d.apply_scratch(rhs_ncols, par),
        ])
    }

    fn nrows(&self) -> usize {
        self.n_s
    }

    fn ncols(&self) -> usize {
        self.n_s
    }

    fn apply(&self, mut out: MatMut<'_, T>, rhs: MatRef<'_, T>, par: Par, stack: &mut MemStack) {
        assert_eq!(rhs.nrows(), self.ncols());
        assert_eq!(out.nrows(), self.nrows());
        assert_eq!(out.ncols(), rhs.ncols());

        let rhs_ncols = rhs.ncols();
        let (mut tmp_b, stack) = temp_mat_zeroed::<T, _, _>(self.n_a, rhs_ncols, stack);
        self.b.apply(tmp_b.as_mat_mut(), rhs, par, stack);
        self.ainv.apply_in_place(tmp_b.as_mat_mut(), par, stack);

        let (mut tmp_c, stack) = temp_mat_zeroed::<T, _, _>(self.n_s, rhs_ncols, stack);
        self.c
            .apply(tmp_c.as_mat_mut(), tmp_b.as_mat_mut().as_ref(), par, stack);
        self.d.apply(out.rb_mut(), rhs, par, stack);
        subtract_in_place(out, tmp_c.as_mat_mut().as_ref());
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

        let rhs_ncols = rhs.ncols();
        let (mut tmp_b, stack) = temp_mat_zeroed::<T, _, _>(self.n_a, rhs_ncols, stack);
        self.b.conj_apply(tmp_b.as_mat_mut(), rhs, par, stack);
        self.ainv
            .conj_apply_in_place(tmp_b.as_mat_mut(), par, stack);

        let (mut tmp_c, stack) = temp_mat_zeroed::<T, _, _>(self.n_s, rhs_ncols, stack);
        self.c
            .conj_apply(tmp_c.as_mat_mut(), tmp_b.as_mat_mut().as_ref(), par, stack);
        self.d.conj_apply(out.rb_mut(), rhs, par, stack);
        subtract_in_place(out, tmp_c.as_mat_mut().as_ref());
    }
}

fn validate_dims(
    which: &'static str,
    actual_nrows: usize,
    actual_ncols: usize,
    expected_nrows: usize,
    expected_ncols: usize,
) -> Result<(), SchurComplementError> {
    if actual_nrows != expected_nrows || actual_ncols != expected_ncols {
        return Err(SchurComplementError::DimensionMismatch {
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
    use super::{SchurComplement2, SchurComplementError};
    use crate::sparse::DiagonalPrecond;
    use faer::dyn_stack::{MemBuffer, MemStack, StackReq};
    use faer::matrix_free::LinOp;
    use faer::{Mat, MatMut, MatRef, Par, c64};
    use faer_traits::ComplexField;
    use faer_traits::ext::ComplexFieldExt;

    #[derive(Clone, Debug)]
    struct DenseBlockOp<T> {
        data: Mat<T>,
    }

    impl<T: ComplexField + Copy> DenseBlockOp<T> {
        fn new(nrows: usize, ncols: usize, values: &[T]) -> Self {
            assert_eq!(values.len(), nrows * ncols);
            let data = Mat::from_fn(nrows, ncols, |i, j| values[i + nrows * j]);
            Self { data }
        }
    }

    impl<T: ComplexField + Copy> LinOp<T> for DenseBlockOp<T> {
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
            mut out: MatMut<'_, T>,
            rhs: MatRef<'_, T>,
            _par: Par,
            _stack: &mut MemStack,
        ) {
            assert_eq!(rhs.nrows(), self.ncols());
            assert_eq!(out.nrows(), self.nrows());
            assert_eq!(out.ncols(), rhs.ncols());
            for col in 0..out.ncols() {
                for row in 0..out.nrows() {
                    out[(row, col)] = faer_traits::math_utils::zero::<T>();
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
            mut out: MatMut<'_, T>,
            rhs: MatRef<'_, T>,
            _par: Par,
            _stack: &mut MemStack,
        ) {
            assert_eq!(rhs.nrows(), self.ncols());
            assert_eq!(out.nrows(), self.nrows());
            assert_eq!(out.ncols(), rhs.ncols());
            for col in 0..out.ncols() {
                for row in 0..out.nrows() {
                    out[(row, col)] = faer_traits::math_utils::zero::<T>();
                }
            }
            for col in 0..rhs.ncols() {
                for k in 0..self.ncols() {
                    let rhs_value = rhs[(k, col)];
                    for row in 0..self.nrows() {
                        out[(row, col)] += self.data[(row, k)].conj() * rhs_value;
                    }
                }
            }
        }
    }

    #[test]
    fn applies_real_schur_complement_operator() {
        let ainv = DiagonalPrecond::from_inverse_diagonal(&[0.5, 0.25]);
        let b = DenseBlockOp::new(2, 1, &[1.0, 2.0]);
        let c = DenseBlockOp::new(1, 2, &[3.0, 5.0]);
        let d = DenseBlockOp::new(1, 1, &[7.0]);
        let schur = SchurComplement2::new::<f64>(ainv, b, c, d).unwrap();

        let rhs = Mat::from_fn(1, 1, |_, _| 4.0);
        let mut out = Mat::<f64>::zeros(1, 1);
        let mut buffer = MemBuffer::new(schur.apply_scratch(1, Par::Seq));
        let stack = MemStack::new(&mut buffer);
        schur.apply(out.as_mut(), rhs.as_ref(), Par::Seq, stack);

        // S = 7 - [3 5] * diag(1/2, 1/4) * [1, 2]^T = 7 - 4 = 3
        assert!((out[(0, 0)] - 12.0).abs() < 1.0e-12);
    }

    #[test]
    fn applies_complex_conjugate_schur_operator() {
        let ainv = DiagonalPrecond::from_inverse_diagonal(&[c64::new(0.5, 0.0)]);
        let b = DenseBlockOp::new(1, 1, &[c64::new(1.0, 2.0)]);
        let c = DenseBlockOp::new(1, 1, &[c64::new(3.0, -1.0)]);
        let d = DenseBlockOp::new(1, 1, &[c64::new(5.0, 0.5)]);
        let expected_symbol = d.data[(0, 0)].conj()
            - c.data[(0, 0)].conj() * ainv.inverse_diagonal()[0].conj() * b.data[(0, 0)].conj();
        let schur = SchurComplement2::new::<c64>(ainv, b, c, d).unwrap();

        let rhs = Mat::from_fn(1, 1, |_, _| c64::new(2.0, -1.0));
        let mut out = Mat::<c64>::zeros(1, 1);
        let mut buffer = MemBuffer::new(schur.apply_scratch(1, Par::Seq));
        let stack = MemStack::new(&mut buffer);
        schur.conj_apply(out.as_mut(), rhs.as_ref(), Par::Seq, stack);

        let expected = expected_symbol * rhs[(0, 0)];
        let err = (out[(0, 0)] - expected).abs1();
        assert!(err < 1.0e-12);
    }

    #[test]
    fn conjugate_schur_operator_matches_forward_apply_for_real_nonscalar_blocks() {
        let ainv = DiagonalPrecond::from_inverse_diagonal(&[0.5, 0.25]);
        let b = DenseBlockOp::new(2, 2, &[1.0, 0.0, 2.0, 1.0]);
        let c = DenseBlockOp::new(2, 2, &[3.0, 4.0, 1.0, 2.0]);
        let d = DenseBlockOp::new(2, 2, &[5.0, 6.0, 7.0, 8.0]);
        let schur = SchurComplement2::new::<f64>(ainv, b, c, d).unwrap();

        let rhs = Mat::from_fn(2, 1, |i, _| [2.0, -1.0][i]);
        let mut expected = Mat::<f64>::zeros(2, 1);
        let mut out = Mat::<f64>::zeros(2, 1);
        let mut buffer = MemBuffer::new(schur.apply_scratch(1, Par::Seq));
        let stack = MemStack::new(&mut buffer);
        schur.apply(expected.as_mut(), rhs.as_ref(), Par::Seq, stack);
        let stack = MemStack::new(&mut buffer);
        schur.conj_apply(out.as_mut(), rhs.as_ref(), Par::Seq, stack);

        for row in 0..2 {
            assert!((out[(row, 0)] - expected[(row, 0)]).abs() < 1.0e-12);
        }
    }

    #[test]
    fn rejects_dimension_mismatch() {
        let ainv = DiagonalPrecond::from_inverse_diagonal(&[1.0, 2.0]);
        let b = DenseBlockOp::new(3, 1, &[1.0, 0.0, 2.0]);
        let c = DenseBlockOp::new(1, 2, &[1.0, 1.0]);
        let d = DenseBlockOp::new(1, 1, &[1.0]);

        assert!(matches!(
            SchurComplement2::new::<f64>(ainv, b, c, d),
            Err(SchurComplementError::DimensionMismatch { which: "b", .. })
        ));
    }
}
