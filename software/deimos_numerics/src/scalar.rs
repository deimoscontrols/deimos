//! Scalar fused multiply-add helpers.
//!
//! These helpers centralize the crate's optional use of `Float::mul_add` under
//! the `fma` feature so call sites can opt into fused multiply-add without
//! repeating feature-gated arithmetic branches throughout the codebase.
//!
//! # Glossary
//!
//! - **FMA:** Fused multiply-add, computing `a * b + c` with a single rounding
//!   step when hardware or the language primitive permits it.
//! - **Feature gate:** Compile-time option selecting one implementation path
//!   or another.

use faer::complex::Complex;
use faer_traits::RealField;
use num_traits::Float;

/// Returns `mul_lhs * mul_rhs + addend`.
///
/// When the `fma` feature is enabled this uses `Float::mul_add`; when it is
/// disabled the expression falls back to the ordinary multiply-then-add
/// sequence.
#[inline]
pub(crate) fn mul_add<R>(mul_lhs: R, mul_rhs: R, addend: R) -> R
where
    R: Float,
{
    #[cfg(feature = "fma")]
    {
        mul_lhs.mul_add(mul_rhs, addend)
    }
    #[cfg(not(feature = "fma"))]
    {
        mul_lhs * mul_rhs + addend
    }
}

/// Returns `addend - mul_lhs * mul_rhs`.
///
/// This is the subtraction counterpart to [`mul_add`], implemented with the
/// same `fma`-gated fused or unfused arithmetic path.
#[inline]
pub(crate) fn neg_mul_add<R>(mul_lhs: R, mul_rhs: R, addend: R) -> R
where
    R: Float,
{
    #[cfg(feature = "fma")]
    {
        (-mul_lhs).mul_add(mul_rhs, addend)
    }
    #[cfg(not(feature = "fma"))]
    {
        addend - mul_lhs * mul_rhs
    }
}

/// Returns `lhs * rhs + addend` for complex scalars.
///
/// The real and imaginary components are assembled from the scalar helpers so
/// the `fma` feature can still help the common Horner and frequency-
/// response paths that operate on complex numbers built from real coefficients.
#[inline]
pub(crate) fn complex_mul_add<R>(lhs: Complex<R>, rhs: Complex<R>, addend: Complex<R>) -> Complex<R>
where
    R: Float + RealField,
{
    let real = neg_mul_add(lhs.im, rhs.im, mul_add(lhs.re, rhs.re, addend.re));
    let imag = mul_add(lhs.im, rhs.re, mul_add(lhs.re, rhs.im, addend.im));
    Complex::new(real, imag)
}

/// Returns `real * rhs + addend` for a real scalar and a complex factor.
#[inline]
pub(crate) fn real_complex_mul_add<R>(real: R, rhs: Complex<R>, addend: Complex<R>) -> Complex<R>
where
    R: Float + RealField,
{
    Complex::new(
        mul_add(real, rhs.re, addend.re),
        mul_add(real, rhs.im, addend.im),
    )
}

/// Returns one Horner step `acc * point + coef` for a real-coefficient complex
/// polynomial evaluation.
#[inline]
pub(crate) fn complex_horner_step_real<R>(acc: Complex<R>, point: Complex<R>, coef: R) -> Complex<R>
where
    R: Float + RealField,
{
    complex_mul_add(acc, point, Complex::new(coef, R::zero()))
}

#[cfg(test)]
mod tests {
    use super::{
        complex_horner_step_real, complex_mul_add, mul_add, neg_mul_add, real_complex_mul_add,
    };
    use faer::complex::Complex;

    fn assert_close(lhs: f64, rhs: f64, tol: f64) {
        let err = (lhs - rhs).abs();
        assert!(err <= tol, "lhs={lhs}, rhs={rhs}, err={err}, tol={tol}");
    }

    #[test]
    fn mul_add_matches_plain_expression() {
        assert_close(mul_add(1.25f64, -3.0, 0.5), 1.25 * -3.0 + 0.5, 0.0);
        assert_close(neg_mul_add(1.25f64, -3.0, 0.5), 0.5 - 1.25 * -3.0, 0.0);
    }

    #[test]
    fn complex_helpers_match_plain_expression() {
        let lhs = Complex::new(1.5f64, -0.25);
        let rhs = Complex::new(-2.0f64, 0.75);
        let addend = Complex::new(0.5f64, -1.25);
        let fused = complex_mul_add(lhs, rhs, addend);
        let expected = lhs * rhs + addend;
        assert_close(fused.re, expected.re, 0.0);
        assert_close(fused.im, expected.im, 0.0);

        let real_fused = real_complex_mul_add(3.0f64, rhs, addend);
        let real_expected = Complex::new(3.0, 0.0) * rhs + addend;
        assert_close(real_fused.re, real_expected.re, 0.0);
        assert_close(real_fused.im, real_expected.im, 0.0);
    }

    #[test]
    fn complex_horner_step_matches_plain_expression() {
        let acc = Complex::new(1.0f64, -2.0);
        let point = Complex::new(0.5f64, 0.25);
        let coef = -3.0f64;
        let step = complex_horner_step_real(acc, point, coef);
        let expected = acc * point + Complex::new(coef, 0.0);
        assert_close(step.re, expected.re, 0.0);
        assert_close(step.im, expected.im, 0.0);
    }
}
