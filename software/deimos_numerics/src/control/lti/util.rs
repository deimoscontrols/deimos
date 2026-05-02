use super::error::LtiError;
use super::{ContinuousTime, DiscreteTime};
use crate::decomp::dense_eigenvalues;
use crate::scalar::{complex_horner_step_real, mul_add};
use alloc::vec::Vec;
use faer::Mat;
use faer::complex::Complex;
use faer_traits::RealField;
use faer_traits::ext::ComplexFieldExt;
use faer_traits::math_utils::{eps, from_f64};
use num_traits::{Float, NumCast};

/// Domain metadata that can validate whether two LTI representations may be
/// composed directly.
#[doc(hidden)]
pub trait CompositionDomain<R>: Clone {
    /// Returns the domain metadata carried by the result of a binary
    /// composition.
    ///
    /// This is intentionally a small internal-facing hook rather than a full
    /// public extension point. The arithmetic layer only needs one question
    /// answered here: "may these two objects participate in the same
    /// composition, and if so what domain metadata should the result carry?"
    fn composed(lhs: &Self, rhs: &Self) -> Result<Self, LtiError>;
}

impl<R> CompositionDomain<R> for ContinuousTime
where
    R: Float + RealField,
{
    fn composed(lhs: &Self, _rhs: &Self) -> Result<Self, LtiError> {
        Ok(*lhs)
    }
}

impl<R> CompositionDomain<R> for DiscreteTime<R>
where
    R: Float + RealField,
{
    fn composed(lhs: &Self, rhs: &Self) -> Result<Self, LtiError> {
        if sample_times_match(lhs.sample_time(), rhs.sample_time()) {
            Ok(*lhs)
        } else {
            Err(LtiError::MismatchedSampleTime)
        }
    }
}

/// Validates the positive finite sample-time invariant shared by discrete LTI
/// representations.
pub(crate) fn validate_sample_time<R: Float + RealField>(sample_time: R) -> Result<(), LtiError> {
    if sample_time.is_finite() && sample_time > R::zero() {
        Ok(())
    } else {
        Err(LtiError::InvalidSampleTime)
    }
}

/// Casts one real scalar into another and annotates failures with an LTI field
/// name.
pub(crate) fn cast_real_scalar<R, S>(value: R, which: &'static str) -> Result<S, LtiError>
where
    R: NumCast + Copy,
    S: NumCast,
{
    NumCast::from(value).ok_or(LtiError::ScalarConversionFailed { which })
}

/// Validates a finite, nonnegative, monotone nondecreasing numeric grid.
///
/// This is shared by the phase-unwrapped frequency-domain helpers and any
/// time-domain API that interprets the supplied points as absolute times from
/// a causal origin.
pub(crate) fn validate_nonnegative_monotone_grid<R: Float + RealField>(
    sample_points: &[R],
    which: &'static str,
) -> Result<(), LtiError> {
    for &sample in sample_points {
        if !sample.is_finite() || sample < R::zero() {
            return Err(LtiError::InvalidSamplePoint { which });
        }
    }
    if sample_points.windows(2).any(|window| window[1] < window[0]) {
        return Err(LtiError::InvalidSampleGrid { which });
    }
    Ok(())
}

/// Drops leading zeros from a real polynomial coefficient vector.
///
/// The polynomial utilities normalize coefficients to descending-power form
/// without redundant leading zeros so degree calculations stay well-defined.
pub(crate) fn trim_leading_zeros<R: Float + RealField>(coeffs: &[R]) -> Vec<R> {
    let first_nz = coeffs.iter().position(|&value| value != R::zero());
    match first_nz {
        Some(idx) => coeffs[idx..].to_vec(),
        None => vec![R::zero()],
    }
}

/// Normalizes a rational polynomial pair so the denominator is monic.
///
/// This gives transfer-function coefficient form a stable canonicalization
/// rule, which simplifies equality checks and conversion round trips.
pub(crate) fn normalize_ratio<R: Float + RealField>(
    numerator: &[R],
    denominator: &[R],
) -> Result<(Vec<R>, Vec<R>), LtiError> {
    if numerator.iter().any(|value| !value.is_finite())
        || denominator.iter().any(|value| !value.is_finite())
    {
        return Err(LtiError::NonFiniteResult {
            which: "normalize_ratio",
        });
    }
    let numerator = trim_leading_zeros(numerator);
    let denominator = trim_leading_zeros(denominator);
    if numerator.is_empty() {
        return Err(LtiError::EmptyPolynomial { which: "numerator" });
    }
    if denominator.is_empty() {
        return Err(LtiError::EmptyPolynomial {
            which: "denominator",
        });
    }
    let leading = denominator[0];
    if leading == R::zero() {
        return Err(LtiError::ZeroLeadingCoefficient {
            which: "denominator",
        });
    }
    let scale = leading.recip();
    Ok((
        numerator.into_iter().map(|value| value * scale).collect(),
        denominator.into_iter().map(|value| value * scale).collect(),
    ))
}

/// Evaluates a real-coefficient polynomial at a complex point via Horner's
/// method.
pub(crate) fn poly_eval<R: Float + RealField>(coeffs: &[R], point: Complex<R>) -> Complex<R> {
    coeffs
        .iter()
        .fold(Complex::new(R::zero(), R::zero()), |acc, &coef| {
            complex_horner_step_real(acc, point, coef)
        })
}

/// Multiplies two real-coefficient polynomials in descending-power form.
pub(crate) fn poly_mul<R: Float + RealField>(lhs: &[R], rhs: &[R]) -> Vec<R> {
    let mut out = vec![R::zero(); lhs.len() + rhs.len() - 1];
    for (i, &lhs_value) in lhs.iter().enumerate() {
        for (j, &rhs_value) in rhs.iter().enumerate() {
            out[i + j] = mul_add(lhs_value, rhs_value, out[i + j]);
        }
    }
    trim_leading_zeros(&out)
}

/// Adds two descending-power polynomials after aligning them by degree.
pub(crate) fn poly_add_aligned<R: Float + RealField>(lhs: &[R], rhs: &[R]) -> Vec<R> {
    poly_addsub_aligned(lhs, rhs, false)
}

/// Subtracts two descending-power polynomials after aligning them by degree.
pub(crate) fn poly_sub_aligned<R: Float + RealField>(lhs: &[R], rhs: &[R]) -> Vec<R> {
    poly_addsub_aligned(lhs, rhs, true)
}

/// Returns whether a polynomial is identically zero.
pub(crate) fn is_zero_polynomial<R: Float + RealField>(coeffs: &[R]) -> bool {
    coeffs.iter().all(|&value| value == R::zero())
}

/// Checks whether two discrete sample intervals agree to within a small
/// relative/absolute tolerance.
///
/// Exact floating equality would make otherwise reasonable discrete-time
/// compositions fail after round-trips through conversions or arithmetic. The
/// tolerance here is intentionally small: it treats clearly different sample
/// times as incompatible while still allowing numerically harmless drift.
pub(crate) fn sample_times_match<R: Float + RealField>(lhs: R, rhs: R) -> bool {
    let scale = R::one().max(lhs.abs()).max(rhs.abs());
    let tol = from_f64::<R>(128.0) * eps::<R>() * scale;
    (lhs - rhs).abs() <= tol
}

/// Unwraps a phase trace expressed in degrees.
///
/// Both the plotting and loop-analysis layers start from pointwise
/// `atan2`-based phase samples, which naturally live in `[-180, 180]`.
/// Unwrapping preserves local continuity across those branch cuts so later
/// callers can reason about trends, slopes, and `-180 deg` crossings on the
/// supplied grid.
pub(crate) fn unwrap_phase_deg<R>(wrapped: &[R]) -> Vec<R>
where
    R: Float + RealField,
{
    if wrapped.is_empty() {
        return Vec::new();
    }

    let full_turn = from_f64::<R>(360.0);
    let half_turn = from_f64::<R>(180.0);
    let mut out = Vec::with_capacity(wrapped.len());
    out.push(wrapped[0]);
    for &phase in &wrapped[1..] {
        let mut adjusted = phase;
        let prev = *out.last().unwrap();
        while adjusted - prev > half_turn {
            adjusted -= full_turn;
        }
        while adjusted - prev < -half_turn {
            adjusted += full_turn;
        }
        out.push(adjusted);
    }
    out
}

/// Computes the roots of a real-coefficient polynomial through its companion
/// matrix.
///
/// This is the dense reference implementation used by the first TF/ZPK/SOS
/// conversion layer. It is appropriate for modest SISO polynomials and keeps
/// the implementation on top of already-available dense eigenvalue routines.
pub(crate) fn poly_roots<R: Float + RealField>(coeffs: &[R]) -> Result<Vec<Complex<R>>, LtiError> {
    let coeffs = trim_leading_zeros(coeffs);
    if coeffs.is_empty() {
        return Err(LtiError::EmptyPolynomial {
            which: "polynomial",
        });
    }
    if coeffs[0] == R::zero() {
        return Err(LtiError::ZeroLeadingCoefficient {
            which: "polynomial",
        });
    }
    if coeffs.len() == 1 {
        return Ok(Vec::new());
    }

    let lead_inv = coeffs[0].recip();
    let degree = coeffs.len() - 1;
    let mut companion = Mat::<R>::zeros(degree, degree);
    for row in 1..degree {
        companion[(row, row - 1)] = R::one();
    }
    for row in 0..degree {
        companion[(row, degree - 1)] = -(coeffs[degree - row] * lead_inv);
    }
    let mut roots = dense_eigenvalues(companion.as_ref())?
        .try_as_col_major()
        .unwrap()
        .as_slice()
        .to_vec();
    roots.sort_by(compare_roots);
    Ok(roots)
}

/// Rebuilds a real polynomial from a root list.
///
/// The result is only accepted when the coefficients are numerically real,
/// which in practice means the supplied roots are closed under complex
/// conjugation to within tolerance.
pub(crate) fn real_poly_from_roots<R: Float + RealField>(
    roots: &[Complex<R>],
    which: &'static str,
) -> Result<Vec<R>, LtiError> {
    let mut coeffs = vec![Complex::new(R::one(), R::zero())];
    for &root in roots {
        let mut next = vec![Complex::new(R::zero(), R::zero()); coeffs.len() + 1];
        for (i, &coef) in coeffs.iter().enumerate() {
            next[i] += coef;
            next[i + 1] -= coef * root;
        }
        coeffs = next;
    }
    complex_coeffs_to_real(&coeffs, which)
}

/// Groups roots into first- or second-order real polynomial sections.
///
/// Real roots are paired into quadratic sections where possible so discrete
/// SOS cascades stay sectionwise proper. Any unpaired real root becomes a
/// padded first-order section. Complex roots must appear in conjugate pairs so
/// each section still has real coefficients.
///
/// The padded first-order convention is:
///
/// `s - r` or `z - r`  ->  `[0, 1, -r]`
///
/// rather than `[1, -r, 0]`, which would represent `s^2 - r s`.
pub(crate) fn root_sections<R: Float + RealField>(
    roots: &[Complex<R>],
    which: &'static str,
) -> Result<Vec<[R; 3]>, LtiError> {
    let mut roots = roots.to_vec();
    roots.sort_by(compare_roots);

    let mut used = vec![false; roots.len()];
    let mut sections = Vec::new();
    let tol = root_tol(&roots);

    for i in 0..roots.len() {
        if used[i] {
            continue;
        }

        let root = roots[i];
        used[i] = true;

        if root.im.abs() <= tol {
            let mut partner = None;
            for j in (i + 1)..roots.len() {
                if !used[j] && roots[j].im.abs() <= tol {
                    partner = Some(j);
                    break;
                }
            }

            if let Some(j) = partner {
                used[j] = true;
                let other = roots[j];
                sections.push([R::one(), -(root.re + other.re), root.re * other.re]);
            } else {
                sections.push([R::zero(), R::one(), -root.re]);
            }
            continue;
        }

        let conj = Complex::new(root.re, -root.im);
        let mut partner = None;
        for j in (i + 1)..roots.len() {
            if !used[j] && (roots[j] - conj).abs() <= tol {
                partner = Some(j);
                break;
            }
        }
        let Some(j) = partner else {
            return Err(LtiError::NotConjugateClosed { which });
        };
        used[j] = true;
        let pair_sum = root + roots[j];
        let pair_prod = root * roots[j];
        sections.push([R::one(), -pair_sum.re, pair_prod.re]);
    }

    Ok(sections)
}

/// Returns the multiplicative identity polynomial `1` in section storage.
///
/// This is used to pad the shorter side when a ZPK representation has more
/// pole than zero sections or vice versa.
///
/// In descending-power section storage, the identity is `[0, 0, 1]`, not
/// `[1, 0, 0]`.
pub(crate) fn identity_section<R: Float + RealField>() -> [R; 3] {
    [R::zero(), R::zero(), R::one()]
}

/// Converts numerically real complex coefficients back into a real vector.
///
/// A small tolerance is allowed so roundoff from root reconstruction does not
/// falsely reject an otherwise valid conjugate-closed polynomial.
fn complex_coeffs_to_real<R: Float + RealField>(
    coeffs: &[Complex<R>],
    which: &'static str,
) -> Result<Vec<R>, LtiError> {
    let tol = coeff_tol(coeffs);
    let mut out = Vec::with_capacity(coeffs.len());
    for &coef in coeffs {
        if coef.im.abs() > tol {
            return Err(LtiError::NotConjugateClosed { which });
        }
        out.push(coef.re);
    }
    Ok(trim_leading_zeros(&out))
}

/// Heuristic matching tolerance for pairing complex-conjugate roots.
fn root_tol<R: Float + RealField>(roots: &[Complex<R>]) -> R {
    let scale = roots
        .iter()
        .fold(R::one(), |acc: R, root: &Complex<R>| acc.max(root.abs()));
    scale * from_f64::<R>(128.0) * eps::<R>()
}

/// Heuristic tolerance for deciding whether reconstructed coefficients are
/// numerically real.
fn coeff_tol<R: Float + RealField>(coeffs: &[Complex<R>]) -> R {
    let scale = coeffs
        .iter()
        .fold(R::one(), |acc: R, coeff: &Complex<R>| acc.max(coeff.abs()));
    scale * from_f64::<R>(128.0) * eps::<R>()
}

/// Stable sort order for roots: descending magnitude, then real part, then
/// imaginary part.
///
/// This keeps conversions and tests deterministic without imposing a stronger
/// mathematical meaning on root ordering than the underlying eigenvalue solve
/// actually provides.
fn compare_roots<R: Float + RealField>(lhs: &Complex<R>, rhs: &Complex<R>) -> core::cmp::Ordering {
    rhs.abs()
        .partial_cmp(&lhs.abs())
        .unwrap_or(core::cmp::Ordering::Equal)
        .then_with(|| {
            rhs.re
                .partial_cmp(&lhs.re)
                .unwrap_or(core::cmp::Ordering::Equal)
        })
        .then_with(|| {
            rhs.im
                .partial_cmp(&lhs.im)
                .unwrap_or(core::cmp::Ordering::Equal)
        })
}

/// Shared implementation for aligned polynomial addition and subtraction.
fn poly_addsub_aligned<R: Float + RealField>(lhs: &[R], rhs: &[R], subtract_rhs: bool) -> Vec<R> {
    // The coefficient vectors are stored in descending-power order, so aligning
    // by degree means right-aligning the shorter vector before combining.
    let len = lhs.len().max(rhs.len());
    let lhs_offset = len - lhs.len();
    let rhs_offset = len - rhs.len();
    let mut out = vec![R::zero(); len];

    for (idx, &value) in lhs.iter().enumerate() {
        out[lhs_offset + idx] += value;
    }
    for (idx, &value) in rhs.iter().enumerate() {
        let value = if subtract_rhs { -value } else { value };
        out[rhs_offset + idx] += value;
    }

    trim_leading_zeros(&out)
}
