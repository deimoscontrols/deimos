//! Compact regular-grid cubic B-spline approximations of the NIST type-K curve.
//!
//! Separate forward and inverse splines approximate the NIST ITS-90 reference
//! function over `-210 degC` to `1370 degC`. Each emitted `f32` spline is
//! monotonic and has a validated maximum temperature-equivalent error of
//! `0.01 K` over its fitted range. Values outside the fitted range use the
//! corresponding spline endpoint tangent. Both fitting and runtime evaluation
//! use `interpn::MultiBsplineRegular`'s coefficient and boundary conventions.
//!
//! # References
//!
//! \[1\] G. W. Burns, M. G. Scroger, G. F. Strouse, M. C. Croarkin, and
//!     W. F. Guthrie, *Temperature-Electromotive Force Reference Functions and
//!     Tables for the Letter-Designated Thermocouple Types Based on the ITS-90*,
//!     NIST Monograph 175, 1993, doi: 10.6028/NIST.MONO.175.
//!
//! \[2\] C. de Boor, *A Practical Guide to Splines*, revised ed., Springer,
//!     2001.

mod data {
    include!("tc_ktype_data.rs");
}

use interpn::MultiBsplineRegular;

use data::{
    TC_FORWARD_COEFFICIENTS_V, TC_FORWARD_STEP_K, TC_INVERSE_COEFFICIENTS_K, TC_INVERSE_STEP_V,
};
pub use data::{
    TC_SPLINE_MAX_TEMPERATURE_K, TC_SPLINE_MAX_VOLTAGE_V, TC_SPLINE_MIN_TEMPERATURE_K,
    TC_SPLINE_MIN_VOLTAGE_V,
};

/// Evaluates one precomputed regular B-spline with the shared `interpn` runtime.
///
/// Args:
///   value: Scalar spline coordinate in input units.
///   start: Coordinate of the first regular-grid node in input units.
///   step: Regular-grid spacing in input units.
///   coefficients: Precomputed `interpn` B-spline coefficients with shape
///     `(n_grid,)` in output units.
///
/// Returns:
///   Interpolated scalar in coefficient units, or `NaN` when `interpn` cannot
///   represent the supplied coordinate.
#[inline]
fn interpolate_regular_bspline(value: f32, start: f32, step: f32, coefficients: &[f32]) -> f32 {
    let Ok(interpolator) = MultiBsplineRegular::<f32, 1>::new(
        [coefficients.len()],
        [start],
        [step],
        coefficients,
        true,
    ) else {
        // Generated dimensions and steps are validated by tests and fixed in
        // the firmware image. Preserve a branchless downstream error value
        // instead of retaining a panic path in every engineering conversion.
        return f32::NAN;
    };
    interpolator.interp_one([value]).unwrap_or(f32::NAN)
}

/// Converts a type-K junction temperature to its NIST-equivalent voltage.
///
/// Values outside `63.15 K` to `1643.15 K` are linearly extrapolated with the
/// fitted spline's endpoint tangent.
///
/// Args:
///   temperature_k: Absolute junction temperature scalar in `K`.
///
/// Returns:
///   NIST-equivalent type-K electromotive-force scalar in `V`.
///
/// References:
///   \[1\] G. W. Burns et al., NIST Monograph 175, 1993,
///   doi: 10.6028/NIST.MONO.175.
#[inline]
pub fn ktype_voltage_v_f32(temperature_k: f32) -> f32 {
    interpolate_regular_bspline(
        temperature_k,
        TC_SPLINE_MIN_TEMPERATURE_K,
        TC_FORWARD_STEP_K,
        &TC_FORWARD_COEFFICIENTS_V,
    )
}

/// Converts a type-K equivalent voltage to junction temperature.
///
/// Voltages outside the fitted NIST range are linearly extrapolated with the
/// inverse spline's endpoint tangent.
///
/// Args:
///   voltage_v: Type-K equivalent electromotive-force scalar in `V`.
///
/// Returns:
///   Absolute junction temperature scalar in `K`.
///
/// References:
///   \[1\] G. W. Burns et al., NIST Monograph 175, 1993,
///   doi: 10.6028/NIST.MONO.175.
#[inline]
pub fn ktype_temperature_k_f32(voltage_v: f32) -> f32 {
    interpolate_regular_bspline(
        voltage_v,
        TC_SPLINE_MIN_VOLTAGE_V,
        TC_INVERSE_STEP_V,
        &TC_INVERSE_COEFFICIENTS_K,
    )
}

/// Applies ITS-90 cold-junction compensation to a type-K measurement.
///
/// The sensed voltage is added to the NIST-equivalent voltage of the cold
/// junction before the inverse curve is evaluated.
///
/// Args:
///   sensed_voltage_v: Measured hot-to-cold-junction electromotive-force scalar
///     in `V`.
///   cold_junction_temperature_k: Absolute connector temperature scalar in `K`.
///
/// Returns:
///   Compensated absolute hot-junction temperature scalar in `K`.
///
/// References:
///   \[1\] G. W. Burns et al., NIST Monograph 175, 1993,
///   doi: 10.6028/NIST.MONO.175.
#[inline]
pub fn ktype_corrected_temperature_k_f32(
    sensed_voltage_v: f32,
    cold_junction_temperature_k: f32,
) -> f32 {
    let cold_junction_voltage_v = ktype_voltage_v_f32(cold_junction_temperature_k);
    ktype_temperature_k_f32(sensed_voltage_v + cold_junction_voltage_v)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoints_and_linear_extrapolation_are_finite() {
        for temperature in [
            50.0,
            TC_SPLINE_MIN_TEMPERATURE_K,
            273.15,
            1_000.0,
            TC_SPLINE_MAX_TEMPERATURE_K + 50.0,
        ] {
            let voltage = ktype_voltage_v_f32(temperature);
            assert!(voltage.is_finite());
            assert!(ktype_temperature_k_f32(voltage).is_finite());
        }
    }

    #[test]
    fn roundtrip_stays_within_fitted_temperature_limit() {
        let mut max_error = 0.0_f32;
        let steps = ((TC_SPLINE_MAX_TEMPERATURE_K - TC_SPLINE_MIN_TEMPERATURE_K) * 100.0) as usize;
        for index in 0..=steps {
            let temperature = TC_SPLINE_MIN_TEMPERATURE_K + index as f32 / 100.0;
            let roundtrip = ktype_temperature_k_f32(ktype_voltage_v_f32(temperature));
            max_error = max_error.max((roundtrip - temperature).abs());
        }
        // The generator independently bounds each direction to 0.01 K.
        assert!(
            max_error <= 0.02,
            "maximum roundtrip error was {max_error} K"
        );
    }
}
