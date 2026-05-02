//! Small dense helpers shared by the alloc-backed embedded estimators.
//!
//! # Glossary
//!
//! - **LLT solve:** Linear solve using a Cholesky factorization of a symmetric
//!   positive-definite matrix.

use crate::embedded::alloc::{Matrix, Vector};
use crate::embedded::error::EmbeddedError;
use crate::embedded::math::ensure_finite;
use faer::linalg::solvers::Solve;
use faer::{Col, ColMut, ColRef, Mat, Side, Unbind};
use faer_traits::ComplexField;
use num_traits::Float;

/// Returns the all-zero dense matrix with the requested shape.
pub(super) fn zero_matrix<T>(nrows: usize, ncols: usize) -> Matrix<T>
where
    T: Float,
{
    Mat::from_fn(nrows, ncols, |_, _| T::zero())
}

#[cfg(test)]
/// Returns the identity matrix of order `n`.
pub(super) fn identity_matrix<T>(n: usize) -> Matrix<T>
where
    T: Float,
{
    Mat::from_fn(
        n,
        n,
        |row, col| if row == col { T::one() } else { T::zero() },
    )
}

/// Returns the all-zero dense vector of length `n`.
pub(super) fn zero_vector<T>(n: usize) -> Vector<T>
where
    T: Float,
{
    Col::from_fn(n, |_| T::zero())
}

/// Copies a dense vector from a slice.
pub(super) fn vector_from_slice<T>(values: &[T]) -> Vector<T>
where
    T: Copy,
{
    Col::from_fn(values.len(), |i| values[i.unbound()])
}

/// Copies one dense vector into a single-column dense matrix.
pub(super) fn vector_as_column_matrix<T>(vector: &Vector<T>) -> Matrix<T>
where
    T: Copy,
{
    Mat::from_fn(vector.nrows(), 1, |row, _| vector[row])
}

/// Copies one single-column dense matrix into a dense vector.
pub(super) fn column_matrix_to_vector<T>(
    matrix: &Matrix<T>,
    which: &'static str,
) -> Result<Vector<T>, EmbeddedError>
where
    T: Copy,
{
    if matrix.ncols() != 1 {
        return Err(EmbeddedError::DimensionMismatch {
            which,
            expected_rows: matrix.nrows(),
            expected_cols: 1,
            actual_rows: matrix.nrows(),
            actual_cols: matrix.ncols(),
        });
    }

    Ok(Col::from_fn(matrix.nrows(), |row| {
        matrix[(row.unbound(), 0)]
    }))
}

/// Packs a set of equal-length vectors into one column-major matrix.
pub(super) fn vectors_as_columns<T>(
    vectors: &[Vector<T>],
    which: &'static str,
) -> Result<Matrix<T>, EmbeddedError>
where
    T: Float,
{
    if vectors.is_empty() {
        return Ok(zero_matrix(0, 0));
    }

    let nrows = vectors[0].nrows();
    for vector in &vectors[1..] {
        if vector.nrows() != nrows {
            return Err(EmbeddedError::LengthMismatch {
                which,
                expected: nrows,
                actual: vector.nrows(),
            });
        }
    }

    Ok(Mat::from_fn(nrows, vectors.len(), |row, col| {
        vectors[col.unbound()][row]
    }))
}

/// Returns an immutable slice view of one dense vector.
pub(super) fn vec_as_slice<T>(vector: &Vector<T>) -> &[T] {
    match vector.try_as_col_major() {
        Some(col) => col.as_slice(),
        None => unreachable!("faer Col storage is always contiguous"),
    }
}

/// Returns a mutable slice view of one dense vector.
pub(super) fn vec_as_slice_mut<T>(vector: &mut Vector<T>) -> &mut [T] {
    match vector.try_as_col_major_mut() {
        Some(col) => col.as_slice_mut(),
        None => unreachable!("faer Col storage is always contiguous"),
    }
}

/// Returns an immutable slice view of one dense matrix column.
pub(super) fn col_as_slice<T>(column: ColRef<'_, T>) -> &[T] {
    match column.try_as_col_major() {
        Some(col) => col.as_slice(),
        None => unreachable!("faer Col storage is always contiguous"),
    }
}

/// Returns a mutable slice view of one dense matrix column.
pub(super) fn col_as_slice_mut<T>(column: ColMut<'_, T>) -> &mut [T] {
    match column.try_as_col_major_mut() {
        Some(col) => col.as_slice_mut(),
        None => unreachable!("faer Col storage is always contiguous"),
    }
}

/// Returns the scalar-scaled matrix `alpha * matrix`.
pub(super) fn scale_matrix<T>(matrix: &Matrix<T>, alpha: T) -> Matrix<T>
where
    T: Float,
{
    Mat::from_fn(matrix.nrows(), matrix.ncols(), |row, col| {
        alpha * matrix[(row, col)]
    })
}

/// Returns the matrix product `lhs * rhs`.
pub(super) fn mat_mul<T>(lhs: &Matrix<T>, rhs: &Matrix<T>) -> Result<Matrix<T>, EmbeddedError>
where
    T: Float,
{
    if lhs.ncols() != rhs.nrows() {
        return Err(EmbeddedError::DimensionMismatch {
            which: "embedded.alloc.matrix.mul",
            expected_rows: lhs.ncols(),
            expected_cols: 1,
            actual_rows: rhs.nrows(),
            actual_cols: 1,
        });
    }

    Ok(Mat::from_fn(lhs.nrows(), rhs.ncols(), |row, col| {
        let mut acc = T::zero();
        for idx in 0..lhs.ncols() {
            acc = acc + lhs[(row, idx)] * rhs[(idx, col)];
        }
        acc
    }))
}

/// Returns the dense vector product `matrix * vector`.
pub(super) fn mat_mul_vec<T>(
    matrix: &Matrix<T>,
    vector: &Vector<T>,
) -> Result<Vector<T>, EmbeddedError>
where
    T: Float,
{
    if matrix.ncols() != vector.nrows() {
        return Err(EmbeddedError::LengthMismatch {
            which: "embedded.alloc.matrix.mul_vec",
            expected: matrix.ncols(),
            actual: vector.nrows(),
        });
    }

    Ok(Col::from_fn(matrix.nrows(), |row| {
        let mut acc = T::zero();
        for col in 0..matrix.ncols() {
            acc = acc + matrix[(row.unbound(), col)] * vector[col];
        }
        acc
    }))
}

/// Returns the Euclidean norm of one dense vector.
pub(super) fn vec_norm<T>(vector: &Vector<T>) -> T
where
    T: Float,
{
    let mut sum = T::zero();
    for idx in 0..vector.nrows() {
        sum = sum + vector[idx] * vector[idx];
    }
    sum.sqrt()
}

/// Returns `lhs + rhs` for dense vectors.
pub(super) fn vec_add<T>(lhs: &Vector<T>, rhs: &Vector<T>) -> Result<Vector<T>, EmbeddedError>
where
    T: Float,
{
    if lhs.nrows() != rhs.nrows() {
        return Err(EmbeddedError::LengthMismatch {
            which: "embedded.alloc.vec_add",
            expected: lhs.nrows(),
            actual: rhs.nrows(),
        });
    }

    Ok(Col::from_fn(lhs.nrows(), |i| {
        lhs[i.unbound()] + rhs[i.unbound()]
    }))
}

/// Returns `lhs - rhs` for dense vectors.
pub(super) fn vec_sub<T>(lhs: &Vector<T>, rhs: &Vector<T>) -> Result<Vector<T>, EmbeddedError>
where
    T: Float,
{
    if lhs.nrows() != rhs.nrows() {
        return Err(EmbeddedError::LengthMismatch {
            which: "embedded.alloc.vec_sub",
            expected: lhs.nrows(),
            actual: rhs.nrows(),
        });
    }

    Ok(Col::from_fn(lhs.nrows(), |i| {
        lhs[i.unbound()] - rhs[i.unbound()]
    }))
}

/// Solves `matrix * X = rhs` using faer's dense `LL^T` factorization.
pub(super) fn llt_solve<T>(
    matrix: &Matrix<T>,
    rhs: &Matrix<T>,
    which: &'static str,
) -> Result<Matrix<T>, EmbeddedError>
where
    T: ComplexField<Real = T> + Float,
{
    if matrix.nrows() != matrix.ncols() {
        return Err(EmbeddedError::DimensionMismatch {
            which,
            expected_rows: matrix.nrows(),
            expected_cols: matrix.nrows(),
            actual_rows: matrix.nrows(),
            actual_cols: matrix.ncols(),
        });
    }
    if rhs.nrows() != matrix.nrows() {
        return Err(EmbeddedError::DimensionMismatch {
            which,
            expected_rows: matrix.nrows(),
            expected_cols: rhs.ncols(),
            actual_rows: rhs.nrows(),
            actual_cols: rhs.ncols(),
        });
    }

    let factor = matrix
        .as_ref()
        .llt(Side::Lower)
        .map_err(|_| EmbeddedError::NonPositiveDefinite { which })?;
    let mut solution = factor.solve(rhs.as_ref());
    for row in 0..solution.nrows() {
        for col in 0..solution.ncols() {
            solution[(row, col)] = ensure_finite(solution[(row, col)], which)?;
        }
    }
    Ok(solution)
}

/// Returns the lower-triangular Cholesky factor `L` such that `A = L L^T`.
pub(super) fn cholesky_lower<T>(
    matrix: &Matrix<T>,
    which: &'static str,
) -> Result<Matrix<T>, EmbeddedError>
where
    T: Float,
{
    if matrix.nrows() != matrix.ncols() {
        return Err(EmbeddedError::DimensionMismatch {
            which,
            expected_rows: matrix.nrows(),
            expected_cols: matrix.nrows(),
            actual_rows: matrix.nrows(),
            actual_cols: matrix.ncols(),
        });
    }

    let n = matrix.nrows();
    let mut out = zero_matrix(n, n);
    for i in 0..n {
        for j in 0..=i {
            let mut sum = matrix[(i, j)];
            for k in 0..j {
                sum = sum - out[(i, k)] * out[(j, k)];
            }

            if i == j {
                if !sum.is_finite() || sum <= T::zero() {
                    return Err(EmbeddedError::NonPositiveDefinite { which });
                }
                out[(i, j)] = sum.sqrt();
            } else {
                out[(i, j)] = sum / out[(j, j)];
            }
        }
    }
    Ok(out)
}
