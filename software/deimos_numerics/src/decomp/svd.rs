//! Singular-value decomposition front-ends.
//!
//! The dense path uses `faer`'s dense SVD backend for full decompositions and
//! may use its partial matrix-free backend for dominant-component requests. The
//! sparse / matrix-free path is always partial and always requests a fixed
//! number of leading singular triplets.
//!
//! # Two Intuitions
//!
//! 1. **Range/nullspace view.** SVD finds the most important input and output
//!    directions of an operator and ranks them by gain.
//! 2. **Truncation-policy view.** The wrappers here also define how dense and
//!    sparse backends present their singular triplets to the rest of the crate:
//!    ordered, truncated, and accompanied by explicit diagnostics.
//!
//! # Glossary
//!
//! - **Singular triplet:** One left vector, singular value, and right vector.
//! - **Thin SVD:** Economy-size dense factorization.
//! - **Partial SVD:** Dominant-component solve that returns only the largest
//!   singular directions.
//!
//! # Mathematical Formulation
//!
//! The factorization is `A = U Sigma V^H`, with the returned singular values
//! ordered by descending magnitude.
//!
//! # Implementation Notes
//!
//! - Dense wrappers may compute a full thin SVD and then truncate.
//! - Sparse wrappers are always dominant-component Krylov solves.
//! - Wrapper-owned diagnostics recompute residual and orthogonality checks
//!   rather than trusting backend ordering alone.

use super::{
    DecompError, DecompInfo, DenseDecompParams, PartialSvd, SparseDecompParams,
    normalized_start_vector, orthogonality_error, partial_eigen_params, permute_col,
    permute_mat_cols, sorted_order_descending_by_abs,
};
use crate::sparse::col::{col_slice, col_slice_mut, zero_col};
use crate::sparse::compensated::{CompensatedField, norm2};
use faer::dyn_stack::{MemBuffer, MemStack, StackReq};
use faer::get_global_parallelism;
use faer::matrix_free::BiLinOp;
use faer::matrix_free::eigen::{partial_svd, partial_svd_scratch};
use faer::{Col, Mat, MatRef};
use faer_traits::math_utils::zero;
use num_traits::{Float, Zero};

fn dense_full_svd<T>(a: MatRef<'_, T>) -> Result<PartialSvd<T>, DecompError>
where
    T: CompensatedField,
    T::Real: Float,
{
    let full = a.thin_svd()?;
    let u_ref = full.U();
    let v_ref = full.V();
    let s_ref = full.S().column_vector();

    let s = Col::from_fn(s_ref.nrows(), |i| s_ref[i]);
    let order = sorted_order_descending_by_abs(s.as_ref());
    let u = permute_mat_cols(u_ref, &order);
    let v = permute_mat_cols(v_ref, &order);
    let s = permute_col(s.as_ref(), &order);
    let info = svd_info::<T, _>(
        &a,
        &u,
        &s,
        &v,
        s.nrows(),
        s.nrows(),
        &mut MemBuffer::new(StackReq::EMPTY),
    );

    Ok(PartialSvd { u, s, v, info })
}

fn validated_sparse_target<T, A>(
    op: &A,
    params: &SparseDecompParams<T>,
) -> Result<usize, DecompError>
where
    T: CompensatedField,
    T::Real: Float,
    A: BiLinOp<T>,
{
    let full_rank = op.nrows().min(op.ncols());
    // `faer`'s partial sparse/operator SVD backend is a restarted dominant
    // solver with an internal Krylov subspace size floor. Small ambient
    // dimensions therefore do not support arbitrary `k`; reject those requests
    // here instead of letting the backend panic on an invalid window size.
    let max_requested = if full_rank > 64 {
        (full_rank - 1) / 2
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
    full_rank: usize,
    min_dim: Option<usize>,
    max_dim: Option<usize>,
) -> (usize, usize) {
    let max_allowed = full_rank - 1;
    // Match the practical window shape expected by `faer`'s restarted partial
    // solver while still allowing callers to override it when they have a good
    // reason to do so.
    let min_dim = min_dim.unwrap_or(32usize.max(n_requested)).min(max_allowed);
    let max_dim = max_dim
        .unwrap_or(64usize.max(2 * n_requested))
        .max(min_dim)
        .min(max_allowed);
    (min_dim, max_dim)
}

fn partial_svd_impl<T, A>(
    op: &A,
    n_requested: usize,
    tol: T::Real,
    min_dim: Option<usize>,
    max_dim: Option<usize>,
    max_restarts: usize,
    start_vector: Option<&Col<T>>,
    scratch: &mut MemBuffer,
) -> Result<PartialSvd<T>, DecompError>
where
    T: CompensatedField,
    T::Real: Float,
    A: BiLinOp<T>,
{
    let par = get_global_parallelism();
    let (min_dim, max_dim) =
        resolved_partial_dims(n_requested, op.nrows().min(op.ncols()), min_dim, max_dim);
    let start = normalized_start_vector(start_vector, op.ncols())?;
    let mut u = Mat::zeros(op.nrows(), n_requested);
    let mut v = Mat::zeros(op.ncols(), n_requested);
    let mut s = vec![zero::<T>(); n_requested];
    let stack = MemStack::new(scratch);
    let info = partial_svd(
        u.as_mut(),
        v.as_mut(),
        &mut s,
        op,
        start.as_ref(),
        tol,
        par,
        stack,
        partial_eigen_params::<T>(Some(min_dim), Some(max_dim), max_restarts),
    );

    let n_converged = info.n_converged_eigen.min(n_requested);
    // `faer` returns the converged window in backend order. Normalize the
    // public contract here by truncating to the converged prefix and then
    // reordering by descending singular-value magnitude.
    let s = Col::from_fn(n_converged, |i| s[i]);
    let u = Mat::from_fn(op.nrows(), n_converged, |i, j| u[(i, j)]);
    let v = Mat::from_fn(op.ncols(), n_converged, |i, j| v[(i, j)]);
    let order = sorted_order_descending_by_abs(s.as_ref());
    let u = permute_mat_cols(u.as_ref(), &order);
    let v = permute_mat_cols(v.as_ref(), &order);
    let s = permute_col(s.as_ref(), &order);
    let info = svd_info(op, &u, &s, &v, n_requested, n_converged, scratch);

    Ok(PartialSvd { u, s, v, info })
}

fn svd_info<T, A>(
    op: &A,
    u: &Mat<T>,
    s: &Col<T>,
    v: &Mat<T>,
    n_requested: usize,
    n_converged: usize,
    scratch: &mut MemBuffer,
) -> DecompInfo<T::Real>
where
    T: CompensatedField,
    T::Real: Float,
    A: BiLinOp<T>,
{
    let par = get_global_parallelism();
    let mut av = zero_col::<T>(op.nrows());
    let mut ahu = zero_col::<T>(op.ncols());
    let mut max_residual_norm = T::Real::zero();

    for j in 0..s.nrows() {
        // Check both SVD residual relations so the reported residual reflects
        // the worst mismatch in the returned singular triplet.
        let stack = MemStack::new(scratch);
        op.apply(av.as_mut().as_mat_mut(), v.col(j).as_mat(), par, stack);
        for (dst, &u_value) in col_slice_mut(&mut av)
            .iter_mut()
            .zip(u.col(j).try_as_col_major().unwrap().as_slice())
        {
            *dst -= s[j] * u_value;
        }
        let residual = norm2(col_slice(&av));
        if residual > max_residual_norm {
            max_residual_norm = residual;
        }

        let stack = MemStack::new(scratch);
        op.adjoint_apply(ahu.as_mut().as_mat_mut(), u.col(j).as_mat(), par, stack);
        for (dst, &v_value) in col_slice_mut(&mut ahu)
            .iter_mut()
            .zip(v.col(j).try_as_col_major().unwrap().as_slice())
        {
            *dst -= s[j] * v_value;
        }
        let residual = norm2(col_slice(&ahu));
        if residual > max_residual_norm {
            max_residual_norm = residual;
        }
    }

    let mut max_orthogonality_error = orthogonality_error(u.as_ref());
    let v_error = orthogonality_error(v.as_ref());
    if v_error > max_orthogonality_error {
        max_orthogonality_error = v_error;
    }

    DecompInfo {
        n_requested,
        n_converged,
        max_residual_norm,
        max_orthogonality_error,
    }
}

fn truncate_svd<T, A>(op: &A, svd: PartialSvd<T>, n_requested: usize) -> PartialSvd<T>
where
    T: CompensatedField,
    T::Real: Float,
    A: BiLinOp<T>,
{
    let u = Mat::from_fn(op.nrows(), n_requested, |i, j| svd.u[(i, j)]);
    let s = Col::from_fn(n_requested, |i| svd.s[i]);
    let v = Mat::from_fn(op.ncols(), n_requested, |i, j| svd.v[(i, j)]);
    let info = svd_info(
        op,
        &u,
        &s,
        &v,
        n_requested,
        n_requested,
        &mut MemBuffer::new(StackReq::EMPTY),
    );

    PartialSvd { u, s, v, info }
}

/// Computes the scratch requirement for [`sparse_svd_with_scratch`].
///
/// This is the expert entry point for callers that want to reuse `MemBuffer`
/// storage across many partial SVD calls and avoid repeated allocation.
pub fn sparse_svd_scratch_req<T, A>(
    op: &A,
    params: &SparseDecompParams<T>,
) -> Result<StackReq, DecompError>
where
    T: CompensatedField,
    T::Real: Float,
    A: BiLinOp<T>,
{
    let n_requested = validated_sparse_target(op, params)?;
    let (min_dim, max_dim) = resolved_partial_dims(
        n_requested,
        op.nrows().min(op.ncols()),
        params.min_dim,
        params.max_dim,
    );
    Ok(partial_svd_scratch(
        op,
        n_requested,
        get_global_parallelism(),
        partial_eigen_params::<T>(Some(min_dim), Some(max_dim), params.max_restarts),
    ))
}

/// Computes a sparse / matrix-free partial SVD with caller-provided scratch
/// space.
///
/// Use this when the same operator shape will be decomposed repeatedly and the
/// caller wants to keep scratch allocation outside the hot path.
pub fn sparse_svd_with_scratch<T, A>(
    op: &A,
    params: &SparseDecompParams<T>,
    scratch: &mut MemBuffer,
) -> Result<PartialSvd<T>, DecompError>
where
    T: CompensatedField,
    T::Real: Float,
    A: BiLinOp<T>,
{
    let n_requested = validated_sparse_target(op, params)?;
    partial_svd_impl(
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

/// Computes a sparse / matrix-free partial SVD.
///
/// The sparse / matrix-free API is intentionally partial-only. The caller must
/// request a fixed number of dominant singular triplets through
/// [`SparseDecompParams`].
pub fn sparse_svd<T, A>(
    op: &A,
    params: &SparseDecompParams<T>,
) -> Result<PartialSvd<T>, DecompError>
where
    T: CompensatedField,
    T::Real: Float,
    A: BiLinOp<T>,
{
    let req = sparse_svd_scratch_req(op, params)?;
    let mut scratch = MemBuffer::new(req);
    sparse_svd_with_scratch(op, params, &mut scratch)
}

/// Computes a dense SVD.
///
/// `params.n_components = None` uses `faer`'s dense thin-SVD backend and
/// returns all singular components. `Some(k)` routes through the partial SVD
/// backend and returns the leading `k` singular triplets when that backend is
/// numerically and dimensionally appropriate. Otherwise the wrapper falls back
/// to the dense full SVD and truncates the result.
pub fn dense_svd<T>(
    a: MatRef<'_, T>,
    params: &DenseDecompParams<T>,
) -> Result<PartialSvd<T>, DecompError>
where
    T: CompensatedField,
    T::Real: Float,
{
    let full_rank = a.nrows().min(a.ncols());
    match params.n_components {
        None => dense_full_svd(a),
        Some(0) => Ok(PartialSvd {
            u: Mat::zeros(a.nrows(), 0),
            s: Col::zeros(0),
            v: Mat::zeros(a.ncols(), 0),
            info: DecompInfo {
                n_requested: 0,
                n_converged: 0,
                max_residual_norm: T::Real::zero(),
                max_orthogonality_error: T::Real::zero(),
            },
        }),
        Some(k) if k >= full_rank => dense_full_svd(a),
        Some(k) if full_rank > 64 && 2 * k < full_rank => {
            let req = partial_svd_scratch(&a, k, get_global_parallelism(), {
                let (min_dim, max_dim) =
                    resolved_partial_dims(k, full_rank, params.min_dim, params.max_dim);
                partial_eigen_params::<T>(Some(min_dim), Some(max_dim), params.max_restarts)
            });
            let mut scratch = MemBuffer::new(req);
            partial_svd_impl(
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
        Some(k) => dense_full_svd(a).map(|svd| truncate_svd(&a, svd, k)),
    }
}

#[cfg(test)]
mod tests {
    use super::{dense_svd, sparse_svd};
    use crate::decomp::operator::CompensatedBiLinOp;
    use crate::decomp::{DenseDecompParams, SparseDecompParams};
    use alloc::vec::Vec;
    use faer::sparse::{SparseColMat, Triplet};
    use faer::{Mat, Unbind};

    #[test]
    fn dense_svd_full_returns_descending_values() {
        let a = Mat::from_fn(3, 2, |i, j| match (i.unbound(), j.unbound()) {
            (0, 0) => 3.0,
            (1, 1) => 2.0,
            _ => 0.0,
        });

        let svd = dense_svd(a.as_ref(), &DenseDecompParams::<f64>::new()).unwrap();
        assert_eq!(svd.s.nrows(), 2);
        assert!(svd.s[0].abs() >= svd.s[1].abs());
        assert!(svd.info.fully_converged());
    }

    #[test]
    fn sparse_svd_accepts_compensated_wrapper() {
        let matrix = SparseColMat::<usize, f64>::try_new_from_triplets(
            70,
            70,
            &(0..70)
                .map(|i| Triplet::new(i, i, (70 - i) as f64))
                .collect::<Vec<_>>(),
        )
        .unwrap();

        let params = SparseDecompParams::<f64>::new(4);
        let svd = sparse_svd(&CompensatedBiLinOp::new(matrix.as_ref()), &params).unwrap();
        assert_eq!(svd.s.nrows(), 4);
        assert!(svd.s[0].abs() >= svd.s[1].abs());
        assert!(svd.info.n_converged <= 4);
    }
}
