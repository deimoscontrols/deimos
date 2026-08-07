//! Compact, allocation-free engineering calculations shared by firmware and software.
//!
//! All real-time evaluators use `f32`. Array arguments document their shapes
//! using the `(rows, columns)` convention used by `deimos_numerics`; scalar
//! values are identified explicitly. Approximation coefficients are generated
//! and validated by `scripts/fit_calcs.py`.

use interpn::MultiBsplineRegular;

pub mod rtd_pt100;
pub mod tc_ktype;

pub use rtd_pt100::*;
pub use tc_ktype::*;

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
