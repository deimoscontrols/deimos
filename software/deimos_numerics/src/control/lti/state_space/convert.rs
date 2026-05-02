//! Dense continuous/discrete conversion routines for state-space models.
//!
//! The public entry points in this file are intentionally method-driven rather
//! than assumption-driven. A `c2d` or `d2c` call is not meaningful unless the
//! caller states what happens between sample instants.
//!
//! # Glossary
//!
//! - **c2d / d2c:** Continuous-to-discrete and discrete-to-continuous
//!   conversion.
//! - **Zero-order hold:** Assumption that the input is piecewise constant over
//!   each sample interval.
//! - **Bilinear / Tustin conversion:** Frequency-warping continuous/discrete
//!   map based on the trapezoidal rule.

use super::domain::{ContinuousTime, DiscreteTime};
use super::error::StateSpaceError;
use super::{ContinuousStateSpace, DiscreteStateSpace, StateSpace};
use crate::control::dense_ops::{dense_mul, frobenius_norm};
use crate::sparse::compensated::CompensatedField;
use crate::twosum::TwoSum;
use faer::linalg::solvers::Solve;
use faer::{Mat, MatRef};
use faer_traits::ComplexField;
use faer_traits::ext::ComplexFieldExt;
use num_traits::{Float, NumCast, ToPrimitive};

/// Dense continuous-to-discrete conversion method.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum DiscretizationMethod<R> {
    /// Exact zero-order-hold conversion for piecewise-constant sampled inputs.
    ///
    /// This is the standard choice when the digital input is updated once per
    /// sample and then held constant by the actuator or DAC.
    ZeroOrderHold,
    /// Bilinear / Tustin conversion.
    ///
    /// The optional prewarp frequency lets the bilinear mapping match one
    /// chosen continuous-time frequency exactly. This is the common frequency-
    /// shaping conversion used in digital filter and controller design.
    Bilinear {
        /// Optional angular frequency to preserve exactly under the bilinear map.
        prewarp_frequency: Option<R>,
    },
}

/// Dense discrete-to-continuous conversion method.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ContinuousizationMethod<R> {
    /// Exact zero-order-hold reconstruction.
    ///
    /// This requires a matrix logarithm of the lifted discrete-time system and
    /// is intentionally left unsupported by this implementation.
    ZeroOrderHold,
    /// Bilinear / Tustin inversion.
    ///
    /// This is the inverse of the bilinear map under the same optional
    /// prewarping choice.
    Bilinear {
        /// Optional angular frequency that was used to prewarp the bilinear map.
        prewarp_frequency: Option<R>,
    },
}

pub(super) fn discretize<T>(
    system: &ContinuousStateSpace<T>,
    sample_time: T::Real,
    method: DiscretizationMethod<T::Real>,
) -> Result<DiscreteStateSpace<T>, StateSpaceError>
where
    T: CompensatedField,
    T::Real: Float,
{
    validate_sample_time(sample_time)?;
    match method {
        DiscretizationMethod::ZeroOrderHold => discretize_zoh(system, sample_time),
        DiscretizationMethod::Bilinear { prewarp_frequency } => {
            discretize_bilinear(system, sample_time, prewarp_frequency)
        }
    }
}

pub(super) fn continuousize<T>(
    system: &DiscreteStateSpace<T>,
    method: ContinuousizationMethod<T::Real>,
) -> Result<ContinuousStateSpace<T>, StateSpaceError>
where
    T: CompensatedField,
    T::Real: Float,
{
    match method {
        ContinuousizationMethod::ZeroOrderHold => Err(StateSpaceError::UnsupportedConversion(
            // Exact ZOH `d2c` is mathematically valid, but it is a matrix-log
            // feature, not a small variant of the dense linear solves already
            // implemented here.
            "exact ZOH d2c requires a matrix logarithm and is not implemented yet",
        )),
        ContinuousizationMethod::Bilinear { prewarp_frequency } => {
            continuousize_bilinear(system, prewarp_frequency)
        }
    }
}

fn discretize_zoh<T>(
    system: &ContinuousStateSpace<T>,
    sample_time: T::Real,
) -> Result<DiscreteStateSpace<T>, StateSpaceError>
where
    T: CompensatedField,
    T::Real: Float,
{
    let n = system.nstates();
    let nu = system.ninputs();
    let size = n + nu;

    // Use the standard lifted exponential:
    //
    // exp(dt * [A B; 0 0]) = [Ad Bd; 0 I]
    //
    // This is exact for inputs that are held constant across each sample
    // interval.
    let mut lifted = Mat::<T>::zeros(size, size);
    for row in 0..n {
        for col in 0..n {
            lifted[(row, col)] = system.a[(row, col)].mul_real(sample_time);
        }
    }
    for row in 0..n {
        for col in 0..nu {
            lifted[(row, n + col)] = system.b[(row, col)].mul_real(sample_time);
        }
    }

    let exp_lifted = matrix_exponential(lifted.as_ref())?;
    let ad = Mat::from_fn(n, n, |row, col| exp_lifted[(row, col)]);
    let bd = Mat::from_fn(n, nu, |row, col| exp_lifted[(row, n + col)]);

    Ok(StateSpace {
        a: ad,
        b: bd,
        c: system.c.clone(),
        d: system.d.clone(),
        domain: DiscreteTime::new(sample_time),
    })
}

fn discretize_bilinear<T>(
    system: &ContinuousStateSpace<T>,
    sample_time: T::Real,
    prewarp_frequency: Option<T::Real>,
) -> Result<DiscreteStateSpace<T>, StateSpaceError>
where
    T: CompensatedField,
    T::Real: Float,
{
    let alpha = bilinear_alpha(sample_time, prewarp_frequency)?;
    let gamma = (alpha + alpha).sqrt();
    let n = system.nstates();

    let identity = Mat::<T>::identity(n, n);
    // The bilinear map is
    //
    // Ad = (I - alpha A)^(-1) (I + alpha A)
    //
    // with matching input/output transformations so the full state-space
    // quadruple follows the same trapezoidal-rule change of variables.
    let p = &identity - dense_scale_real(system.a.as_ref(), alpha).as_ref();
    let q = &identity + dense_scale_real(system.a.as_ref(), alpha).as_ref();
    let p_inv = inverse_checked(p.as_ref(), "I - alpha A")?;

    let ad = dense_mul(p_inv.as_ref(), q.as_ref());
    let bd = dense_scale_real(dense_mul(p_inv.as_ref(), system.b.as_ref()).as_ref(), gamma);
    let c_p_inv = dense_mul(system.c.as_ref(), p_inv.as_ref());
    let cd = dense_scale_real(c_p_inv.as_ref(), gamma);
    let d_corr = dense_scale_real(
        dense_mul(c_p_inv.as_ref(), system.b.as_ref()).as_ref(),
        alpha,
    );
    let dd = system.d.as_ref() + &d_corr;

    Ok(StateSpace {
        a: ad,
        b: bd,
        c: cd,
        d: dd,
        domain: DiscreteTime::new(sample_time),
    })
}

fn continuousize_bilinear<T>(
    system: &DiscreteStateSpace<T>,
    prewarp_frequency: Option<T::Real>,
) -> Result<ContinuousStateSpace<T>, StateSpaceError>
where
    T: CompensatedField,
    T::Real: Float,
{
    let sample_time = system.sample_time();
    let alpha = bilinear_alpha(sample_time, prewarp_frequency)?;
    let gamma = (alpha + alpha).sqrt();
    let n = system.nstates();

    let identity = Mat::<T>::identity(n, n);
    let ap = system.a.as_ref() + &identity;
    let am = system.a.as_ref() - &identity;
    let ap_inv = inverse_checked(ap.as_ref(), "Ad + I")?;
    let bc_scale = (gamma + gamma) / (gamma * gamma);

    // Inverting the bilinear map gives
    //
    // A = alpha^(-1) (Ad - I) (Ad + I)^(-1)
    //
    // with matching `B` and `C` scalings chosen so a forward bilinear
    // discretization round-trips back to the original continuous model.
    let a = dense_scale_real(
        dense_mul(am.as_ref(), ap_inv.as_ref()).as_ref(),
        alpha.recip(),
    );
    let b = dense_scale_real(
        dense_mul(ap_inv.as_ref(), system.b.as_ref()).as_ref(),
        bc_scale,
    );
    let c_ap_inv = dense_mul(system.c.as_ref(), ap_inv.as_ref());
    let c = dense_scale_real(c_ap_inv.as_ref(), bc_scale);
    let d_corr = dense_mul(c_ap_inv.as_ref(), system.b.as_ref());
    let d = system.d.as_ref() - &d_corr;

    Ok(StateSpace {
        a,
        b,
        c,
        d,
        domain: ContinuousTime,
    })
}

/// Validates the common positive finite sample-time invariant shared by all
/// explicit continuous/discrete conversion methods.
fn validate_sample_time<R: Float>(sample_time: R) -> Result<(), StateSpaceError> {
    if !sample_time.is_finite() || sample_time <= R::zero() {
        return Err(StateSpaceError::InvalidSampleTime);
    }
    Ok(())
}

/// Computes the bilinear/Tustin scaling parameter.
///
/// Without prewarping this is just `dt / 2`. With prewarping it becomes the
/// modified scale that preserves one chosen continuous-time frequency exactly
/// under the bilinear map.
fn bilinear_alpha<R: Float>(
    sample_time: R,
    prewarp_frequency: Option<R>,
) -> Result<R, StateSpaceError> {
    validate_sample_time(sample_time)?;
    match prewarp_frequency {
        None => Ok(sample_time / (R::one() + R::one())),
        Some(w) if !w.is_finite() || w < R::zero() => Err(StateSpaceError::InvalidPrewarpFrequency),
        Some(w) if w == R::zero() => Ok(sample_time / (R::one() + R::one())),
        Some(w) => {
            // Prewarping changes the bilinear scaling so one chosen continuous
            // frequency is matched exactly after discretization.
            let half = sample_time / (R::one() + R::one());
            let alpha = (w * half).tan() / w;
            if !alpha.is_finite() || alpha <= R::zero() {
                return Err(StateSpaceError::InvalidPrewarpFrequency);
            }
            Ok(alpha)
        }
    }
}

/// Dense reference matrix exponential used by state-space conversions and
/// continuous-time response evaluation.
///
/// This is intentionally `pub(crate)` so higher-level LTI analysis code can
/// reuse the same dense scaling-and-squaring implementation instead of growing
/// a second copy of the same numerical kernel.
pub(crate) fn matrix_exponential<T>(matrix: MatRef<'_, T>) -> Result<Mat<T>, StateSpaceError>
where
    T: CompensatedField,
    T::Real: Float,
{
    if matrix.nrows() != matrix.ncols() {
        return Err(StateSpaceError::DimensionMismatch {
            which: "matrix_exponential.matrix",
            expected_nrows: matrix.ncols(),
            expected_ncols: matrix.ncols(),
            actual_nrows: matrix.nrows(),
            actual_ncols: matrix.ncols(),
        });
    }

    let n = matrix.nrows();
    if n == 0 {
        return Ok(Mat::zeros(0, 0));
    }

    let theta13 = <T::Real as NumCast>::from(5.371_920_351_148_152f64).unwrap();
    let norm1 = matrix_one_norm(matrix);
    let s = if norm1 > theta13 {
        (norm1 / theta13).log2().ceil().to_u32().unwrap_or(u32::MAX)
    } else {
        0
    };
    let scale =
        <T::Real as NumCast>::from(2.0f64.powi(i32::try_from(s).unwrap_or(i32::MAX))).unwrap();
    let a = dense_scale_real(matrix, scale.recip());

    // Pade-13 scaling-and-squaring is the standard dense reference choice for
    // a general matrix exponential. It gives a good accuracy/cost tradeoff for
    // the modest state dimensions this first dense control layer targets.
    let a2 = dense_mul(a.as_ref(), a.as_ref());
    let a4 = dense_mul(a2.as_ref(), a2.as_ref());
    let a6 = dense_mul(a2.as_ref(), a4.as_ref());
    let ident = Mat::<T>::identity(n, n);

    let b = pade13_coeffs::<T::Real>();

    let mut tmp = dense_scale_real(a6.as_ref(), b[13]);
    dense_axpy_real(&mut tmp, b[11], a4.as_ref());
    dense_axpy_real(&mut tmp, b[9], a2.as_ref());

    let mut u_inner = dense_mul(a6.as_ref(), tmp.as_ref());
    dense_axpy_real(&mut u_inner, b[7], a6.as_ref());
    dense_axpy_real(&mut u_inner, b[5], a4.as_ref());
    dense_axpy_real(&mut u_inner, b[3], a2.as_ref());
    dense_axpy_real(&mut u_inner, b[1], ident.as_ref());
    let u = dense_mul(a.as_ref(), u_inner.as_ref());

    let mut tmp = dense_scale_real(a6.as_ref(), b[12]);
    dense_axpy_real(&mut tmp, b[10], a4.as_ref());
    dense_axpy_real(&mut tmp, b[8], a2.as_ref());
    let mut v = dense_mul(a6.as_ref(), tmp.as_ref());
    dense_axpy_real(&mut v, b[6], a6.as_ref());
    dense_axpy_real(&mut v, b[4], a4.as_ref());
    dense_axpy_real(&mut v, b[2], a2.as_ref());
    dense_axpy_real(&mut v, b[0], ident.as_ref());

    let p = &v - &u;
    let q = &v + &u;
    let mut result = solve_left_checked(p.as_ref(), q.as_ref(), "matrix exponential solve")?;

    for _ in 0..s {
        result = dense_mul(result.as_ref(), result.as_ref());
    }

    if !result.as_ref().is_all_finite() {
        return Err(StateSpaceError::NonFiniteResult {
            which: "matrix_exponential",
        });
    }
    Ok(result)
}

/// Returns the fixed Pade-13 coefficients used by the dense scaling-and-
/// squaring matrix exponential.
///
/// Keeping these in one helper makes the exponential implementation easier to
/// audit against the standard algorithm.
fn pade13_coeffs<R: Float>() -> [R; 14] {
    [
        R::from(64_764_752_532_480_000.0).unwrap(),
        R::from(32_382_376_266_240_000.0).unwrap(),
        R::from(7_771_770_303_897_600.0).unwrap(),
        R::from(1_187_353_796_428_800.0).unwrap(),
        R::from(129_060_195_264_000.0).unwrap(),
        R::from(10_559_470_521_600.0).unwrap(),
        R::from(670_442_572_800.0).unwrap(),
        R::from(33_522_128_640.0).unwrap(),
        R::from(1_323_241_920.0).unwrap(),
        R::from(40_840_800.0).unwrap(),
        R::from(960_960.0).unwrap(),
        R::from(16_380.0).unwrap(),
        R::from(182.0).unwrap(),
        R::one(),
    ]
}

/// Computes a checked dense inverse by solving against the identity.
///
/// This keeps all inversion logic going through the same residual-checked solve
/// path, so conversion formulas fail explicitly on singular or numerically
/// unusable matrices.
fn inverse_checked<T>(matrix: MatRef<'_, T>, which: &'static str) -> Result<Mat<T>, StateSpaceError>
where
    T: CompensatedField,
    T::Real: Float,
{
    // Route inversion through a checked solve against the identity so singular
    // or nearly singular conversion formulas fail explicitly instead of
    // quietly returning nonsense.
    solve_left_checked(
        matrix,
        Mat::<T>::identity(matrix.nrows(), matrix.ncols()).as_ref(),
        which,
    )
}

/// Solves `lhs * X = rhs` and rejects non-finite or numerically poor results.
///
/// The dense conversion formulas rely on inverses of matrices such as
/// `I - α A`, `I + α A`, and the linear systems inside the matrix exponential.
/// A bare LU solve can return a finite-looking answer even when the problem is
/// too ill-conditioned for the result to be trusted, so this helper performs an
/// explicit residual check before accepting the solve.
fn solve_left_checked<T>(
    lhs: MatRef<'_, T>,
    rhs: MatRef<'_, T>,
    which: &'static str,
) -> Result<Mat<T>, StateSpaceError>
where
    T: CompensatedField,
    T::Real: Float,
{
    let sol = lhs.full_piv_lu().solve(rhs);
    if !sol.as_ref().is_all_finite() {
        return Err(StateSpaceError::SingularConversion { which });
    }

    // Verify the solve explicitly. Conversion formulas are sensitive to bad
    // inverses, so a non-finite or wildly inaccurate solve should be surfaced
    // as a conversion failure, not silently accepted.
    let residual = dense_mul(lhs, sol.as_ref()) - rhs;
    let residual_norm = frobenius_norm(residual.as_ref());
    let scale = frobenius_norm(lhs) * frobenius_norm(sol.as_ref())
        + frobenius_norm(rhs)
        + <T::Real as num_traits::One>::one();
    let tol = <T::Real as NumCast>::from(64.0).unwrap() * T::Real::epsilon().sqrt() * scale;
    if !residual_norm.is_finite() || residual_norm > tol {
        return Err(StateSpaceError::SingularConversion { which });
    }

    Ok(sol)
}

/// Adds `alpha * src` into `dst` in place.
///
/// This is the small dense equivalent of the BLAS `axpy` pattern and is used
/// heavily in the Pade polynomial assembly for the matrix exponential.
fn dense_axpy_real<T>(dst: &mut Mat<T>, alpha: T::Real, src: MatRef<'_, T>)
where
    T: ComplexField + Copy,
{
    assert_eq!(dst.nrows(), src.nrows());
    assert_eq!(dst.ncols(), src.ncols());
    for col in 0..dst.ncols() {
        for row in 0..dst.nrows() {
            dst[(row, col)] += src[(row, col)].mul_real(&alpha);
        }
    }
}

/// Scales a dense matrix by a real scalar.
///
/// The helper keeps the call sites in the conversion formulas readable and
/// avoids open-coding the same elementwise loop around `mul_real`.
fn dense_scale_real<T>(matrix: MatRef<'_, T>, alpha: T::Real) -> Mat<T>
where
    T: ComplexField + Copy,
{
    Mat::from_fn(matrix.nrows(), matrix.ncols(), |row, col| {
        matrix[(row, col)].mul_real(&alpha)
    })
}

/// Returns the matrix one-norm `max_j sum_i |a_ij|`.
///
/// The scaling-and-squaring exponential uses this as its cheap norm estimate
/// when deciding how aggressively to scale the input before the Pade step.
fn matrix_one_norm<T>(matrix: MatRef<'_, T>) -> T::Real
where
    T: CompensatedField,
    T::Real: Float,
{
    let mut max_norm = <T::Real as num_traits::Zero>::zero();
    for col in 0..matrix.ncols() {
        let mut acc: Option<TwoSum<T::Real>> = None;
        for row in 0..matrix.nrows() {
            let value = matrix[(row, col)].abs1();
            match acc.as_mut() {
                Some(acc) => acc.add(value),
                None => acc = Some(TwoSum::new(value)),
            }
        }
        let col_sum = match acc {
            Some(acc) => {
                let (sum, residual) = acc.finish();
                sum + residual
            }
            None => <T::Real as num_traits::Zero>::zero(),
        };
        if col_sum > max_norm {
            max_norm = col_sum;
        }
    }
    max_norm
}
