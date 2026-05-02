//! Self-adjoint and general eigendecomposition front-ends.
//!
//! The decomposition layer distinguishes between:
//!
//! - self-adjoint eigendecomposition, where the stronger structure justifies a
//!   dedicated API surface
//! - general eigendecomposition, where eigenvectors need not be orthogonal and
//!   the wrapper mostly provides deterministic ordering and residual
//!   diagnostics
//!
//! Dense full decompositions are supported for both cases. Sparse / matrix-free
//! wrappers are partial dominant-component solvers.
//!
//! # Two Intuitions
//!
//! 1. **Spectral-analysis view.** Eigendecomposition exposes the dominant modes
//!    of a linear operator.
//! 2. **Wrapper-policy view.** This module is also where the crate decides how
//!    to order eigenpairs, how to truncate dense factorizations, and how to
//!    report residual quality back to callers.
//!
//! # Glossary
//!
//! - **Self-adjoint:** Hermitian/symmetric matrix with orthogonal eigenvectors.
//! - **Ritz pair:** Approximate eigenpair returned by a Krylov method.
//! - **Generalized eigenpair:** Pair `(alpha, beta)` representing
//!   `lambda = alpha / beta`.
//!
//! # Mathematical Formulation
//!
//! The dense wrappers solve `A v = lambda v`; the generalized wrapper solves
//! `A v = lambda B v`; and the sparse wrappers return dominant Ritz pairs with
//! explicit residual diagnostics.
//!
//! # Implementation Notes
//!
//! - Dense outputs are ordered by descending magnitude to match the rest of the
//!   decomposition layer.
//! - Sparse diagnostics are recomputed explicitly on the returned vectors.
//! - Generalized eigen support returns right eigenvectors only.

use super::{
    DecompError, DecompInfo, DenseDecompParams, PartialEigen, PartialGeneralizedEigen,
    SparseDecompParams, normalized_start_vector, orthogonality_error, partial_eigen_params,
    permute_col, permute_mat_cols, sorted_order_descending_by_abs,
};
use crate::sparse::col::{col_slice, col_slice_mut, zero_col};
use crate::sparse::compensated::{CompensatedField, norm2};
use alloc::vec::Vec;
use faer::complex::Complex;
use faer::dyn_stack::{MemBuffer, MemStack, StackReq};
use faer::get_global_parallelism;
use faer::matrix_free::LinOp;
use faer::matrix_free::eigen::{
    partial_eigen, partial_eigen_scratch, partial_self_adjoint_eigen,
    partial_self_adjoint_eigen_scratch,
};
use faer::{Col, Mat, MatRef, Side, Unbind};
use faer_traits::ComplexField;
use faer_traits::ext::ComplexFieldExt;
use faer_traits::math_utils::zero;
use num_traits::{Float, Zero};

fn dense_full_self_adjoint_eigen<T>(a: MatRef<'_, T>) -> Result<PartialEigen<T>, DecompError>
where
    T: CompensatedField,
    T::Real: Float,
{
    if a.nrows() != a.ncols() {
        return Err(DecompError::DimensionMismatch {
            which: "matrix.ncols",
            expected: a.nrows(),
            actual: a.ncols(),
        });
    }

    let full = a.self_adjoint_eigen(Side::Lower)?;
    let vectors_ref = full.U();
    let values_ref = full.S().column_vector();
    let values = Col::from_fn(values_ref.nrows(), |i| values_ref[i]);
    let order = sorted_order_descending_by_abs(values.as_ref());
    let values = permute_col(values.as_ref(), &order);
    let vectors = permute_mat_cols(vectors_ref, &order);
    let info = eigen_info::<T, _>(
        &a,
        &values,
        &vectors,
        values.nrows(),
        values.nrows(),
        &mut MemBuffer::new(StackReq::EMPTY),
    );

    Ok(PartialEigen {
        values,
        vectors,
        info,
    })
}

fn dense_full_eigen<T>(a: MatRef<'_, T>) -> Result<PartialEigen<Complex<T::Real>>, DecompError>
where
    T: CompensatedField,
    T::Real: Float,
{
    if a.nrows() != a.ncols() {
        return Err(DecompError::DimensionMismatch {
            which: "matrix.ncols",
            expected: a.nrows(),
            actual: a.ncols(),
        });
    }

    let full = a.eigen()?;
    let vectors_ref = full.U();
    let values_ref = full.S().column_vector();
    let values = Col::from_fn(values_ref.nrows(), |i| values_ref[i]);
    // The public decomposition layer always exposes descending-magnitude
    // ordering, even though the backend's native ordering is backend-specific.
    let order = sorted_order_descending_by_abs(values.as_ref());
    let values = permute_col(values.as_ref(), &order);
    let vectors = permute_mat_cols(vectors_ref, &order);
    let info = dense_general_eigen_info(a, &values, &vectors, values.nrows(), values.nrows());

    Ok(PartialEigen {
        values,
        vectors,
        info,
    })
}

fn dense_full_generalized_eigen<T>(
    a: MatRef<'_, T>,
    b: MatRef<'_, T>,
) -> Result<PartialGeneralizedEigen<Complex<T::Real>>, DecompError>
where
    T: CompensatedField,
    T::Real: Float,
{
    if a.nrows() != a.ncols() {
        return Err(DecompError::DimensionMismatch {
            which: "a.ncols",
            expected: a.nrows(),
            actual: a.ncols(),
        });
    }
    if b.nrows() != b.ncols() {
        return Err(DecompError::DimensionMismatch {
            which: "b.ncols",
            expected: b.nrows(),
            actual: b.ncols(),
        });
    }
    if a.nrows() != b.nrows() {
        return Err(DecompError::DimensionMismatch {
            which: "b.nrows",
            expected: a.nrows(),
            actual: b.nrows(),
        });
    }

    let full = a.generalized_eigen(b)?;
    let alpha_ref = full.S_a().column_vector();
    let beta_ref = full.S_b().column_vector();
    let vectors_ref = full.U();
    let alpha = Col::from_fn(alpha_ref.nrows(), |i| alpha_ref[i]);
    let beta = Col::from_fn(beta_ref.nrows(), |i| beta_ref[i]);
    // Order generalized eigenpairs by the magnitude of the implied finite
    // eigenvalue `alpha / beta`, treating `beta = 0` as an infinite mode.
    let order = sorted_generalized_order_descending_by_abs(alpha.as_ref(), beta.as_ref());
    let alpha = permute_col(alpha.as_ref(), &order);
    let beta = permute_col(beta.as_ref(), &order);
    let vectors = permute_mat_cols(vectors_ref, &order);
    let info =
        dense_generalized_eigen_info(a, b, &alpha, &beta, &vectors, alpha.nrows(), alpha.nrows());

    Ok(PartialGeneralizedEigen {
        alpha,
        beta,
        vectors,
        info,
    })
}

fn complexify_scalar<T: CompensatedField>(value: T) -> Complex<T::Real>
where
    T::Real: Float,
{
    Complex::new(value.real(), value.imag())
}

fn dense_apply_complex<T>(
    a: MatRef<'_, T>,
    rhs: MatRef<'_, Complex<T::Real>>,
) -> Mat<Complex<T::Real>>
where
    T: CompensatedField,
    T::Real: Float,
{
    // `faer`'s dense general eigen returns complex Ritz data even for real
    // input matrices. The residual checks therefore need a small local dense
    // multiply that promotes the original matrix coefficients into the complex
    // field of the returned eigenvectors.
    Mat::from_fn(a.nrows(), rhs.ncols(), |i, j| {
        let row = i.unbound();
        let col = j.unbound();
        let mut acc = Complex::<T::Real>::new(<T::Real as Zero>::zero(), <T::Real as Zero>::zero());
        for k in 0..a.ncols() {
            acc += complexify_scalar(a[(row, k)]) * rhs[(k, col)];
        }
        acc
    })
}

fn dense_general_eigen_info<T>(
    a: MatRef<'_, T>,
    values: &Col<Complex<T::Real>>,
    vectors: &Mat<Complex<T::Real>>,
    n_requested: usize,
    n_converged: usize,
) -> DecompInfo<T::Real>
where
    T: CompensatedField,
    T::Real: Float,
{
    let mut max_residual_norm = <T::Real as Zero>::zero();
    for j in 0..values.nrows() {
        let rhs = Mat::from_fn(a.ncols(), 1, |i, _| vectors[(i.unbound(), j)]);
        let mut residual = dense_apply_complex(a, rhs.as_ref());
        for i in 0..a.nrows() {
            residual[(i, 0)] -= values[j] * vectors[(i, j)];
        }
        let residual_norm = norm2(residual.col(0).try_as_col_major().unwrap().as_slice());
        if residual_norm > max_residual_norm {
            max_residual_norm = residual_norm;
        }
    }

    DecompInfo {
        n_requested,
        n_converged,
        max_residual_norm,
        max_orthogonality_error: orthogonality_error(vectors.as_ref()),
    }
}

fn dense_generalized_eigen_info<T>(
    a: MatRef<'_, T>,
    b: MatRef<'_, T>,
    alpha: &Col<Complex<T::Real>>,
    beta: &Col<Complex<T::Real>>,
    vectors: &Mat<Complex<T::Real>>,
    n_requested: usize,
    n_converged: usize,
) -> DecompInfo<T::Real>
where
    T: CompensatedField,
    T::Real: Float,
{
    let mut max_residual_norm = <T::Real as Zero>::zero();
    for j in 0..alpha.nrows() {
        let rhs = Mat::from_fn(a.ncols(), 1, |i, _| vectors[(i.unbound(), j)]);
        let av = dense_apply_complex(a, rhs.as_ref());
        let bv = dense_apply_complex(b, rhs.as_ref());
        let residual = Mat::from_fn(a.nrows(), 1, |i, _| {
            beta[j] * av[(i.unbound(), 0)] - alpha[j] * bv[(i.unbound(), 0)]
        });
        let residual_norm = norm2(residual.col(0).try_as_col_major().unwrap().as_slice());
        if residual_norm > max_residual_norm {
            max_residual_norm = residual_norm;
        }
    }

    DecompInfo {
        n_requested,
        n_converged,
        max_residual_norm,
        max_orthogonality_error: orthogonality_error(vectors.as_ref()),
    }
}

fn truncate_general_eigen<T>(
    a: MatRef<'_, T>,
    eig: PartialEigen<Complex<T::Real>>,
    n_requested: usize,
) -> PartialEigen<Complex<T::Real>>
where
    T: CompensatedField,
    T::Real: Float,
{
    // The dense general path computes the full factorization and
    // truncates afterward. Recompute diagnostics on the truncated window rather
    // than carrying the full-spectrum maxima forward.
    let values = Col::from_fn(n_requested, |i| eig.values[i]);
    let vectors = Mat::from_fn(a.nrows(), n_requested, |i, j| {
        eig.vectors[(i.unbound(), j.unbound())]
    });
    let info = dense_general_eigen_info(a, &values, &vectors, n_requested, n_requested);
    PartialEigen {
        values,
        vectors,
        info,
    }
}

fn generalized_eigenvalue_abs<R: Float>(alpha: Complex<R>, beta: Complex<R>) -> R {
    if beta.re == R::zero() && beta.im == R::zero() {
        R::infinity()
    } else {
        let value = alpha / beta;
        (value.re * value.re + value.im * value.im).sqrt()
    }
}

fn sorted_generalized_order_descending_by_abs<R: Float>(
    alpha: faer::ColRef<'_, Complex<R>>,
    beta: faer::ColRef<'_, Complex<R>>,
) -> Vec<usize> {
    let mut order: Vec<_> = (0..alpha.nrows()).collect();
    order.sort_by(|&lhs, &rhs| {
        let lhs = generalized_eigenvalue_abs(alpha[lhs], beta[lhs]);
        let rhs = generalized_eigenvalue_abs(alpha[rhs], beta[rhs]);
        rhs.partial_cmp(&lhs).unwrap_or(core::cmp::Ordering::Equal)
    });
    order
}

fn validated_sparse_target<T, A>(
    op: &A,
    params: &SparseDecompParams<T>,
) -> Result<usize, DecompError>
where
    T: CompensatedField,
    T::Real: Float,
    A: LinOp<T>,
{
    if op.nrows() != op.ncols() {
        return Err(DecompError::DimensionMismatch {
            which: "operator.ncols",
            expected: op.nrows(),
            actual: op.ncols(),
        });
    }

    // The same restarted-window constraint that applies to sparse partial SVD
    // also applies here. Reject incompatible requests before calling into the
    // backend.
    let max_requested = if op.nrows() > 64 {
        (op.nrows() - 1) / 2
    } else {
        0
    };
    if params.n_components == 0 || params.n_components > max_requested {
        return Err(DecompError::InvalidTarget {
            requested: params.n_components,
            max: max_requested,
        });
    }

    Ok(params.n_components)
}

fn resolved_partial_dims(
    n_requested: usize,
    dim: usize,
    min_dim: Option<usize>,
    max_dim: Option<usize>,
) -> (usize, usize) {
    let max_allowed = dim - 1;
    // Keep the Krylov window within the range expected by `faer`'s restarted
    // self-adjoint eigensolver, while still honoring explicit caller
    // overrides.
    let min_dim = min_dim.unwrap_or(32usize.max(n_requested)).min(max_allowed);
    let max_dim = max_dim
        .unwrap_or(64usize.max(2 * n_requested))
        .max(min_dim)
        .min(max_allowed);
    (min_dim, max_dim)
}

fn partial_self_adjoint_eigen_impl<T, A>(
    op: &A,
    n_requested: usize,
    tol: T::Real,
    min_dim: Option<usize>,
    max_dim: Option<usize>,
    max_restarts: usize,
    start_vector: Option<&Col<T>>,
    scratch: &mut MemBuffer,
) -> Result<PartialEigen<T>, DecompError>
where
    T: CompensatedField,
    T::Real: Float,
    A: LinOp<T>,
{
    let par = get_global_parallelism();
    let (min_dim, max_dim) = resolved_partial_dims(n_requested, op.nrows(), min_dim, max_dim);
    let start = normalized_start_vector(start_vector, op.nrows())?;
    let mut vectors = Mat::zeros(op.nrows(), n_requested);
    let mut values = vec![zero::<T>(); n_requested];
    let stack = MemStack::new(scratch);
    let info = partial_self_adjoint_eigen(
        vectors.as_mut(),
        &mut values,
        op,
        start.as_ref(),
        tol,
        par,
        stack,
        partial_eigen_params::<T>(Some(min_dim), Some(max_dim), max_restarts),
    );

    let n_converged = info.n_converged_eigen.min(n_requested);
    // Reorder the converged Ritz pairs into the public descending-magnitude
    // order used consistently throughout this module.
    let values = Col::from_fn(n_converged, |i| values[i]);
    let vectors = Mat::from_fn(op.nrows(), n_converged, |i, j| vectors[(i, j)]);
    let order = sorted_order_descending_by_abs(values.as_ref());
    let values = permute_col(values.as_ref(), &order);
    let vectors = permute_mat_cols(vectors.as_ref(), &order);
    let info = eigen_info(op, &values, &vectors, n_requested, n_converged, scratch);

    Ok(PartialEigen {
        values,
        vectors,
        info,
    })
}

fn apply_operator_to_complex_vector<T, A>(
    op: &A,
    rhs: &[Complex<T::Real>],
    scratch: &mut MemBuffer,
) -> Col<Complex<T::Real>>
where
    T: CompensatedField,
    T::Real: Float,
    A: LinOp<T>,
{
    let par = get_global_parallelism();
    if T::IS_REAL {
        let rhs_real = Mat::from_fn(op.ncols(), 1, |i, _| {
            T::from_real_imag(rhs[i.unbound()].re, <T::Real as Zero>::zero())
        });
        let rhs_imag = Mat::from_fn(op.ncols(), 1, |i, _| {
            T::from_real_imag(rhs[i.unbound()].im, <T::Real as Zero>::zero())
        });
        let mut out_real = Mat::zeros(op.nrows(), 1);
        let mut out_imag = Mat::zeros(op.nrows(), 1);
        let stack = MemStack::new(scratch);
        op.apply(out_real.as_mut(), rhs_real.as_ref(), par, stack);
        let stack = MemStack::new(scratch);
        op.apply(out_imag.as_mut(), rhs_imag.as_ref(), par, stack);

        return Col::from_fn(op.nrows(), |i| {
            Complex::new(
                out_real[(i.unbound(), 0)].real(),
                out_imag[(i.unbound(), 0)].real(),
            )
        });
    }

    // The matrix-free operator is still defined over `T`, so the complex
    // residual check routes each complex vector through `T::from_real_imag`
    // before applying the operator and lifting the result back into the
    // complex residual field.
    let rhs_t = Mat::from_fn(op.ncols(), 1, |i, _| {
        T::from_real_imag(rhs[i.unbound()].re, rhs[i.unbound()].im)
    });
    let mut out_t = Mat::zeros(op.nrows(), 1);
    let stack = MemStack::new(scratch);
    op.apply(out_t.as_mut(), rhs_t.as_ref(), par, stack);
    Col::from_fn(op.nrows(), |i| {
        let value = out_t[(i.unbound(), 0)];
        Complex::new(value.real(), value.imag())
    })
}

fn partial_eigen_impl<T, A>(
    op: &A,
    n_requested: usize,
    tol: T::Real,
    min_dim: Option<usize>,
    max_dim: Option<usize>,
    max_restarts: usize,
    start_vector: Option<&Col<T>>,
    scratch: &mut MemBuffer,
) -> Result<PartialEigen<Complex<T::Real>>, DecompError>
where
    T: CompensatedField,
    T::Real: Float,
    A: LinOp<T>,
{
    let par = get_global_parallelism();
    let (min_dim, max_dim) = resolved_partial_dims(n_requested, op.nrows(), min_dim, max_dim);
    let start = normalized_start_vector(start_vector, op.nrows())?;
    let mut vectors = Mat::zeros(op.nrows(), n_requested);
    let mut values =
        vec![
            Complex::<T::Real>::new(<T::Real as Zero>::zero(), <T::Real as Zero>::zero());
            n_requested
        ];
    let stack = MemStack::new(scratch);
    let info = partial_eigen(
        vectors.as_mut(),
        &mut values,
        op,
        start.as_ref(),
        tol,
        par,
        stack,
        partial_eigen_params::<T>(Some(min_dim), Some(max_dim), max_restarts),
    );

    let n_converged = info.n_converged_eigen.min(n_requested);
    // As in the self-adjoint path, expose a deterministic dominant-first
    // ordering instead of the backend's native Ritz ordering.
    let values = Col::from_fn(n_converged, |i| values[i]);
    let vectors = Mat::from_fn(op.nrows(), n_converged, |i, j| {
        vectors[(i.unbound(), j.unbound())]
    });
    let order = sorted_order_descending_by_abs(values.as_ref());
    let values = permute_col(values.as_ref(), &order);
    let vectors = permute_mat_cols(vectors.as_ref(), &order);
    let info = general_eigen_info(op, &values, &vectors, n_requested, n_converged, scratch);

    Ok(PartialEigen {
        values,
        vectors,
        info,
    })
}

fn eigen_info<T, A>(
    op: &A,
    values: &Col<T>,
    vectors: &Mat<T>,
    n_requested: usize,
    n_converged: usize,
    scratch: &mut MemBuffer,
) -> DecompInfo<T::Real>
where
    T: CompensatedField,
    T::Real: Float,
    A: LinOp<T>,
{
    let par = get_global_parallelism();
    let mut residual_vec = zero_col::<T>(op.nrows());
    let mut max_residual_norm = <T::Real as Zero>::zero();

    for j in 0..values.nrows() {
        // For self-adjoint eigenpairs, one residual relation is enough:
        // `A v - lambda v`.
        let stack = MemStack::new(scratch);
        op.apply(
            residual_vec.as_mut().as_mat_mut(),
            vectors.col(j).as_mat(),
            par,
            stack,
        );
        for (dst, &value) in col_slice_mut(&mut residual_vec)
            .iter_mut()
            .zip(vectors.col(j).try_as_col_major().unwrap().as_slice())
        {
            *dst -= values[j] * value;
        }
        let residual = norm2(col_slice(&residual_vec));
        if residual > max_residual_norm {
            max_residual_norm = residual;
        }
    }

    DecompInfo {
        n_requested,
        n_converged,
        max_residual_norm,
        max_orthogonality_error: orthogonality_error(vectors.as_ref()),
    }
}

fn general_eigen_info<T, A>(
    op: &A,
    values: &Col<Complex<T::Real>>,
    vectors: &Mat<Complex<T::Real>>,
    n_requested: usize,
    n_converged: usize,
    scratch: &mut MemBuffer,
) -> DecompInfo<T::Real>
where
    T: CompensatedField,
    T::Real: Float,
    A: LinOp<T>,
{
    let mut max_residual_norm = <T::Real as Zero>::zero();

    for j in 0..values.nrows() {
        let vector = vectors.col(j).try_as_col_major().unwrap();
        let mut residual = apply_operator_to_complex_vector(op, vector.as_slice(), scratch);
        for (dst, &value) in col_slice_mut(&mut residual)
            .iter_mut()
            .zip(vector.as_slice())
        {
            *dst -= values[j] * value;
        }
        let residual_norm = norm2(col_slice(&residual));
        if residual_norm > max_residual_norm {
            max_residual_norm = residual_norm;
        }
    }

    DecompInfo {
        n_requested,
        n_converged,
        max_residual_norm,
        max_orthogonality_error: orthogonality_error(vectors.as_ref()),
    }
}

fn truncate_eigen<T, A>(op: &A, eig: PartialEigen<T>, n_requested: usize) -> PartialEigen<T>
where
    T: CompensatedField,
    T::Real: Float,
    A: LinOp<T>,
{
    let values = Col::from_fn(n_requested, |i| eig.values[i]);
    let vectors = Mat::from_fn(op.nrows(), n_requested, |i, j| eig.vectors[(i, j)]);
    let info = eigen_info(
        op,
        &values,
        &vectors,
        n_requested,
        n_requested,
        &mut MemBuffer::new(StackReq::EMPTY),
    );

    PartialEigen {
        values,
        vectors,
        info,
    }
}

/// Computes the scratch requirement for
/// [`sparse_self_adjoint_eigen_with_scratch`].
///
/// This is the expert entry point for callers that want to manage reusable
/// `MemBuffer` storage themselves.
pub fn sparse_self_adjoint_eigen_scratch_req<T, A>(
    op: &A,
    params: &SparseDecompParams<T>,
) -> Result<StackReq, DecompError>
where
    T: CompensatedField,
    T::Real: Float,
    A: LinOp<T>,
{
    let n_requested = validated_sparse_target(op, params)?;
    let (min_dim, max_dim) =
        resolved_partial_dims(n_requested, op.nrows(), params.min_dim, params.max_dim);
    Ok(partial_self_adjoint_eigen_scratch(
        op,
        n_requested,
        get_global_parallelism(),
        partial_eigen_params::<T>(Some(min_dim), Some(max_dim), params.max_restarts),
    ))
}

/// Computes a sparse / matrix-free partial self-adjoint eigendecomposition with
/// caller-provided scratch space.
///
/// This is the lowest-allocation path when the same operator size will be
/// decomposed repeatedly.
pub fn sparse_self_adjoint_eigen_with_scratch<T, A>(
    op: &A,
    params: &SparseDecompParams<T>,
    scratch: &mut MemBuffer,
) -> Result<PartialEigen<T>, DecompError>
where
    T: CompensatedField,
    T::Real: Float,
    A: LinOp<T>,
{
    let n_requested = validated_sparse_target(op, params)?;
    partial_self_adjoint_eigen_impl(
        op,
        n_requested,
        params.tol,
        params.min_dim,
        params.max_dim,
        params.max_restarts,
        params.start_vector.as_ref(),
        scratch,
    )
}

/// Computes a sparse / matrix-free partial self-adjoint eigendecomposition.
///
/// The sparse / matrix-free self-adjoint API is partial-only. Callers must
/// provide the number of dominant eigenpairs they want through
/// [`SparseDecompParams`].
pub fn sparse_self_adjoint_eigen<T, A>(
    op: &A,
    params: &SparseDecompParams<T>,
) -> Result<PartialEigen<T>, DecompError>
where
    T: CompensatedField,
    T::Real: Float,
    A: LinOp<T>,
{
    let req = sparse_self_adjoint_eigen_scratch_req(op, params)?;
    let mut scratch = MemBuffer::new(req);
    sparse_self_adjoint_eigen_with_scratch(op, params, &mut scratch)
}

/// Computes the scratch requirement for [`sparse_eigen_with_scratch`].
///
/// This mirrors [`sparse_self_adjoint_eigen_scratch_req`] for the general
/// nonsymmetric path so callers that repeat many decompositions can amortize
/// scratch allocation.
pub fn sparse_eigen_scratch_req<T, A>(
    op: &A,
    params: &SparseDecompParams<T>,
) -> Result<StackReq, DecompError>
where
    T: CompensatedField,
    T::Real: Float,
    A: LinOp<T>,
{
    let n_requested = validated_sparse_target(op, params)?;
    let (min_dim, max_dim) =
        resolved_partial_dims(n_requested, op.nrows(), params.min_dim, params.max_dim);
    Ok(partial_eigen_scratch(
        op,
        n_requested,
        get_global_parallelism(),
        partial_eigen_params::<T>(Some(min_dim), Some(max_dim), params.max_restarts),
    ))
}

/// Computes a sparse / matrix-free partial general eigendecomposition with
/// caller-provided scratch space.
///
/// This is the expert, lowest-allocation entry point for nonsymmetric matrix-
/// free problems. Returned Ritz pairs are reordered by descending eigenvalue
/// magnitude before being exposed.
pub fn sparse_eigen_with_scratch<T, A>(
    op: &A,
    params: &SparseDecompParams<T>,
    scratch: &mut MemBuffer,
) -> Result<PartialEigen<Complex<T::Real>>, DecompError>
where
    T: CompensatedField,
    T::Real: Float,
    A: LinOp<T>,
{
    let n_requested = validated_sparse_target(op, params)?;
    partial_eigen_impl(
        op,
        n_requested,
        params.tol,
        params.min_dim,
        params.max_dim,
        params.max_restarts,
        params.start_vector.as_ref(),
        scratch,
    )
}

/// Computes a sparse / matrix-free partial general eigendecomposition.
///
/// This is a dominant-component Arnoldi-style wrapper. It does not attempt a
/// full spectrum and is therefore the natural entry point for large sparse
/// operators when only a leading spectral window is needed.
pub fn sparse_eigen<T, A>(
    op: &A,
    params: &SparseDecompParams<T>,
) -> Result<PartialEigen<Complex<T::Real>>, DecompError>
where
    T: CompensatedField,
    T::Real: Float,
    A: LinOp<T>,
{
    let req = sparse_eigen_scratch_req(op, params)?;
    let mut scratch = MemBuffer::new(req);
    sparse_eigen_with_scratch(op, params, &mut scratch)
}

/// Computes a dense self-adjoint eigendecomposition.
///
/// `params.n_components = None` uses `faer`'s dense self-adjoint eigensolver.
/// `Some(k)` routes through the partial self-adjoint eigensolver when that is a
/// good fit for the matrix size, and otherwise falls back to a full dense
/// self-adjoint eigendecomposition followed by truncation.
pub fn dense_self_adjoint_eigen<T>(
    a: MatRef<'_, T>,
    params: &DenseDecompParams<T>,
) -> Result<PartialEigen<T>, DecompError>
where
    T: CompensatedField,
    T::Real: Float,
{
    if a.nrows() != a.ncols() {
        return Err(DecompError::DimensionMismatch {
            which: "matrix.ncols",
            expected: a.nrows(),
            actual: a.ncols(),
        });
    }

    match params.n_components {
        None => dense_full_self_adjoint_eigen(a),
        Some(0) => Ok(PartialEigen {
            values: Col::zeros(0),
            vectors: Mat::zeros(a.nrows(), 0),
            info: DecompInfo {
                n_requested: 0,
                n_converged: 0,
                max_residual_norm: <T::Real as Zero>::zero(),
                max_orthogonality_error: <T::Real as Zero>::zero(),
            },
        }),
        Some(k) if k >= a.nrows() => dense_full_self_adjoint_eigen(a),
        Some(k) if a.nrows() > 64 && 2 * k < a.nrows() => {
            let req = partial_self_adjoint_eigen_scratch(&a, k, get_global_parallelism(), {
                let (min_dim, max_dim) =
                    resolved_partial_dims(k, a.nrows(), params.min_dim, params.max_dim);
                partial_eigen_params::<T>(Some(min_dim), Some(max_dim), params.max_restarts)
            });
            let mut scratch = MemBuffer::new(req);
            partial_self_adjoint_eigen_impl(
                &a,
                k,
                params.tol,
                params.min_dim,
                params.max_dim,
                params.max_restarts,
                params.start_vector.as_ref(),
                &mut scratch,
            )
        }
        Some(k) => dense_full_self_adjoint_eigen(a).map(|eig| truncate_eigen(&a, eig, k)),
    }
}

/// Computes a dense general eigendecomposition.
///
/// This wrapper always computes the full dense eigendecomposition
/// and truncates afterward when `params.n_components = Some(k)`.
///
/// Returned eigenpairs are ordered by descending eigenvalue magnitude. The
/// residual and orthogonality diagnostics are recomputed on the exposed window
/// after any truncation step.
pub fn dense_eigen<T>(
    a: MatRef<'_, T>,
    params: &DenseDecompParams<T>,
) -> Result<PartialEigen<Complex<T::Real>>, DecompError>
where
    T: CompensatedField,
    T::Real: Float,
{
    if a.nrows() != a.ncols() {
        return Err(DecompError::DimensionMismatch {
            which: "matrix.ncols",
            expected: a.nrows(),
            actual: a.ncols(),
        });
    }

    match params.n_components {
        None => dense_full_eigen(a),
        Some(0) => Ok(PartialEigen {
            values: Col::zeros(0),
            vectors: Mat::zeros(a.nrows(), 0),
            info: DecompInfo {
                n_requested: 0,
                n_converged: 0,
                max_residual_norm: <T::Real as Zero>::zero(),
                max_orthogonality_error: <T::Real as Zero>::zero(),
            },
        }),
        Some(k) if k >= a.nrows() => dense_full_eigen(a),
        Some(k) => dense_full_eigen(a).map(|eig| truncate_general_eigen(a, eig, k)),
    }
}

/// Computes the dense eigenvalues of a square matrix and orders them by
/// descending magnitude.
///
/// This is the lightest-weight dense wrapper in the module. It forwards to the
/// backend's eigenvalue-only path and only adds dimension validation and the
/// crate-wide ordering convention.
pub fn dense_eigenvalues<T>(a: MatRef<'_, T>) -> Result<Col<Complex<T::Real>>, DecompError>
where
    T: ComplexField,
    T::Real: Float,
{
    if a.nrows() != a.ncols() {
        return Err(DecompError::DimensionMismatch {
            which: "matrix.ncols",
            expected: a.nrows(),
            actual: a.ncols(),
        });
    }

    let values_vec = a.eigenvalues()?;
    let values = Col::from_fn(a.nrows(), |i| values_vec[i.unbound()]);
    let order = sorted_order_descending_by_abs(values.as_ref());
    Ok(permute_col(values.as_ref(), &order))
}

/// Computes a dense generalized eigendecomposition.
///
/// The returned generalized eigenpairs are stored as `(alpha, beta)` rather
/// than pre-divided eigenvalues so callers can preserve infinite modes and
/// avoid unnecessary division when they need the raw pencil factors.
pub fn dense_generalized_eigen<T>(
    a: MatRef<'_, T>,
    b: MatRef<'_, T>,
) -> Result<PartialGeneralizedEigen<Complex<T::Real>>, DecompError>
where
    T: CompensatedField,
    T::Real: Float,
{
    dense_full_generalized_eigen(a, b)
}

#[cfg(test)]
mod tests {
    use super::{
        dense_eigen, dense_eigenvalues, dense_generalized_eigen, dense_self_adjoint_eigen,
        sparse_eigen, sparse_self_adjoint_eigen,
    };
    use crate::decomp::operator::CompensatedLinOp;
    use crate::decomp::{DenseDecompParams, SparseDecompParams};
    use alloc::vec::Vec;
    use faer::complex::Complex;
    use faer::dyn_stack::MemBuffer;
    use faer::matrix_free::LinOp;
    use faer::sparse::{SparseColMat, Triplet};
    use faer::{Col, Mat, Par, Unbind};
    use nalgebra::ComplexField;

    #[test]
    fn dense_self_adjoint_eigen_orders_by_magnitude() {
        let a = Mat::from_fn(3, 3, |i, j| match (i.unbound(), j.unbound()) {
            (0, 0) => -5.0,
            (1, 1) => 2.0,
            (2, 2) => 1.0,
            _ => 0.0,
        });

        let eig = dense_self_adjoint_eigen(a.as_ref(), &DenseDecompParams::<f64>::new()).unwrap();
        assert_eq!(eig.values.nrows(), 3);
        assert!(eig.values[0].abs() >= eig.values[1].abs());
        assert!(eig.values[1].abs() >= eig.values[2].abs());
    }

    #[test]
    fn sparse_self_adjoint_eigen_accepts_compensated_wrapper() {
        let matrix = SparseColMat::<usize, f64>::try_new_from_triplets(
            70,
            70,
            &(0..70)
                .map(|i| {
                    let value = if i % 2 == 0 {
                        (70 - i) as f64
                    } else {
                        -((70 - i) as f64)
                    };
                    Triplet::new(i, i, value)
                })
                .collect::<Vec<_>>(),
        )
        .unwrap();

        let eig = sparse_self_adjoint_eigen(
            &CompensatedLinOp::new(matrix.as_ref()),
            &SparseDecompParams::<f64>::new(4),
        )
        .unwrap();
        assert_eq!(eig.values.nrows(), 4);
        assert!(eig.values[0].abs() >= eig.values[1].abs());
        assert!(eig.info.n_converged <= 4);
    }

    #[test]
    fn dense_general_eigen_orders_nonsymmetric_spectrum_by_magnitude() {
        let a = Mat::from_fn(3, 3, |i, j| match (i.unbound(), j.unbound()) {
            (0, 0) => 3.0,
            (0, 1) => 1.0,
            (1, 1) => -2.0,
            (1, 2) => 2.0,
            (2, 2) => 0.5,
            _ => 0.0,
        });

        let eig = dense_eigen(a.as_ref(), &DenseDecompParams::<f64>::new()).unwrap();
        assert_eq!(eig.values.nrows(), 3);
        assert!((eig.values[0] - Complex::<f64>::new(3.0, 0.0)).abs() < 1e-10);
        assert!((eig.values[1] - Complex::<f64>::new(-2.0, 0.0)).abs() < 1e-10);
        assert!((eig.values[2] - Complex::<f64>::new(0.5, 0.0)).abs() < 1e-10);
        assert!(eig.info.max_residual_norm < 1e-10);
    }

    #[test]
    fn dense_eigenvalues_orders_by_descending_magnitude() {
        let a = Mat::from_fn(3, 3, |i, j| match (i.unbound(), j.unbound()) {
            (0, 0) => -1.0,
            (0, 1) => 4.0,
            (1, 1) => 2.0,
            (2, 2) => -3.0,
            _ => 0.0,
        });

        let values: Col<Complex<f64>> = dense_eigenvalues(a.as_ref()).unwrap();
        assert_eq!(values.nrows(), 3);
        assert!((values[0].re + 3.0).abs() < 1e-10);
        assert!(values[0].im.abs() < 1e-10);
        assert!((values[1].re - 2.0).abs() < 1e-10);
        assert!(values[1].im.abs() < 1e-10);
        assert!((values[2].re + 1.0).abs() < 1e-10);
        assert!(values[2].im.abs() < 1e-10);
    }

    #[test]
    fn dense_generalized_eigen_orders_by_generalized_magnitude() {
        let a = Mat::from_fn(2, 2, |i, j| match (i.unbound(), j.unbound()) {
            (0, 0) => 2.0,
            (1, 1) => -6.0,
            _ => 0.0,
        });
        let b = Mat::from_fn(2, 2, |i, j| match (i.unbound(), j.unbound()) {
            (0, 0) => 1.0,
            (1, 1) => 2.0,
            _ => 0.0,
        });

        let gevd = dense_generalized_eigen(a.as_ref(), b.as_ref()).unwrap();
        assert_eq!(gevd.alpha.nrows(), 2);
        let lambda0: Complex<f64> = gevd.alpha[0] / gevd.beta[0];
        let lambda1: Complex<f64> = gevd.alpha[1] / gevd.beta[1];
        assert!((lambda0.re + 3.0).abs() < 1e-10);
        assert!(lambda0.im.abs() < 1e-10);
        assert!((lambda1.re - 2.0).abs() < 1e-10);
        assert!(lambda1.im.abs() < 1e-10);
        assert!(gevd.info.max_residual_norm < 1e-10);
    }

    #[test]
    fn sparse_general_eigen_accepts_compensated_wrapper() {
        let matrix = SparseColMat::<usize, f64>::try_new_from_triplets(
            70,
            70,
            &(0..70)
                .map(|i| {
                    let value = if i % 2 == 0 {
                        (70 - i) as f64
                    } else {
                        -((70 - i) as f64)
                    };
                    Triplet::new(i, i, value)
                })
                .collect::<Vec<_>>(),
        )
        .unwrap();

        let eig = sparse_eigen(
            &CompensatedLinOp::new(matrix.as_ref()),
            &SparseDecompParams::<f64>::new(4),
        )
        .unwrap();
        assert_eq!(eig.values.nrows(), 4);
        assert!(eig.values[0].abs() >= eig.values[1].abs());
        assert!(eig.values[1].abs() >= eig.values[2].abs());
        assert!(eig.values[2].abs() >= eig.values[3].abs());
        assert!(eig.info.n_converged <= 4);
        assert!(eig.values.iter().all(|value| value.im.abs() < 1e-10));
    }

    #[test]
    fn real_operator_complex_vector_application_preserves_imaginary_part() {
        let matrix = Mat::from_fn(2, 2, |row, col| match (row.unbound(), col.unbound()) {
            (0, 1) => -1.0,
            (1, 0) => 1.0,
            _ => 0.0,
        });
        let op = matrix.as_ref();
        let rhs = [
            Complex::<f64>::new(1.0, 0.0),
            Complex::<f64>::new(0.0, -1.0),
        ];
        let mut scratch = MemBuffer::new(op.apply_scratch(1, Par::Seq));

        let out: Col<Complex<f64>> =
            super::apply_operator_to_complex_vector(&op, &rhs, &mut scratch);

        assert!(out[0].re.abs() < 1.0e-12);
        assert!((out[0].im - 1.0).abs() < 1.0e-12);
        assert!((out[1].re - 1.0).abs() < 1.0e-12);
        assert!(out[1].im.abs() < 1.0e-12);
    }
}
