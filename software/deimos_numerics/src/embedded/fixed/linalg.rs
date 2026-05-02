//! Fixed-size linear algebra helpers for the embedded runtime lane.
//!
use crate::embedded::error::EmbeddedError;
use crate::embedded::math::ensure_finite;
use num_traits::Float;

/// Fixed-size row-major matrix storage.
pub type Matrix<T, const R: usize, const C: usize> = [[T; C]; R];

/// Fixed-size column vector storage.
pub type Vector<T, const N: usize> = [T; N];

/// Returns the all-zero fixed-size matrix.
pub(crate) fn zero_matrix<T, const R: usize, const C: usize>() -> Matrix<T, R, C>
where
    T: Float,
{
    [[T::zero(); C]; R]
}

/// Returns the all-zero fixed-size vector.
pub(crate) fn zero_vector<T, const N: usize>() -> Vector<T, N>
where
    T: Float,
{
    [T::zero(); N]
}

/// Returns the identity matrix of order `N`.
pub(crate) fn identity_matrix<T, const N: usize>() -> Matrix<T, N, N>
where
    T: Float,
{
    let mut out = zero_matrix::<T, N, N>();
    for i in 0..N {
        out[i][i] = T::one();
    }
    out
}

/// Multiplies one fixed-size matrix by one fixed-size vector.
pub(crate) fn mat_vec_mul<T, const R: usize, const C: usize>(
    a: &Matrix<T, R, C>,
    x: &Vector<T, C>,
) -> Vector<T, R>
where
    T: Float,
{
    let mut out = zero_vector::<T, R>();
    for i in 0..R {
        let mut acc = T::zero();
        for j in 0..C {
            acc = acc + a[i][j] * x[j];
        }
        out[i] = acc;
    }
    out
}

/// Multiplies two fixed-size matrices.
pub(crate) fn mat_mul<T, const R: usize, const K: usize, const C: usize>(
    a: &Matrix<T, R, K>,
    b: &Matrix<T, K, C>,
) -> Matrix<T, R, C>
where
    T: Float,
{
    let mut out = zero_matrix::<T, R, C>();
    for i in 0..R {
        for j in 0..C {
            let mut acc = T::zero();
            for k in 0..K {
                acc = acc + a[i][k] * b[k][j];
            }
            out[i][j] = acc;
        }
    }
    out
}

/// Transposes one fixed-size matrix.
pub(crate) fn transpose<T, const R: usize, const C: usize>(a: &Matrix<T, R, C>) -> Matrix<T, C, R>
where
    T: Float,
{
    let mut out = zero_matrix::<T, C, R>();
    for i in 0..R {
        for j in 0..C {
            out[j][i] = a[i][j];
        }
    }
    out
}

/// Adds two fixed-size matrices elementwise.
pub(crate) fn mat_add<T, const R: usize, const C: usize>(
    a: &Matrix<T, R, C>,
    b: &Matrix<T, R, C>,
) -> Matrix<T, R, C>
where
    T: Float,
{
    let mut out = zero_matrix::<T, R, C>();
    for i in 0..R {
        for j in 0..C {
            out[i][j] = a[i][j] + b[i][j];
        }
    }
    out
}

/// Subtracts two fixed-size matrices elementwise.
pub(crate) fn mat_sub<T, const R: usize, const C: usize>(
    a: &Matrix<T, R, C>,
    b: &Matrix<T, R, C>,
) -> Matrix<T, R, C>
where
    T: Float,
{
    let mut out = zero_matrix::<T, R, C>();
    for i in 0..R {
        for j in 0..C {
            out[i][j] = a[i][j] - b[i][j];
        }
    }
    out
}

/// Adds two fixed-size vectors elementwise.
pub(crate) fn vec_add<T, const N: usize>(a: &Vector<T, N>, b: &Vector<T, N>) -> Vector<T, N>
where
    T: Float,
{
    let mut out = zero_vector::<T, N>();
    for i in 0..N {
        out[i] = a[i] + b[i];
    }
    out
}

/// Subtracts two fixed-size vectors elementwise.
pub(crate) fn vec_sub<T, const N: usize>(a: &Vector<T, N>, b: &Vector<T, N>) -> Vector<T, N>
where
    T: Float,
{
    let mut out = zero_vector::<T, N>();
    for i in 0..N {
        out[i] = a[i] - b[i];
    }
    out
}

/// Computes the Euclidean norm of one fixed-size vector.
pub(crate) fn vec_norm<T, const N: usize>(x: &Vector<T, N>) -> T
where
    T: Float,
{
    let mut sum = T::zero();
    for i in 0..N {
        sum = sum + x[i] * x[i];
    }
    sum.sqrt()
}

/// Solves `A X = B` for one fixed-size right-hand side block.
///
/// This routine is the small, no-allocation dense solve used by fixed-size
/// embedded code when an `alloc`-backed solver is unavailable or unnecessary.
/// It performs Gauss-Jordan elimination with partial pivoting:
///
/// - each step selects the largest available pivot in the active column,
/// - swaps the pivot row into place in both `A` and `B`,
/// - scales the pivot row to make the pivot equal to one,
/// - eliminates that column from every other row.
///
/// After the last column, the working copy of `A` has been reduced to the
/// identity and the working copy of `B` contains `X = A^-1 B`. Multiple
/// right-hand sides are handled at the same time by treating `B` as an `N x M`
/// block.
///
/// The singularity test is deliberately conservative: a pivot whose magnitude
/// is at or below `sqrt(epsilon)` is rejected before division. That avoids
/// accepting clearly ill-conditioned solves in embedded paths that cannot fall
/// back to iterative refinement. The final finite-value check catches overflow,
/// invalid arithmetic, and non-finite inputs that survive the pivot tests.
///
/// Args:
///   a: Coefficient matrix with shape `(N, N)`.
///   b: Right-hand side block with shape `(N, M)`.
///   which: Error context returned with singular or non-finite failures.
///
/// Returns:
///   Solution block `X` with shape `(N, M)`.
pub(crate) fn solve_linear_system<T, const N: usize, const M: usize>(
    a: &Matrix<T, N, N>,
    b: &Matrix<T, N, M>,
    which: &'static str,
) -> Result<Matrix<T, N, M>, EmbeddedError>
where
    T: Float,
{
    let mut a = *a;
    let mut b = *b;
    let epsilon = T::epsilon().sqrt();

    for k in 0..N {
        // Partial pivoting keeps the largest remaining entry in the active
        // column on the diagonal before dividing by it.
        let mut pivot_row = k;
        let mut pivot_abs = a[k][k].abs();
        for row in (k + 1)..N {
            let candidate = a[row][k].abs();
            if candidate > pivot_abs {
                pivot_abs = candidate;
                pivot_row = row;
            }
        }

        if pivot_abs <= epsilon {
            return Err(EmbeddedError::SingularMatrix { which });
        }

        if pivot_row != k {
            // The same row operation must be applied to the right-hand side so
            // the augmented system `[A | B]` remains equivalent.
            a.swap(k, pivot_row);
            b.swap(k, pivot_row);
        }

        // Normalize the pivot row first; subsequent elimination can then use
        // the pivot-column entry directly as the multiplier.
        let diag = a[k][k];
        for j in k..N {
            a[k][j] = a[k][j] / diag;
        }
        for rhs_col in 0..M {
            b[k][rhs_col] = b[k][rhs_col] / diag;
        }

        // Gauss-Jordan elimination clears the pivot column in every other row,
        // so no separate back-substitution pass is required.
        for i in 0..N {
            if i != k {
                let factor = a[i][k];
                if factor != T::zero() {
                    for j in k..N {
                        a[i][j] = a[i][j] - factor * a[k][j];
                    }
                    for rhs_col in 0..M {
                        b[i][rhs_col] = b[i][rhs_col] - factor * b[k][rhs_col];
                    }
                }
            }
        }
    }

    for row in 0..N {
        for col in 0..M {
            b[row][col] = ensure_finite(b[row][col], which)?;
        }
    }

    Ok(b)
}
