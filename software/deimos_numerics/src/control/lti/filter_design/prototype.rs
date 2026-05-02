//! Analog lowpass prototype generation.
//!
//! The public design layer builds every supported filter from one of these
//! normalized lowpass prototypes before applying the requested analog shape
//! transform.
//!
//! # Glossary
//!
//! - **Prototype:** Normalized lowpass filter used as the starting point for a
//!   larger design family.
//! - **Normalized cutoff:** Prototype cutoff fixed to a canonical value,
//!   usually `1 rad/s`.
//! - **Passband ripple:** Allowed gain variation inside the passband.
//! - **Pole-zero-gain (ZPK):** Representation using roots and an overall gain.

use super::error::FilterDesignError;
use super::spec::AnalogFilterFamily;
use crate::control::lti::util::poly_roots;
use crate::control::lti::zpk::ContinuousZpk;
use crate::control::lti::{ContinuousTime, LtiError};
use alloc::vec::Vec;
use faer::complex::Complex;
use faer_traits::RealField;
use faer_traits::ext::ComplexFieldExt;
use num_traits::Float;

pub(super) fn analog_lowpass_prototype_zpk<R>(
    order: usize,
    family: AnalogFilterFamily<R>,
) -> Result<ContinuousZpk<R>, FilterDesignError>
where
    R: Float + RealField,
{
    match family {
        AnalogFilterFamily::Butterworth => butterworth_prototype(order),
        AnalogFilterFamily::Chebyshev1 { ripple_db } => chebyshev1_prototype(order, ripple_db),
        AnalogFilterFamily::Bessel => bessel_prototype(order),
    }
}

fn butterworth_prototype<R>(order: usize) -> Result<ContinuousZpk<R>, FilterDesignError>
where
    R: Float + RealField,
{
    let n = R::from(order).unwrap();
    let two = R::from(2.0).unwrap();
    let pi = R::from(core::f64::consts::PI).unwrap();
    let mut poles = Vec::with_capacity(order);
    for k in 0..order {
        let k = R::from(k).unwrap();
        // Butterworth poles lie uniformly on the unit semicircle in the left
        // half-plane after normalization.
        let theta = pi * (two * k + n + R::one()) / (two * n);
        poles.push(Complex::new(theta.cos(), theta.sin()));
    }
    let gain = gain_for_dc_target::<R>(&[], &poles, R::one())?;
    ContinuousZpk::new(Vec::new(), poles, gain, ContinuousTime).map_err(Into::into)
}

fn chebyshev1_prototype<R>(
    order: usize,
    ripple_db: R,
) -> Result<ContinuousZpk<R>, FilterDesignError>
where
    R: Float + RealField,
{
    let n = R::from(order).unwrap();
    // Type-I Chebyshev prototypes are parameterized by passband ripple via the
    // standard epsilon / asinh transform of the Butterworth pole angles.
    //
    // This implementation intentionally normalizes the lowpass prototype for
    // unity DC gain for both odd and even orders. Some literature instead uses
    // the equiripple passband convention in which even-order Type-I filters
    // have DC gain `1 / sqrt(1 + epsilon^2)`.
    let epsilon = (R::from(10.0)
        .unwrap()
        .powf(ripple_db / R::from(10.0).unwrap())
        - R::one())
    .sqrt();
    let mu = ((R::one() / epsilon) + ((R::one() / epsilon).powi(2) + R::one()).sqrt()).ln() / n;
    let pi = R::from(core::f64::consts::PI).unwrap();
    let two = R::from(2.0).unwrap();
    let sinh_mu = mu.sinh();
    let cosh_mu = mu.cosh();
    let mut poles = Vec::with_capacity(order);
    for k in 0..order {
        let theta = pi * (two * R::from(k).unwrap() + R::one()) / (two * n);
        let re = -sinh_mu * theta.sin();
        let im = cosh_mu * theta.cos();
        poles.push(Complex::new(re, im));
    }
    let gain = gain_for_dc_target::<R>(&[], &poles, R::one())?;
    ContinuousZpk::new(Vec::new(), poles, gain, ContinuousTime).map_err(Into::into)
}

fn bessel_prototype<R>(order: usize) -> Result<ContinuousZpk<R>, FilterDesignError>
where
    R: Float + RealField,
{
    // Reverse Bessel polynomials are tabulated in coefficient form. Their
    // roots are the classical delay-normalized Bessel poles, so they must be
    // rescaled before the rest of the design layer can treat them as ordinary
    // cutoff-normalized lowpass prototypes.
    let denominator = reverse_bessel_denominator::<R>(order);
    let delay_normalized_poles = poly_roots(&denominator)?;
    let delay_normalized_gain = gain_for_dc_target::<R>(&[], &delay_normalized_poles, R::one())?;
    let cutoff_scale = bessel_cutoff_normalization(delay_normalized_gain, &delay_normalized_poles)?;
    let poles = delay_normalized_poles
        .iter()
        .map(|&pole| pole / cutoff_scale)
        .collect::<Vec<_>>();
    let gain = gain_for_dc_target::<R>(&[], &poles, R::one())?;
    ContinuousZpk::new(Vec::new(), poles, gain, ContinuousTime).map_err(Into::into)
}

fn reverse_bessel_denominator<R>(order: usize) -> Vec<R>
where
    R: Float + RealField,
{
    let mut ascending = Vec::with_capacity(order + 1);
    for k in 0..=order {
        let coeff = factorial_f64(2 * order - k)
            / (2.0f64.powi((order - k) as i32) * factorial_f64(k) * factorial_f64(order - k));
        ascending.push(R::from(coeff).unwrap());
    }
    ascending.into_iter().rev().collect()
}

fn factorial_f64(n: usize) -> f64 {
    (1..=n).fold(1.0, |acc, value| acc * value as f64)
}

fn bessel_cutoff_normalization<R>(gain: R, poles: &[Complex<R>]) -> Result<R, FilterDesignError>
where
    R: Float + RealField,
{
    let target = R::one() / R::from(2.0).unwrap().sqrt();
    let mut lower = R::zero();
    let mut upper = R::one();

    while zpk_magnitude_at(gain, poles, upper)? > target {
        upper = upper + upper;
        if !upper.is_finite() {
            return Err(FilterDesignError::Lti(LtiError::NonFiniteResult {
                which: "bessel_cutoff_normalization",
            }));
        }
    }

    for _ in 0..80 {
        let mid = (lower + upper) / R::from(2.0).unwrap();
        if zpk_magnitude_at(gain, poles, mid)? > target {
            lower = mid;
        } else {
            upper = mid;
        }
    }

    let scale = (lower + upper) / R::from(2.0).unwrap();
    if !scale.is_finite() || scale <= R::zero() {
        Err(FilterDesignError::Lti(LtiError::NonFiniteResult {
            which: "bessel_cutoff_normalization",
        }))
    } else {
        Ok(scale)
    }
}

fn zpk_magnitude_at<R>(gain: R, poles: &[Complex<R>], omega: R) -> Result<R, FilterDesignError>
where
    R: Float + RealField,
{
    let s = Complex::new(R::zero(), omega);
    let denominator = poles
        .iter()
        .fold(Complex::new(R::one(), R::zero()), |acc, &pole| {
            acc * (s - pole)
        });
    let value = Complex::new(gain, R::zero()) / denominator;
    let magnitude = value.abs();
    if magnitude.is_finite() {
        Ok(magnitude)
    } else {
        Err(FilterDesignError::Lti(LtiError::NonFiniteResult {
            which: "bessel_cutoff_normalization",
        }))
    }
}

fn gain_for_dc_target<R>(
    zeros: &[Complex<R>],
    poles: &[Complex<R>],
    target: R,
) -> Result<R, FilterDesignError>
where
    R: Float + RealField,
{
    // The prototype builders naturally determine poles, but not always the
    // real scalar gain convention we want. Normalize here so the lowpass
    // response has the requested value at s = 0.
    let num = zeros
        .iter()
        .fold(Complex::new(R::one(), R::zero()), |acc, &zero| acc * -zero);
    let den = poles
        .iter()
        .fold(Complex::new(R::one(), R::zero()), |acc, &pole| acc * -pole);
    real_scalar(
        Complex::new(target, R::zero()) * den / num,
        "prototype_gain",
    )
}

fn real_scalar<R>(value: Complex<R>, which: &'static str) -> Result<R, FilterDesignError>
where
    R: Float + RealField,
{
    let scale = R::one().max(value.re.abs()).max(value.im.abs());
    let tol = R::from(128.0).unwrap() * R::epsilon() * scale;
    if value.im.abs() > tol || !value.re.is_finite() {
        Err(FilterDesignError::Lti(LtiError::NonFiniteResult { which }))
    } else {
        Ok(value.re)
    }
}
