//! Fixed-size fractional-delay FIR constructors.
//!
//! # Glossary
//!
//! - **Fractional delay:** Delay by a non-integer number of samples.
//! - **Lagrange taps:** Interpolating FIR coefficients derived from Lagrange
//!   polynomials.

use super::Fir;
use crate::embedded::error::EmbeddedError;
use crate::embedded::math::ensure_finite;
use num_traits::{Float, NumCast};

/// Computes Lagrange fractional-delay taps in newest-to-oldest order.
///
/// `delay` is measured in samples behind the newest input. For best results,
/// keep it in `[0, TAPS - 1]`; the common fractional-delay use case is
/// `[0, 1]`.
pub fn lagrange_fractional_delay_taps<const TAPS: usize, T>(
    delay: T,
) -> Result<[T; TAPS], EmbeddedError>
where
    T: Float,
{
    if TAPS < 2 {
        return Err(EmbeddedError::InvalidParameter {
            which: "fir.lagrange.taps",
        });
    }
    ensure_finite(delay, "fir.lagrange.delay")?;

    let mut taps = [T::zero(); TAPS];
    for (k, tap) in taps.iter_mut().enumerate() {
        let kv = cast_index::<T>(k, "fir.lagrange.index")?;
        let mut coeff = T::one();
        for m in 0..TAPS {
            if m != k {
                let mv = cast_index::<T>(m, "fir.lagrange.index")?;
                coeff = coeff * (delay - mv) / (kv - mv);
            }
        }
        *tap = coeff;
    }

    let mut tap_sum = T::zero();
    for &tap in &taps {
        tap_sum = tap_sum + tap;
    }
    if !tap_sum.is_finite() || tap_sum == T::zero() {
        return Err(EmbeddedError::NonFiniteValue {
            which: "fir.lagrange.tap_sum",
        });
    }

    for tap in &mut taps {
        *tap = ensure_finite(*tap / tap_sum, "fir.lagrange.taps")?;
    }

    Ok(taps)
}

/// Creates a fixed-size FIR bank implementing a Lagrange fractional delay.
pub fn lagrange_fractional_delay<const TAPS: usize, const LANES: usize, T>(
    delay: T,
    sample_time: T,
) -> Result<Fir<T, TAPS, LANES>, EmbeddedError>
where
    T: Float,
{
    Fir::new(
        lagrange_fractional_delay_taps::<TAPS, T>(delay)?,
        sample_time,
    )
}

/// Casts one compile-time index into the target scalar type.
fn cast_index<T>(value: usize, which: &'static str) -> Result<T, EmbeddedError>
where
    T: Float,
{
    NumCast::from(value).ok_or(EmbeddedError::InvalidParameter { which })
}

#[cfg(test)]
mod tests {
    use super::{lagrange_fractional_delay, lagrange_fractional_delay_taps};
    use crate::embedded::fixed::lti::FirState;

    fn assert_close(lhs: f32, rhs: f32, tol: f32) {
        let err = (lhs - rhs).abs();
        assert!(err <= tol, "lhs={lhs}, rhs={rhs}, err={err}, tol={tol}");
    }

    #[test]
    fn lagrange_fractional_delay_reproduces_polynomials() {
        let taps = lagrange_fractional_delay_taps::<4, f32>(0.2).unwrap();
        assert_close(taps.iter().copied().sum::<f32>(), 1.0, 1.0e-6);

        let func = |x: f32| 0.3 + 0.18 * x - 0.5 * x * x + 0.7 * x * x * x;
        let values = [func(0.0), func(1.0), func(2.0), func(3.0)];
        let filter = lagrange_fractional_delay::<4, 1, f32>(0.2, 1.0).unwrap();
        let mut state = FirState::<f32, 4, 1>::zeros();
        let mut output = 0.0;

        for &value in &values {
            output = filter.step(&mut state, [value])[0];
        }

        assert_close(output, func(2.8), 1.0e-6);
    }
}
