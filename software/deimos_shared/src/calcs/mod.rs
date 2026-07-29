//! Compact, allocation-free engineering calculations shared by firmware and software.
//!
//! All real-time evaluators use `f32`. Array arguments document their shapes
//! using the `(rows, columns)` convention used by `deimos_numerics`; scalar
//! values are identified explicitly. Approximation coefficients are generated
//! and validated by `scripts/fit_calcs.py`.

pub mod rtd_pt100;
pub mod tc_ktype;

pub use rtd_pt100::*;
pub use tc_ktype::*;
