//! Shared dense-matrix helpers for control algorithms.
//!
//! These helpers centralize the crate's repeated dense kernels so control
//! submodules can share one implementation and one accumulation policy.
//!
//! # Glossary
//!
//! - **Adjoint:** Conjugate transpose of a matrix.
//! - **Compensated accumulation:** Summation that keeps a running correction
//!   term to reduce floating-point loss.
//! - **Hermitian projection:** Explicit symmetrization step used to pull a
//!   nearly Hermitian matrix back onto the Hermitian manifold.
//! - **Frobenius norm:** Square-root of the sum of squared entry magnitudes.

use crate::sparse::compensated::{CompensatedField, CompensatedSum};
use crate::twosum::TwoSum;
use faer::prelude::Solve;
use faer::{Mat, MatRef};
use faer_traits::ext::ComplexFieldExt;
use num_traits::{Float, One, Zero};

/// Dense matrix multiply using compensated accumulation per output entry.
pub(crate) fn dense_mul<T>(lhs: MatRef<'_, T>, rhs: MatRef<'_, T>) -> Mat<T>
where
    T: CompensatedField,
    T::Real: Float,
{
    Mat::from_fn(lhs.nrows(), rhs.ncols(), |row, col| {
        let mut acc = CompensatedSum::<T>::default();
        for k in 0..lhs.ncols() {
            acc.add(lhs[(row, k)] * rhs[(k, col)]);
        }
        acc.finish()
    })
}

/// Dense multiply with an adjoint on the right-hand factor.
pub(crate) fn dense_mul_adjoint_rhs<T>(lhs: MatRef<'_, T>, rhs: MatRef<'_, T>) -> Mat<T>
where
    T: CompensatedField,
    T::Real: Float,
{
    Mat::from_fn(lhs.nrows(), rhs.nrows(), |row, col| {
        let mut acc = CompensatedSum::<T>::default();
        for k in 0..lhs.ncols() {
            acc.add(lhs[(row, k)] * rhs[(col, k)].conj());
        }
        acc.finish()
    })
}

/// Dense multiply with an adjoint on the left-hand factor.
pub(crate) fn dense_mul_adjoint_lhs<T>(lhs: MatRef<'_, T>, rhs: MatRef<'_, T>) -> Mat<T>
where
    T: CompensatedField,
    T::Real: Float,
{
    Mat::from_fn(lhs.ncols(), rhs.ncols(), |row, col| {
        let mut acc = CompensatedSum::<T>::default();
        for k in 0..lhs.nrows() {
            acc.add(lhs[(k, row)].conj() * rhs[(k, col)]);
        }
        acc.finish()
    })
}

/// Plain dense matrix multiply used only in residual checks or lightweight analysis.
pub(crate) fn dense_mul_plain<T>(lhs: MatRef<'_, T>, rhs: MatRef<'_, T>) -> Mat<T>
where
    T: faer_traits::ComplexField + Copy,
{
    Mat::from_fn(lhs.nrows(), rhs.ncols(), |row, col| {
        let mut acc = T::zero();
        for k in 0..lhs.ncols() {
            acc += lhs[(row, k)] * rhs[(k, col)];
        }
        acc
    })
}

/// Returns the real part of the Hermitian inner product between two vectors.
pub(crate) fn inner_product_real<T>(lhs: MatRef<'_, T>, rhs: MatRef<'_, T>) -> T::Real
where
    T: CompensatedField,
    T::Real: Float,
{
    let mut acc = CompensatedSum::<T>::default();
    for row in 0..lhs.nrows() {
        acc.add(lhs[(row, 0)].conj() * rhs[(row, 0)]);
    }
    acc.finish().real()
}

/// Returns the Euclidean norm of a dense column vector.
pub(crate) fn column_vector_norm<T>(vector: MatRef<'_, T>) -> T::Real
where
    T: CompensatedField,
    T::Real: Float,
{
    let mut acc = <T::Real as Zero>::zero();
    for row in 0..vector.nrows() {
        acc += vector[(row, 0)].abs2();
    }
    acc.sqrt()
}

/// Projects a dense matrix onto the Hermitian subspace in place.
pub(crate) fn hermitian_project_in_place<T>(matrix: &mut Mat<T>)
where
    T: CompensatedField,
    T::Real: Float,
{
    let one = <T::Real as One>::one();
    let half = one / (one + one);
    for col in 0..matrix.ncols() {
        for row in 0..=col {
            let avg = (matrix[(row, col)] + matrix[(col, row)].conj()).mul_real(half);
            matrix[(row, col)] = avg;
            matrix[(col, row)] = avg.conj();
        }
    }
}

/// Returns the Frobenius norm of a dense matrix without compensation.
pub(crate) fn frobenius_norm_plain<T>(matrix: MatRef<'_, T>) -> T::Real
where
    T: faer_traits::ComplexField + Copy,
    T::Real: Float,
{
    let mut acc = <T::Real as Zero>::zero();
    for col in 0..matrix.ncols() {
        for row in 0..matrix.nrows() {
            acc += matrix[(row, col)].abs2();
        }
    }
    acc.sqrt()
}

/// Returns the Frobenius norm of a dense matrix with compensated accumulation.
pub(crate) fn frobenius_norm<T>(matrix: MatRef<'_, T>) -> T::Real
where
    T: faer_traits::ComplexField + Copy,
    T::Real: Float,
{
    let mut acc: Option<TwoSum<T::Real>> = None;
    for col in 0..matrix.ncols() {
        for row in 0..matrix.nrows() {
            let value = matrix[(row, col)].abs2();
            match acc.as_mut() {
                Some(acc) => acc.add(value),
                None => acc = Some(TwoSum::new(value)),
            }
        }
    }

    match acc {
        Some(acc) => {
            let (sum, residual) = acc.finish();
            (sum + residual).sqrt()
        }
        None => <T::Real as Zero>::zero(),
    }
}

/// Returns the default tolerance used by dense residual-checked solves.
pub(crate) fn default_solve_tolerance<T>() -> T::Real
where
    T: CompensatedField,
    T::Real: Float,
{
    T::Real::epsilon().sqrt()
}

/// Solves `lhs * X = rhs` and rejects nonfinite or high-residual results.
pub(crate) fn solve_left_checked<T, E, F>(
    lhs: MatRef<'_, T>,
    rhs: MatRef<'_, T>,
    tol: T::Real,
    err: F,
) -> Result<Mat<T>, E>
where
    T: faer_traits::ComplexField + Copy,
    T::Real: Float,
    F: Fn() -> E,
{
    let solution = lhs.full_piv_lu().solve(rhs);
    if !solution.as_ref().is_all_finite() {
        return Err(err());
    }

    let residual = dense_mul_plain(lhs, solution.as_ref()) - rhs;
    let residual_norm = frobenius_norm_plain(residual.as_ref());
    let scale = frobenius_norm_plain(lhs) * frobenius_norm_plain(solution.as_ref())
        + frobenius_norm_plain(rhs);
    let one = <T::Real as One>::one();
    let threshold = scale.max(one) * tol * (one + one);
    if !residual_norm.is_finite() || residual_norm > threshold {
        return Err(err());
    }

    Ok(solution)
}

/// Solves `X * lhs = rhs` by transposing into [`solve_left_checked`].
pub(crate) fn solve_right_checked<T, E, F>(
    rhs_left: MatRef<'_, T>,
    lhs_right: MatRef<'_, T>,
    tol: T::Real,
    err: F,
) -> Result<Mat<T>, E>
where
    T: faer_traits::ComplexField + Copy,
    T::Real: Float,
    F: Fn() -> E + Copy,
{
    let solved_t = solve_left_checked(lhs_right.transpose(), rhs_left.transpose(), tol, err)?;
    Ok(solved_t.transpose().to_owned())
}
