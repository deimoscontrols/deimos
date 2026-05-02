//! Shared nonlinear-estimation algebra for the alloc-backed embedded runtimes.
//!
//! # Glossary
//!
//! - **Innovation:** Measurement residual between the actual and predicted
//!   outputs.
//! - **Innovation covariance:** Covariance of the innovation vector.
//! - **Weighted mean / covariance:** Unscented-transform moments reconstructed
//!   from sigma points and their weights.
//! - **Joseph update:** Covariance update form that better preserves
//!   positive-semidefiniteness in floating point.

use super::dense::llt_solve;
use crate::control::dense_ops::{dense_mul, dense_mul_adjoint_rhs, hermitian_project_in_place};
use crate::control::estimation::CovarianceUpdate;
use crate::embedded::EmbeddedError;
use crate::sparse::compensated::{CompensatedField, CompensatedSum};
use faer::{Mat, MatRef};
use faer_traits::RealField;
use faer_traits::ext::ComplexFieldExt;
use num_traits::{Float, Zero};

pub(super) fn predict_covariance<R>(f: MatRef<'_, R>, p: MatRef<'_, R>, q: MatRef<'_, R>) -> Mat<R>
where
    R: CompensatedField + RealField,
    R::Real: Float,
{
    let mut covariance = dense_mul_adjoint_rhs(dense_mul(f, p).as_ref(), f) + q;
    hermitian_project_in_place(&mut covariance);
    covariance
}

pub(super) fn weighted_mean<R>(points: MatRef<'_, R>, weights: &[R]) -> Mat<R>
where
    R: CompensatedField + RealField,
    R::Real: Float,
{
    Mat::from_fn(points.nrows(), 1, |row, _| {
        let mut acc = CompensatedSum::<R>::default();
        for col in 0..points.ncols() {
            acc.add(points[(row, col)] * weights[col]);
        }
        acc.finish()
    })
}

pub(super) fn weighted_covariance<R>(
    points: MatRef<'_, R>,
    mean: MatRef<'_, R>,
    weights: &[R],
) -> Mat<R>
where
    R: CompensatedField + RealField,
    R::Real: Float,
{
    Mat::from_fn(points.nrows(), points.nrows(), |row, col| {
        let mut acc = CompensatedSum::<R>::default();
        for k in 0..points.ncols() {
            let dr = points[(row, k)] - mean[(row, 0)];
            let dc = points[(col, k)] - mean[(col, 0)];
            acc.add(dr * dc.conj() * weights[k]);
        }
        acc.finish()
    })
}

pub(super) fn weighted_cross_covariance<R>(
    lhs_points: MatRef<'_, R>,
    lhs_mean: MatRef<'_, R>,
    rhs_points: MatRef<'_, R>,
    rhs_mean: MatRef<'_, R>,
    weights: &[R],
) -> Mat<R>
where
    R: CompensatedField + RealField,
    R::Real: Float,
{
    Mat::from_fn(lhs_points.nrows(), rhs_points.nrows(), |row, col| {
        let mut acc = CompensatedSum::<R>::default();
        for k in 0..lhs_points.ncols() {
            let dl = lhs_points[(row, k)] - lhs_mean[(row, 0)];
            let dr = rhs_points[(col, k)] - rhs_mean[(col, 0)];
            acc.add(dl * dr.conj() * weights[k]);
        }
        acc.finish()
    })
}

pub(super) fn updated_covariance<R>(
    covariance_update: CovarianceUpdate,
    predicted_covariance: MatRef<'_, R>,
    gain: MatRef<'_, R>,
    h: MatRef<'_, R>,
    r: MatRef<'_, R>,
    innovation_covariance: MatRef<'_, R>,
) -> Mat<R>
where
    R: CompensatedField + RealField,
    R::Real: Float,
{
    match covariance_update {
        CovarianceUpdate::Simple => {
            predicted_covariance
                - dense_mul_adjoint_rhs(dense_mul(gain, innovation_covariance).as_ref(), gain)
                    .as_ref()
        }
        CovarianceUpdate::Joseph => {
            let identity =
                Mat::<R>::identity(predicted_covariance.nrows(), predicted_covariance.nrows());
            let kh = dense_mul(gain, h);
            let i_minus_kh = &identity - &kh;
            let first = dense_mul_adjoint_rhs(
                dense_mul(i_minus_kh.as_ref(), predicted_covariance).as_ref(),
                i_minus_kh.as_ref(),
            );
            let second = dense_mul_adjoint_rhs(dense_mul(gain, r).as_ref(), gain);
            &first + &second
        }
    }
}

pub(super) fn updated_covariance_ukf<R>(
    covariance_update: CovarianceUpdate,
    predicted_covariance: MatRef<'_, R>,
    gain: MatRef<'_, R>,
    cross: MatRef<'_, R>,
    r: MatRef<'_, R>,
    innovation_covariance: MatRef<'_, R>,
) -> Result<Mat<R>, EmbeddedError>
where
    R: CompensatedField + RealField,
    R::Real: Float,
{
    match covariance_update {
        CovarianceUpdate::Simple => Ok(predicted_covariance
            - dense_mul_adjoint_rhs(dense_mul(gain, innovation_covariance).as_ref(), gain)
                .as_ref()),
        CovarianceUpdate::Joseph => {
            let h_t = llt_solve(
                &predicted_covariance.to_owned(),
                &cross.to_owned(),
                "embedded.alloc.ukf.predicted_covariance",
            )?;
            Ok(updated_covariance(
                CovarianceUpdate::Joseph,
                predicted_covariance,
                gain,
                h_t.transpose().as_ref(),
                r,
                innovation_covariance,
            ))
        }
    }
}

pub(super) fn normalized_innovation_norm<R>(
    innovation: MatRef<'_, R>,
    innovation_covariance: MatRef<'_, R>,
) -> Result<R::Real, EmbeddedError>
where
    R: CompensatedField + RealField,
    R::Real: Float,
{
    let whitened = llt_solve(
        &innovation_covariance.to_owned(),
        &innovation.to_owned(),
        "embedded.alloc.estimation.innovation_covariance",
    )?;
    let mut acc = CompensatedSum::<R::Real>::default();
    for row in 0..innovation.nrows() {
        acc.add((innovation[(row, 0)] * whitened[(row, 0)]).real());
    }
    Ok(acc.finish().max(<R::Real as Zero>::zero()).sqrt())
}
