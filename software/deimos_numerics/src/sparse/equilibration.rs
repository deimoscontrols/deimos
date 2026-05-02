//! Iterative sparse-matrix equilibration.
//!
//! Equilibration rescales a matrix on the left and right so that row and
//! column magnitudes become more comparable. Unlike an incomplete
//! factorization, this does not change the exact solution of the linear system:
//! it is just a change of units.
//!
//! For a system `A x = b`, this module computes positive real diagonal scales
//! `D_r` and `D_c`, forms the balanced system
//!
//! `D_r A D_c y = D_r b`
//!
//! and recovers the original variables with
//!
//! `x = D_c y`.
//!
//! Intuitively:
//!
//! - `D_r` rescales equations, so very large or very small rows stop dominating
//!   the solve
//! - `D_c` rescales unknowns, so very large or very small columns stop
//!   stretching the search space
//! - using both simultaneously balances the operator without discarding any
//!   information
//!
//! The implementation here uses simultaneous iterative infinity-norm updates.
//! That choice matters for symmetric or Hermitian inputs: if the current matrix
//! is exactly symmetric/Hermitian, the row and column norms match at each
//! iteration, so the left and right updates also match. In that case the
//! general asymmetric algorithm naturally collapses to the symmetric one.
//!
//! # Two Intuitions
//!
//! 1. **Units view.** Equilibration changes the units of equations and unknowns
//!    so no row or column dominates purely because of scale.
//! 2. **Preconditioning view.** It is a cheap first layer of conditioning
//!    improvement that often makes later direct or iterative solves behave much
//!    better without changing the true solution.
//!
//! # Glossary
//!
//! - **Row scale / column scale:** Positive diagonal scaling factors.
//! - **Infinity norm balancing:** Matching row/column max magnitudes toward
//!   one.
//!
//! # Mathematical Formulation
//!
//! The module iteratively builds diagonal scalings `D_r` and `D_c` so the
//! scaled matrix `D_r A D_c` has row and column infinity norms near one.
//!
//! # Implementation Notes
//!
//! - Updates are simultaneous on rows and columns rather than alternating one
//!   side to convergence before the other.
//! - The same scales can be applied consistently to matrices, vectors, and
//!   solution recovery.

use alloc::vec::Vec;
use faer::sparse::{
    SparseColMat, SparseColMatRef, SparseRowMat, SparseRowMatRef, SymbolicSparseColMatRef,
    SymbolicSparseRowMatRef,
};
use faer::{Index, Unbind};
use faer_traits::ComplexField;
use faer_traits::Conjugate;
use faer_traits::ext::ComplexFieldExt;
use num_traits::Float;

/// Parameters controlling iterative equilibration.
///
/// The algorithm repeatedly measures row and column infinity norms of the
/// current scaled matrix, then applies square-root updates to both sides at
/// once. `tol` controls when that process is considered "balanced enough".
///
/// `norm_floor` and `norm_ceil` bound the norm values before inversion. They
/// keep extremely tiny or extremely large entries from producing unusable
/// scaling factors.
///
/// Args:
///   max_iters: Maximum number of balancing iterations.
///   tol: Stopping tolerance on row/column infinity-norm deviation from `1`.
///     It is dimensionless.
///   norm_floor: Lower clamp on measured norms before inversion. It is
///     dimensionless.
///   norm_ceil: Upper clamp on measured norms before inversion. It is
///     dimensionless.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EquilibrationParams<R> {
    /// Maximum number of balancing iterations.
    pub max_iters: usize,
    /// Stop once every row and column infinity norm is within `tol` of `1`.
    pub tol: R,
    /// Lower clamp applied to measured row/column norms before inversion.
    pub norm_floor: R,
    /// Upper clamp applied to measured row/column norms before inversion.
    pub norm_ceil: R,
}

impl<R: Float> Default for EquilibrationParams<R> {
    fn default() -> Self {
        Self {
            max_iters: 8,
            tol: R::from(1.0e-2).unwrap(),
            norm_floor: R::epsilon(),
            norm_ceil: R::epsilon().recip(),
        }
    }
}

/// Errors that can occur while building or applying an [`Equilibration`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EquilibrationError {
    /// The caller supplied parameters that do not define a meaningful iteration.
    InvalidParams {
        /// Short explanation of the invalid parameter choice.
        reason: &'static str,
    },
    /// The matrix has a structurally or numerically zero row.
    ZeroRow {
        /// Index of the zero row.
        index: usize,
    },
    /// The matrix has a structurally or numerically zero column.
    ZeroColumn {
        /// Index of the zero column.
        index: usize,
    },
}

/// Two-sided row and column equilibration factors.
///
/// `row_scale[i]` is the positive real factor applied to row `i`, and
/// `col_scale[j]` is the positive real factor applied to column `j`.
///
/// Applying both gives the balanced matrix
///
/// `A_eq = D_r A D_c`
///
/// with `D_r = diag(row_scale)` and `D_c = diag(col_scale)`.
///
/// These scales are the cumulative result of several balancing iterations. At
/// each iteration we look at the current row and column infinity norms,
/// compute square-root inverse corrections, and multiply them into the stored
/// scales. Keeping only the final real scale vectors is enough to reconstruct
/// the balanced matrix and to map vectors between the original and balanced
/// coordinate systems.
///
/// `row_scale` has length `nrows()` and `col_scale` has length `ncols()`.
#[derive(Clone, Debug, PartialEq)]
pub struct Equilibration<T: ComplexField> {
    row_scale: Vec<T::Real>,
    col_scale: Vec<T::Real>,
}

impl<T: ComplexField> Equilibration<T> {
    /// Builds row and column scales from a CSC matrix.
    ///
    /// The matrix may be unsymmetric; the algorithm updates row and column
    /// scales simultaneously from the same current matrix state.
    ///
    /// Technically, this runs a Ruiz-style infinity-norm iteration over a
    /// mutable working copy of the numeric values. The sparse pattern is read
    /// once up front and reused throughout the iteration.
    ///
    /// Intuitively, this asks two questions about the current scaled matrix:
    ///
    /// - which rows still have entries that are much too large or too small?
    /// - which columns still have entries that are much too large or too small?
    ///
    /// The square-root update splits the correction evenly between left and
    /// right scaling so that we do not over-correct in one step.
    ///
    /// Args:
    ///   matrix: Sparse CSC matrix with shape `(nrows, ncols)`.
    ///   params: Iteration limit and balancing tolerances. These are
    ///     dimensionless because they describe relative scaling.
    ///
    /// Returns:
    ///   Row scales with length `nrows` and column scales with length `ncols`.
    pub fn compute_from_csc<I, ViewT>(
        matrix: SparseColMatRef<'_, I, ViewT>,
        params: EquilibrationParams<T::Real>,
    ) -> Result<Self, EquilibrationError>
    where
        T: Copy,
        T::Real: Float,
        I: Index,
        ViewT: Conjugate<Canonical = T>,
    {
        let matrix = matrix.canonical();
        let nrows = matrix.nrows().unbound();
        let ncols = matrix.ncols().unbound();
        let col_ptr = matrix.col_ptr();
        let row_idx = matrix.row_idx();
        // The original matrix is left untouched during analysis. We instead
        // rebalance a scratch copy of the values so each iteration measures the
        // same sparse pattern under progressively better scaling.
        let mut working = matrix.val().to_vec();
        let one = T::Real::one();
        let zero = T::Real::zero();

        let mut row_scale = vec![one; nrows];
        let mut col_scale = vec![one; ncols];
        let mut row_norm = vec![zero; nrows];
        let mut col_norm = vec![zero; ncols];
        let mut row_update = vec![one; nrows];
        let mut col_update = vec![one; ncols];

        validate_params(params)?;

        for _ in 0..params.max_iters {
            row_norm.fill(zero);
            col_norm.fill(zero);

            // Row and column norms are measured from the same current matrix
            // state before either side is updated. That is what makes the
            // asymmetric implementation naturally collapse to the symmetric one
            // on exactly symmetric or Hermitian inputs.
            for col in 0..ncols {
                let mut max_in_col = zero;
                for idx in col_ptr[col].zx()..col_ptr[col + 1].zx() {
                    let row = row_idx[idx].zx();
                    let mag = working[idx].abs();
                    if mag > row_norm[row] {
                        row_norm[row] = mag;
                    }
                    if mag > max_in_col {
                        max_in_col = mag;
                    }
                }
                col_norm[col] = max_in_col;
            }

            validate_nonzero_norms(&row_norm, &col_norm)?;
            if max_deviation(&row_norm, &col_norm) <= params.tol {
                break;
            }

            compute_updates(
                &row_norm,
                &mut row_update,
                &mut row_scale,
                params.norm_floor,
                params.norm_ceil,
            );
            compute_updates(
                &col_norm,
                &mut col_update,
                &mut col_scale,
                params.norm_floor,
                params.norm_ceil,
            );

            // Apply the freshly computed row and column corrections together so
            // the next iteration sees the next balanced matrix `D_r A D_c`.
            for col in 0..ncols {
                let col_factor = col_update[col];
                for idx in col_ptr[col].zx()..col_ptr[col + 1].zx() {
                    let row = row_idx[idx].zx();
                    working[idx] = working[idx].mul_real(row_update[row]).mul_real(col_factor);
                }
            }
        }

        Ok(Self {
            row_scale,
            col_scale,
        })
    }

    /// Builds row and column scales from a CSR matrix.
    ///
    /// This is the same simultaneous two-sided balancing iteration as
    /// [`compute_from_csc`](Self::compute_from_csc), but written against CSR
    /// traversal so callers can avoid format conversion when their solver path
    /// is already row-oriented.
    ///
    /// Args:
    ///   matrix: Sparse CSR matrix with shape `(nrows, ncols)`.
    ///   params: Iteration limit and balancing tolerances. These are
    ///     dimensionless because they describe relative scaling.
    ///
    /// Returns:
    ///   Row scales with length `nrows` and column scales with length `ncols`.
    pub fn compute_from_csr<I, ViewT>(
        matrix: SparseRowMatRef<'_, I, ViewT>,
        params: EquilibrationParams<T::Real>,
    ) -> Result<Self, EquilibrationError>
    where
        T: Copy,
        T::Real: Float,
        I: Index,
        ViewT: Conjugate<Canonical = T>,
    {
        let matrix = matrix.canonical();
        let nrows = matrix.nrows().unbound();
        let ncols = matrix.ncols().unbound();
        let row_ptr = matrix.row_ptr();
        let col_idx = matrix.col_idx();
        // Just like the CSC path, the iteration operates on scratch values so
        // callers can reuse the original matrix for an unscaled solve or for
        // constructing a separately scaled copy later.
        let mut working = matrix.val().to_vec();
        let one = T::Real::one();
        let zero = T::Real::zero();

        let mut row_scale = vec![one; nrows];
        let mut col_scale = vec![one; ncols];
        let mut row_norm = vec![zero; nrows];
        let mut col_norm = vec![zero; ncols];
        let mut row_update = vec![one; nrows];
        let mut col_update = vec![one; ncols];

        validate_params(params)?;

        for _ in 0..params.max_iters {
            row_norm.fill(zero);
            col_norm.fill(zero);

            for row in 0..nrows {
                let mut max_in_row = zero;
                for idx in row_ptr[row].zx()..row_ptr[row + 1].zx() {
                    let col = col_idx[idx].zx();
                    let mag = working[idx].abs();
                    if mag > max_in_row {
                        max_in_row = mag;
                    }
                    if mag > col_norm[col] {
                        col_norm[col] = mag;
                    }
                }
                row_norm[row] = max_in_row;
            }

            validate_nonzero_norms(&row_norm, &col_norm)?;
            if max_deviation(&row_norm, &col_norm) <= params.tol {
                break;
            }

            compute_updates(
                &row_norm,
                &mut row_update,
                &mut row_scale,
                params.norm_floor,
                params.norm_ceil,
            );
            compute_updates(
                &col_norm,
                &mut col_update,
                &mut col_scale,
                params.norm_floor,
                params.norm_ceil,
            );

            // Update the working values with the new left/right factors so the
            // next pass measures the newly equilibrated operator.
            for row in 0..nrows {
                let row_factor = row_update[row];
                for idx in row_ptr[row].zx()..row_ptr[row + 1].zx() {
                    let col = col_idx[idx].zx();
                    working[idx] = working[idx].mul_real(row_factor).mul_real(col_update[col]);
                }
            }
        }

        Ok(Self {
            row_scale,
            col_scale,
        })
    }

    /// Number of rows covered by the equilibration. This is the dimension of
    /// vectors that can be scaled on the left, such as residuals and
    /// right-hand sides.
    #[inline]
    #[must_use]
    pub fn nrows(&self) -> usize {
        self.row_scale.len()
    }

    /// Number of columns covered by the equilibration. This is the dimension of
    /// vectors that live in the unknown space, such as initial guesses and
    /// recovered solutions.
    #[inline]
    #[must_use]
    pub fn ncols(&self) -> usize {
        self.col_scale.len()
    }

    /// Positive row scaling factors with length `nrows()`. Multiplying row `i`
    /// by `row_scale[i]` is equivalent to left-multiplying by `D_r`.
    #[inline]
    #[must_use]
    pub fn row_scale(&self) -> &[T::Real] {
        &self.row_scale
    }

    /// Positive column scaling factors with length `ncols()`. Multiplying
    /// column `j` by `col_scale[j]` is equivalent to right-multiplying by
    /// `D_c`.
    #[inline]
    #[must_use]
    pub fn col_scale(&self) -> &[T::Real] {
        &self.col_scale
    }

    /// Applies the stored row and column scaling to CSC values in place.
    ///
    /// This transforms the numeric values for a matrix with the supplied
    /// symbolic structure into the values of `D_r A D_c`.
    ///
    /// The sparsity pattern is unchanged. Only the stored numeric values are
    /// rescaled.
    ///
    /// Args:
    ///   symbolic: CSC symbolic structure with shape `(nrows(), ncols())`.
    ///   values: Numeric value array with length equal to the number of stored
    ///     CSC nonzeros. It is overwritten in place.
    pub fn scale_csc_values_in_place<I>(
        &self,
        symbolic: SymbolicSparseColMatRef<'_, I>,
        values: &mut [T],
    ) where
        T: Copy,
        I: Index,
    {
        let nrows = symbolic.nrows().unbound();
        let ncols = symbolic.ncols().unbound();
        assert_eq!(self.nrows(), nrows);
        assert_eq!(self.ncols(), ncols);
        assert_eq!(values.len(), symbolic.row_idx().len());

        let col_ptr = symbolic.col_ptr();
        let row_idx = symbolic.row_idx();

        for col in 0..ncols {
            let col_factor = &self.col_scale[col];
            for idx in col_ptr[col].zx()..col_ptr[col + 1].zx() {
                let row = row_idx[idx].zx();
                values[idx] = values[idx]
                    .mul_real(&self.row_scale[row])
                    .mul_real(col_factor);
            }
        }
    }

    /// Applies the stored row and column scaling to CSR values in place.
    ///
    /// Like [`scale_csc_values_in_place`](Self::scale_csc_values_in_place), this
    /// leaves the sparsity pattern alone and only rescales the numeric values.
    ///
    /// Args:
    ///   symbolic: CSR symbolic structure with shape `(nrows(), ncols())`.
    ///   values: Numeric value array with length equal to the number of stored
    ///     CSR nonzeros. It is overwritten in place.
    pub fn scale_csr_values_in_place<I>(
        &self,
        symbolic: SymbolicSparseRowMatRef<'_, I>,
        values: &mut [T],
    ) where
        T: Copy,
        I: Index,
    {
        let nrows = symbolic.nrows().unbound();
        let ncols = symbolic.ncols().unbound();
        assert_eq!(self.nrows(), nrows);
        assert_eq!(self.ncols(), ncols);
        assert_eq!(values.len(), symbolic.col_idx().len());

        let row_ptr = symbolic.row_ptr();
        let col_idx = symbolic.col_idx();

        for row in 0..nrows {
            let row_factor = &self.row_scale[row];
            for idx in row_ptr[row].zx()..row_ptr[row + 1].zx() {
                let col = col_idx[idx].zx();
                values[idx] = values[idx]
                    .mul_real(row_factor)
                    .mul_real(&self.col_scale[col]);
            }
        }
    }

    /// Applies the stored scaling to an owned CSC matrix in place.
    ///
    /// This is the convenient owned-matrix variant of
    /// [`scale_csc_values_in_place`](Self::scale_csc_values_in_place).
    ///
    /// Args:
    ///   matrix: Owned CSC matrix with shape `(nrows(), ncols())`. Its numeric
    ///     values are overwritten in place.
    pub fn scale_csc_matrix_in_place<I>(&self, matrix: &mut SparseColMat<I, T>)
    where
        T: Copy,
        I: Index,
    {
        let (symbolic, values) = matrix.parts_mut();
        self.scale_csc_values_in_place(symbolic, values);
    }

    /// Applies the stored scaling to an owned CSR matrix in place.
    ///
    /// This is the convenient owned-matrix variant of
    /// [`scale_csr_values_in_place`](Self::scale_csr_values_in_place).
    ///
    /// Args:
    ///   matrix: Owned CSR matrix with shape `(nrows(), ncols())`. Its numeric
    ///     values are overwritten in place.
    pub fn scale_csr_matrix_in_place<I>(&self, matrix: &mut SparseRowMat<I, T>)
    where
        T: Copy,
        I: Index,
    {
        let (symbolic, values) = matrix.parts_mut();
        self.scale_csr_values_in_place(symbolic, values);
    }

    /// Scales a right-hand side in place with `D_r`.
    ///
    /// If the original system is `A x = b`, the balanced system uses
    /// `b_eq = D_r b`.
    ///
    /// Intuitively, this rescales each equation by the same factor used on the
    /// corresponding row of the matrix.
    ///
    /// Args:
    ///   rhs: Right-hand side vector with length `nrows()`. It is overwritten
    ///     in place.
    pub fn scale_rhs_in_place(&self, rhs: &mut [T])
    where
        T: Copy,
    {
        assert_eq!(rhs.len(), self.nrows());
        for (value, scale) in rhs.iter_mut().zip(self.row_scale.iter()) {
            *value = value.mul_real(scale);
        }
    }

    /// Converts an initial guess for `x` into the scaled unknown `y`.
    ///
    /// Since `x = D_c y`, a guess in the original coordinates must be mapped to
    /// `y = D_c^{-1} x` before solving the balanced system.
    ///
    /// This is the easy step to miss when swapping between original and
    /// equilibrated systems: row scaling changes the equations, but column
    /// scaling changes the coordinates of the unknown itself.
    ///
    /// Args:
    ///   x: Initial guess vector with length `ncols()`. It is overwritten in
    ///     place with the scaled-coordinate guess `y`.
    pub fn scale_initial_guess_in_place(&self, x: &mut [T])
    where
        T: Copy,
        T::Real: Float,
    {
        assert_eq!(x.len(), self.ncols());
        for (value, &scale) in x.iter_mut().zip(self.col_scale.iter()) {
            *value = value.mul_real(scale.recip());
        }
    }

    /// Converts a solution of the balanced system back to the original variables.
    ///
    /// If `y` solves `D_r A D_c y = D_r b`, then the original solution is
    /// `x = D_c y`.
    ///
    /// This undoes the coordinate change introduced by
    /// [`scale_initial_guess_in_place`](Self::scale_initial_guess_in_place).
    ///
    /// Args:
    ///   y: Balanced-coordinate solution vector with length `ncols()`. It is
    ///     overwritten in place with the original-coordinate solution.
    pub fn unscale_solution_in_place(&self, y: &mut [T])
    where
        T: Copy,
    {
        assert_eq!(y.len(), self.ncols());
        for (value, scale) in y.iter_mut().zip(self.col_scale.iter()) {
            *value = value.mul_real(scale);
        }
    }
}

fn validate_params<R: Float>(params: EquilibrationParams<R>) -> Result<(), EquilibrationError> {
    if params.max_iters == 0 {
        return Err(EquilibrationError::InvalidParams {
            reason: "max_iters must be at least 1",
        });
    }
    if params.tol < R::zero() {
        return Err(EquilibrationError::InvalidParams {
            reason: "tol must be nonnegative",
        });
    }
    if params.norm_floor <= R::zero() {
        return Err(EquilibrationError::InvalidParams {
            reason: "norm_floor must be positive",
        });
    }
    if params.norm_ceil < params.norm_floor {
        return Err(EquilibrationError::InvalidParams {
            reason: "norm_ceil must be at least norm_floor",
        });
    }
    Ok(())
}

fn validate_nonzero_norms<R: Float>(
    row_norm: &[R],
    col_norm: &[R],
) -> Result<(), EquilibrationError> {
    // A zero row or column cannot be balanced to unit norm without an explicit
    // singular-handling policy, so we surface it immediately.
    for (index, &value) in row_norm.iter().enumerate() {
        if value <= R::zero() {
            return Err(EquilibrationError::ZeroRow { index });
        }
    }
    for (index, &value) in col_norm.iter().enumerate() {
        if value <= R::zero() {
            return Err(EquilibrationError::ZeroColumn { index });
        }
    }
    Ok(())
}

fn max_deviation<R: Float>(row_norm: &[R], col_norm: &[R]) -> R {
    let one = R::one();
    row_norm
        .iter()
        .chain(col_norm.iter())
        .map(|&value| (value - one).abs())
        .fold(R::zero(), |max_dev, dev| max_dev.max(dev))
}

fn compute_updates<R: Float>(
    norms: &[R],
    updates: &mut [R],
    scales: &mut [R],
    norm_floor: R,
    norm_ceil: R,
) {
    for ((&norm, update), scale) in norms.iter().zip(updates.iter_mut()).zip(scales.iter_mut()) {
        // The sqrt split is the key balancing heuristic: it applies half of the
        // correction on the left and half on the right, which keeps the
        // iteration stable and symmetric between rows and columns.
        let clamped = norm.max(norm_floor).min(norm_ceil);
        let value = clamped.sqrt().recip();
        *update = value;
        *scale = *scale * value;
    }
}

#[cfg(test)]
mod test {
    use super::{Equilibration, EquilibrationError, EquilibrationParams};
    use crate::sparse::compensated::norm2;
    use crate::sparse::matvec::SparseMatVec;
    use crate::sparse::{BiCGSTAB, BiCGSTABSolveError};
    use alloc::vec::Vec;
    use faer::Unbind;
    use faer::sparse::{SparseColMat, SparseRowMat, Triplet};
    use faer_traits::IndexCore;

    fn row_col_spread_csc(matrix: &SparseColMat<usize, f64>) -> (f64, f64) {
        let matrix = matrix.as_ref();
        let nrows = matrix.nrows().unbound();
        let ncols = matrix.ncols().unbound();
        let col_ptr = matrix.col_ptr();
        let row_idx = matrix.row_idx();
        let values = matrix.val();
        let mut row_norm = vec![0.0f64; nrows];
        let mut col_norm = vec![0.0f64; ncols];

        for col in 0..ncols {
            for idx in col_ptr[col].zx()..col_ptr[col + 1].zx() {
                let row = row_idx[idx].zx();
                let mag = values[idx].abs();
                row_norm[row] = row_norm[row].max(mag);
                col_norm[col] = col_norm[col].max(mag);
            }
        }

        let row_min = row_norm.iter().copied().fold(f64::INFINITY, f64::min);
        let row_max = row_norm.iter().copied().fold(0.0, f64::max);
        let col_min = col_norm.iter().copied().fold(f64::INFINITY, f64::min);
        let col_max = col_norm.iter().copied().fold(0.0, f64::max);
        (row_max / row_min, col_max / col_min)
    }

    #[test]
    fn rejects_zero_rows_and_columns() {
        let matrix =
            SparseColMat::<usize, f64>::try_new_from_triplets(2, 2, &[Triplet::new(0, 0, 1.0)])
                .unwrap();

        assert_eq!(
            Equilibration::<f64>::compute_from_csc(matrix.as_ref(), EquilibrationParams::default()),
            Err(EquilibrationError::ZeroRow { index: 1 })
        );
    }

    #[test]
    fn simultaneous_updates_collapse_to_same_scales_on_symmetric_input() {
        let matrix = SparseColMat::<usize, f64>::try_new_from_triplets(
            4,
            4,
            &[
                Triplet::new(0, 0, 1.0e-6),
                Triplet::new(0, 1, 2.0),
                Triplet::new(1, 0, 2.0),
                Triplet::new(1, 1, 3.0e2),
                Triplet::new(1, 2, -5.0),
                Triplet::new(2, 1, -5.0),
                Triplet::new(2, 2, 7.0e4),
                Triplet::new(2, 3, 0.25),
                Triplet::new(3, 2, 0.25),
                Triplet::new(3, 3, 2.0e-3),
            ],
        )
        .unwrap();

        let eq =
            Equilibration::<f64>::compute_from_csc(matrix.as_ref(), EquilibrationParams::default())
                .unwrap();
        for (&row, &col) in eq.row_scale().iter().zip(eq.col_scale().iter()) {
            assert!((row - col).abs() < 1.0e-12);
        }
    }

    #[test]
    fn reduces_row_and_column_norm_spread() {
        let mut matrix = SparseColMat::<usize, f64>::try_new_from_triplets(
            5,
            5,
            &[
                Triplet::new(0, 0, 1.0e-8),
                Triplet::new(0, 2, 1.0),
                Triplet::new(1, 1, 1.0e4),
                Triplet::new(1, 3, -2.0),
                Triplet::new(2, 0, 5.0e2),
                Triplet::new(2, 2, 3.0e-2),
                Triplet::new(3, 1, -7.0e-4),
                Triplet::new(3, 4, 6.0e3),
                Triplet::new(4, 3, 9.0e-6),
                Triplet::new(4, 4, 2.0),
            ],
        )
        .unwrap();

        let before = row_col_spread_csc(&matrix);
        let eq =
            Equilibration::<f64>::compute_from_csc(matrix.as_ref(), EquilibrationParams::default())
                .unwrap();
        eq.scale_csc_matrix_in_place(&mut matrix);
        let after = row_col_spread_csc(&matrix);

        assert!(after.0 < before.0);
        assert!(after.1 < before.1);
        assert!(after.0 < before.0.sqrt());
        assert!(after.1 < before.1.sqrt());
    }

    #[test]
    fn scaled_bicgstab_solve_recovers_original_solution() {
        let n = 10usize;
        let row_true: Vec<f64> = (0..n).map(|i| 10.0f64.powi(i as i32 - 5)).collect();
        let col_true: Vec<f64> = (0..n).map(|i| 10.0f64.powi(5 - i as i32)).collect();
        let mut triplets = Vec::with_capacity(3 * n - 2);

        for row in 0..n {
            let row_inv = row_true[row].recip();
            for (col, base) in [
                (row.saturating_sub(1), if row > 0 { -0.2 } else { 0.0 }),
                (row, 2.5),
                (row + 1, if row + 1 < n { -0.15 } else { 0.0 }),
            ] {
                if base == 0.0 || col >= n {
                    continue;
                }
                let value = row_inv * base * col_true[col].recip();
                triplets.push(Triplet::new(row, col, value));
            }
        }

        let a = SparseRowMat::<usize, f64>::try_new_from_triplets(n, n, &triplets).unwrap();
        let eq = Equilibration::<f64>::compute_from_csr(a.as_ref(), EquilibrationParams::default())
            .unwrap();
        let x_true: Vec<f64> = (0..n)
            .map(|i| if i % 2 == 0 { 1.0 } else { -1.0 })
            .collect();
        let b = {
            let mut out = vec![0.0; n];
            a.as_ref().apply_compensated(&mut out, &x_true);
            out
        };

        let mut a_eq = a.clone();
        eq.scale_csr_matrix_in_place(&mut a_eq);
        let mut b_eq = b.clone();
        eq.scale_rhs_in_place(&mut b_eq);
        let mut x0_eq = vec![0.0; n];
        eq.scale_initial_guess_in_place(&mut x0_eq);

        let scaled = BiCGSTAB::solve(a_eq.as_ref(), &x0_eq, &b_eq, 1.0e-10, 200).unwrap();
        let mut x = scaled.x().iter().copied().collect::<Vec<_>>();
        eq.unscale_solution_in_place(&mut x);

        let mut diff = x.clone();
        for (dst, &truth) in diff.iter_mut().zip(x_true.iter()) {
            *dst -= truth;
        }

        let unscaled = BiCGSTAB::solve(a.as_ref(), &[0.0; 10], &b, 1.0e-10, 200);

        assert!(norm2::<f64>(&diff) < 1.0e-7);
        match unscaled {
            Ok(unscaled) => assert!(scaled.iteration_count() <= unscaled.iteration_count()),
            Err(BiCGSTABSolveError::NoConvergence(unscaled)) => {
                assert!(unscaled.err() >= 1.0e-10)
            }
            Err(BiCGSTABSolveError::InvalidInput(err)) => panic!("unexpected invalid input: {err}"),
        }
    }
}
