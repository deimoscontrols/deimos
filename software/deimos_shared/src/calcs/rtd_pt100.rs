//! Compact IEC 60751 Pt100 conversion functions.
//!
//! The forward direction evaluates the Callendar--Van Dusen (CVD) equation
//! directly. The inverse direction uses one degree-10 polynomial fitted to the
//! CVD curve over the standard `-200 degC` to `850 degC` range. The emitted
//! `f32` inverse was checked for monotonicity and for a maximum temperature
//! error of `0.01 K`; [`crate::calcs`] contains the generated coefficients.
//!
//! # References
//!
//! \[1\] IEC 60751, *Industrial platinum resistance thermometers and platinum
//!     temperature sensors*.

mod data {
    include!("rtd_pt100_data.rs");
}

use data::*;

const ZERO_C_K: f32 = PT100_TEMPERATURE_ORIGIN_K;
const R0_OHM: f32 = 100.0;
const A: f32 = 3.9083e-3;
const B: f32 = -5.775e-7;
const C: f32 = -4.183e-12;

/// Converts temperature to Pt100 resistance with the IEC 60751 CVD curve.
///
/// The standard range is `73.15 K` to `1123.15 K` (`-200 degC` to `850 degC`).
/// Finite values outside that range are evaluated with the same CVD expression
/// rather than clamped. IEEE-754 exceptional values propagate through the
/// arithmetic naturally.
///
/// Args:
///   temperature_k: Absolute temperature scalar in `K`.
///
/// Returns:
///   Pt100 resistance scalar in `ohm`.
///
/// References:
///   \[1\] IEC 60751, *Industrial platinum resistance thermometers and
///   platinum temperature sensors*.
#[inline]
pub fn pt100_resistance_ohm_f32(temperature_k: f32) -> f32 {
    let temperature_c = temperature_k - ZERO_C_K;
    let temperature_c_2 = temperature_c * temperature_c;
    let negative_term = if temperature_c < 0.0 {
        C * (temperature_c - 100.0) * temperature_c_2 * temperature_c
    } else {
        0.0
    };
    R0_OHM * (1.0 + A * temperature_c + B * temperature_c_2 + negative_term)
}

/// Converts Pt100 resistance to temperature with one prevalidated global polynomial.
///
/// The supported IEC range uses one normalized power-basis polynomial and one
/// fixed Horner chain, with no range partition or iterative solve. Finite values
/// outside that range use the exact CVD inverse endpoint tangent. NaN propagates;
/// infinite resistance extrapolates to the corresponding infinite temperature.
///
/// Args:
///   resistance_ohm: Pt100 resistance scalar in `ohm`.
///
/// Returns:
///   Absolute temperature scalar in `K`. The maximum fitted-range error of the
///   emitted `f32` evaluator is `0.01 K`.
///
/// References:
///   \[1\] IEC 60751, *Industrial platinum resistance thermometers and
///   platinum temperature sensors*.
#[inline]
pub fn pt100_temperature_k_f32(resistance_ohm: f32) -> f32 {
    #[inline]
    fn inside(resistance_ohm: f32) -> f32 {
        let normalized = (resistance_ohm - PT100_RESISTANCE_ORIGIN_OHM) * PT100_RESISTANCE_SCALE;
        let mut temperature_c =
            PT100_INVERSE_COEFFICIENTS_C[PT100_INVERSE_COEFFICIENTS_C.len() - 1];
        for &coefficient in PT100_INVERSE_COEFFICIENTS_C[..PT100_INVERSE_COEFFICIENTS_C.len() - 1]
            .iter()
            .rev()
        {
            temperature_c = temperature_c * normalized + coefficient;
        }
        temperature_c + PT100_TEMPERATURE_ORIGIN_K
    }

    if resistance_ohm < PT100_MIN_RESISTANCE_OHM {
        inside(PT100_MIN_RESISTANCE_OHM)
            + PT100_MIN_EXTRAPOLATION_SLOPE_K_PER_OHM * (resistance_ohm - PT100_MIN_RESISTANCE_OHM)
    } else if resistance_ohm > PT100_MAX_RESISTANCE_OHM {
        inside(PT100_MAX_RESISTANCE_OHM)
            + PT100_MAX_EXTRAPOLATION_SLOPE_K_PER_OHM * (resistance_ohm - PT100_MAX_RESISTANCE_OHM)
    } else {
        inside(resistance_ohm)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reference_resistance(temperature_c: f64) -> f64 {
        const A: f64 = 3.9083e-3;
        const B: f64 = -5.775e-7;
        const C: f64 = -4.183e-12;
        let negative = if temperature_c < 0.0 {
            C * (temperature_c - 100.0) * temperature_c.powi(3)
        } else {
            0.0
        };
        100.0 * (1.0 + A * temperature_c + B * temperature_c.powi(2) + negative)
    }

    #[test]
    fn global_inverse_stays_within_temperature_error_limit() {
        let mut max_error = 0.0_f64;
        for index in 0..=1_050_000 {
            let temperature_c = -200.0 + index as f64 / 1000.0;
            let resistance = reference_resistance(temperature_c);
            let calculated = pt100_temperature_k_f32(resistance as f32) as f64;
            max_error = max_error.max((calculated - (temperature_c + 273.15)).abs());
        }
        assert!(max_error <= 0.01, "maximum inverse error was {max_error} K");
    }

    #[test]
    fn global_inverse_is_monotonic_and_extrapolates_with_endpoint_tangents() {
        let mut previous = pt100_temperature_k_f32(PT100_MIN_RESISTANCE_OHM);
        for index in 1..=1_000_000 {
            let resistance = PT100_MIN_RESISTANCE_OHM
                + (PT100_MAX_RESISTANCE_OHM - PT100_MIN_RESISTANCE_OHM) * index as f32
                    / 1_000_000.0;
            let next = pt100_temperature_k_f32(resistance);
            assert!(next >= previous);
            previous = next;
        }

        let delta = 0.25_f32;
        let below = pt100_temperature_k_f32(PT100_MIN_RESISTANCE_OHM - delta);
        let at_minimum = pt100_temperature_k_f32(PT100_MIN_RESISTANCE_OHM);
        assert!(
            ((at_minimum - below) / delta - PT100_MIN_EXTRAPOLATION_SLOPE_K_PER_OHM).abs() < 1.0e-4
        );

        let at_maximum = pt100_temperature_k_f32(PT100_MAX_RESISTANCE_OHM);
        let above = pt100_temperature_k_f32(PT100_MAX_RESISTANCE_OHM + delta);
        assert!(
            ((above - at_maximum) / delta - PT100_MAX_EXTRAPOLATION_SLOPE_K_PER_OHM).abs() < 1.0e-4
        );
    }

    #[test]
    fn nonfinite_behavior_is_explicit() {
        assert!(pt100_resistance_ohm_f32(f32::NAN).is_nan());
        assert!(pt100_resistance_ohm_f32(f32::INFINITY).is_nan());
        assert!(pt100_temperature_k_f32(f32::NAN).is_nan());
        assert_eq!(pt100_temperature_k_f32(f32::INFINITY), f32::INFINITY);
        assert_eq!(
            pt100_temperature_k_f32(f32::NEG_INFINITY),
            f32::NEG_INFINITY
        );
    }

    #[test]
    fn forward_curve_matches_iec_coefficients_in_f32() {
        for temperature_c in (-200..=850).step_by(5) {
            let expected = reference_resistance(temperature_c as f64);
            let calculated = pt100_resistance_ohm_f32(temperature_c as f32 + ZERO_C_K);
            assert!((calculated as f64 - expected).abs() < 5.0e-5);
        }
    }
}
