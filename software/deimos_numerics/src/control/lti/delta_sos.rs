//! Delta-operator runtime form for discrete SOS filters.
//!
//! The design path in this crate already produces ordinary second-order
//! sections in powers of `z^-1`. That is the right storage format for most
//! uses, but very low normalized cutoffs push poles close to `z = 1`, which
//! makes direct-form section coefficients look like tiny perturbations of
//! `[1, -2, 1]`.
//!
//! This module keeps the same section factorization and converts each section
//! into a forward-delta state update. That shifts the numerically important
//! quantities into explicit small parameters such as `alpha0` and `alpha1`
//! instead of recovering them from subtraction against large fixed constants.
//!
//! `DeltaSos` is therefore a derived discrete execution representation, not a
//! replacement for ordinary [`DiscreteSos`](super::DiscreteSos). Design,
//! storage, conversion, and section algebra still flow through the ordinary SOS
//! type, with delta-SOS introduced only when the runtime basis matters.
//!
//! # Glossary
//!
//! - **Delta operator:** The discrete operator `δ = (1 - z^-1) / dt`.
//! - **Low normalized cutoff:** A digital cutoff frequency close to DC
//!   relative to the sample rate.

use super::error::LtiError;
use super::util::{cast_real_scalar, trim_leading_zeros, validate_sample_time};
use super::{DiscreteSos, DiscreteTime};
use alloc::vec::Vec;
use faer::complex::Complex;
use faer_traits::RealField;
use num_traits::{Float, NumCast};

/// One section in a delta-operator SOS runtime cascade.
///
/// This is a discrete-time execution representation derived from an ordinary
/// SOS factorization, but expressed in the basis
///
/// `δ = (1 - z^-1) / dt`
///
/// so low-cutoff poles near `z = 1` are represented through moderate delta
/// coefficients instead of tiny deviations from `2` and `1` in ordinary
/// `z^-1` denominator form.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum DeltaSection<R> {
    /// Pure direct-feedthrough section.
    Direct { d: R },
    /// First-order delta section:
    ///
    /// `δ x = -alpha0 x + u`
    ///
    /// `y   = c0 x + d u`
    First { alpha0: R, c0: R, d: R },
    /// Second-order delta section:
    ///
    /// `δ x1 = x2`
    ///
    /// `δ x2 = -alpha0 x1 - alpha1 x2 + u`
    ///
    /// `y    = c1 x1 + c2 x2 + d u`
    Second {
        alpha0: R,
        alpha1: R,
        c1: R,
        c2: R,
        d: R,
    },
}

/// Discrete-time delta-operator second-order-section cascade.
///
/// This stores the same transfer map as an ordinary [`DiscreteSos`], but in a
/// runtime-oriented basis that is better conditioned for low normalized
/// cutoffs. It is intended to be derived from ordinary SOS storage when
/// execution conditioning matters, not used as the primary design or algebra
/// representation.
#[derive(Clone, Debug, PartialEq)]
pub struct DeltaSos<R> {
    sections: Vec<DeltaSection<R>>,
    gain: R,
    domain: DiscreteTime<R>,
}

impl<R> DeltaSos<R>
where
    R: Float + RealField,
{
    /// Creates a discrete-time delta-operator cascade.
    ///
    /// The stored sections are already in execution form. No additional
    /// normalization or section synthesis is performed here beyond validating
    /// the sample time and rejecting an empty cascade. Callers that start from
    /// an ordinary IIR design should usually prefer
    /// [`DiscreteSos::to_delta_sos`](super::DiscreteSos::to_delta_sos) over
    /// constructing `DeltaSos` sections by hand.
    pub fn new(
        sections: impl Into<Vec<DeltaSection<R>>>,
        gain: R,
        sample_time: R,
    ) -> Result<Self, LtiError> {
        validate_sample_time(sample_time)?;
        let sections = sections.into();
        if sections.is_empty() {
            return Err(LtiError::EmptySos);
        }
        Ok(Self {
            sections,
            gain,
            domain: DiscreteTime::new(sample_time),
        })
    }

    /// Delta sections in cascade order.
    #[must_use]
    pub fn sections(&self) -> &[DeltaSection<R>] {
        &self.sections
    }

    /// Overall gain applied ahead of the section cascade.
    #[must_use]
    pub fn gain(&self) -> R {
        self.gain
    }

    /// Sample interval carried by the discrete-time representation.
    #[must_use]
    pub fn sample_time(&self) -> R {
        self.domain.sample_time()
    }

    /// Returns the steady-state gain `G(1)`.
    ///
    /// In the delta basis, discrete-time DC corresponds to `δ = 0`. The value
    /// returned here therefore matches the input/output transfer-map DC gain of
    /// the equivalent ordinary discrete SOS cascade.
    pub fn dc_gain(&self) -> Result<Complex<R>, LtiError> {
        let mut gain = self.gain;
        for section in &self.sections {
            gain *= delta_section_dc_gain(*section);
        }

        let gain = Complex::new(gain, R::zero());
        if gain.re.is_finite() && gain.im.is_finite() {
            Ok(gain)
        } else {
            Err(LtiError::NonFiniteResult { which: "dc_gain" })
        }
    }

    /// Casts the delta cascade to another real scalar dtype.
    ///
    /// This is intended for runtime experiments such as evaluating the same
    /// designed filter in `f32` versus `f64` without re-running the design
    /// pipeline.
    pub fn try_cast<S>(&self) -> Result<DeltaSos<S>, LtiError>
    where
        S: Float + RealField + NumCast,
    {
        DeltaSos::new(
            self.sections
                .iter()
                .map(|section| section.try_cast())
                .collect::<Result<Vec<_>, _>>()?,
            cast_real_scalar(self.gain, "delta_sos.gain")?,
            cast_real_scalar(self.sample_time(), "delta_sos.sample_time")?,
        )
    }
}

impl<R> DeltaSection<R>
where
    R: Float + RealField,
{
    /// Casts one delta section to another real scalar dtype.
    ///
    /// The structural variant is preserved exactly; only the stored scalar
    /// parameters are converted.
    pub fn try_cast<S>(&self) -> Result<DeltaSection<S>, LtiError>
    where
        S: Float + RealField + NumCast,
    {
        match *self {
            DeltaSection::Direct { d } => Ok(DeltaSection::Direct {
                d: cast_real_scalar(d, "delta_sos.section.d")?,
            }),
            DeltaSection::First { alpha0, c0, d } => Ok(DeltaSection::First {
                alpha0: cast_real_scalar(alpha0, "delta_sos.section.alpha0")?,
                c0: cast_real_scalar(c0, "delta_sos.section.c0")?,
                d: cast_real_scalar(d, "delta_sos.section.d")?,
            }),
            DeltaSection::Second {
                alpha0,
                alpha1,
                c1,
                c2,
                d,
            } => Ok(DeltaSection::Second {
                alpha0: cast_real_scalar(alpha0, "delta_sos.section.alpha0")?,
                alpha1: cast_real_scalar(alpha1, "delta_sos.section.alpha1")?,
                c1: cast_real_scalar(c1, "delta_sos.section.c1")?,
                c2: cast_real_scalar(c2, "delta_sos.section.c2")?,
                d: cast_real_scalar(d, "delta_sos.section.d")?,
            }),
        }
    }
}

impl<R> DiscreteSos<R>
where
    R: Float + RealField,
{
    /// Converts an ordinary discrete SOS cascade into delta-operator section
    /// form for improved low-cutoff runtime conditioning.
    ///
    /// The conversion keeps the original cascade factorization. Each section
    /// is reduced to its true order, normalized to a monic denominator, and
    /// then rewritten into a forward-delta state update. The ordinary
    /// [`DiscreteSos`] remains the canonical stored representation; this
    /// conversion only changes the runtime basis used during execution.
    pub fn to_delta_sos(&self) -> Result<DeltaSos<R>, LtiError> {
        let dt = self.sample_time();
        let sections = self
            .sections()
            .iter()
            .map(|section| delta_section_from_sos(section.numerator(), section.denominator(), dt))
            .collect::<Result<Vec<_>, _>>()?;
        DeltaSos::new(sections, self.gain(), self.sample_time())
    }
}

/// Returns the steady-state gain contribution of one delta section.
///
/// This evaluates the section at `δ = 0`, which is the discrete-time DC point
/// corresponding to `z = 1`.
fn delta_section_dc_gain<R>(section: DeltaSection<R>) -> R
where
    R: Float + RealField,
{
    match section {
        DeltaSection::Direct { d } => d,
        DeltaSection::First { alpha0, c0, d } => d + c0 / alpha0,
        DeltaSection::Second { alpha0, c1, d, .. } => d + c1 / alpha0,
    }
}

/// Converts one ordinary SOS section into the equivalent delta-section form.
///
/// The input coefficients are first reduced to the true section order and
/// normalized to a monic denominator. The resulting polynomial is then
/// rewritten in the forward-delta basis used by [`DeltaSection`].
fn delta_section_from_sos<R>(
    numerator: [R; 3],
    denominator: [R; 3],
    sample_time: R,
) -> Result<DeltaSection<R>, LtiError>
where
    R: Float + RealField,
{
    // First collapse padded first-order sections and normalize the denominator
    // so the basis change works from the actual section polynomial rather than
    // from storage-level zeros.
    let (numerator, denominator) = reduced_section_polynomials(numerator, denominator)?;
    let dt = sample_time;
    let dt2 = dt * dt;

    match denominator.len() - 1 {
        0 => {
            if numerator.len() != 1 {
                return Err(LtiError::ImproperTransferFunction {
                    numerator_degree: numerator.len() - 1,
                    denominator_degree: 0,
                });
            }
            Ok(DeltaSection::Direct { d: numerator[0] })
        }
        1 => {
            let d0 = denominator[1];
            let [n1, n0] = match numerator.as_slice() {
                [n0] => [R::zero(), *n0],
                [n1, n0] => [*n1, *n0],
                _ => {
                    return Err(LtiError::ImproperTransferFunction {
                        numerator_degree: numerator.len() - 1,
                        denominator_degree: 1,
                    });
                }
            };

            // For `H(z) = (n1 z + n0) / (z + d0)`, substitute
            // `z = 1 / (1 - dt δ)` and collect powers of `δ` to obtain the
            // first-order delta form `δ x = -alpha0 x + u`, `y = c0 x + d u`.
            let alpha0 = (R::one() + d0) / dt;
            let d = n1;
            let g0 = (n1 + n0) / dt;
            let c0 = g0 - alpha0 * d;

            Ok(DeltaSection::First { alpha0, c0, d })
        }
        2 => {
            let d1 = denominator[1];
            let d0 = denominator[2];
            let [n2, n1, n0] = match numerator.as_slice() {
                [n0] => [R::zero(), R::zero(), *n0],
                [n1, n0] => [R::zero(), *n1, *n0],
                [n2, n1, n0] => [*n2, *n1, *n0],
                _ => unreachable!("section order is at most two"),
            };

            // For `H(z) = (n2 z^2 + n1 z + n0) / (z^2 + d1 z + d0)`, the same
            // substitution produces a delta-basis denominator
            // `δ² + alpha1 δ + alpha0`. The output coefficients are then
            // chosen so the delta state update realizes the same transfer map.
            let alpha1 = (R::one() + R::one() + d1) / dt;
            let alpha0 = (R::one() + d1 + d0) / dt2;
            let d = n2;
            let g1 = (n2 + n2 + n1) / dt;
            let g0 = (n2 + n1 + n0) / dt2;
            let c2 = g1 - alpha1 * d;
            let c1 = g0 - alpha0 * d;

            Ok(DeltaSection::Second {
                alpha0,
                alpha1,
                c1,
                c2,
                d,
            })
        }
        _ => unreachable!("SOS sections have order at most two"),
    }
}

/// Reduces one stored SOS section to its true normalized polynomial pair.
///
/// This strips leading storage zeros, cancels the common trailing zero used by
/// padded first-order sections, and rescales the denominator to monic form.
fn reduced_section_polynomials<R>(
    numerator: [R; 3],
    denominator: [R; 3],
) -> Result<(Vec<R>, Vec<R>), LtiError>
where
    R: Float + RealField,
{
    let mut numerator = trim_leading_zeros(&numerator);
    let mut denominator = trim_leading_zeros(&denominator);

    // Padded first-order sections are stored with a shared trailing zero in
    // both numerator and denominator. Cancel that common factor so later order
    // checks and delta formulas operate on the true section order.
    while numerator.len() > 1
        && denominator.len() > 1
        && numerator.last() == Some(&R::zero())
        && denominator.last() == Some(&R::zero())
    {
        numerator.pop();
        denominator.pop();
    }

    if denominator.is_empty() || denominator[0] == R::zero() {
        return Err(LtiError::ZeroLeadingCoefficient {
            which: "sos.denominator",
        });
    }

    // Normalize to a monic denominator so the delta formulas can read the
    // section coefficients directly.
    let scale = denominator[0].recip();
    for value in &mut numerator {
        *value *= scale;
    }
    for value in &mut denominator {
        *value *= scale;
    }

    if numerator.len() > denominator.len() {
        return Err(LtiError::ImproperTransferFunction {
            numerator_degree: numerator.len() - 1,
            denominator_degree: denominator.len() - 1,
        });
    }

    Ok((numerator, denominator))
}
