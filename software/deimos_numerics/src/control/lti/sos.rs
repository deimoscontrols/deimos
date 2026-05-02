//! Canonical second-order-section representation for realized SISO filters.
//!
//! `Sos` is the crate's primary realized IIR storage form:
//!
//! - it is the preferred output of digital and analog IIR design
//! - it is the sectioned form used for conversion and composition
//! - it is the ordinary discrete execution form used by the DF2T runtime
//!
//! For very low normalized digital cutoffs, callers can derive
//! [`DeltaSos`](super::DeltaSos) from a [`DiscreteSos`] when they need a more
//! stable runtime basis near `z = 1`. That does not replace `Sos` as the
//! canonical stored representation.
//!
//! # Glossary
//!
//! - **SOS:** Second-order sections, a factorization of an IIR filter into
//!   first- and second-order pieces.
//! - **Section gain:** Scalar factor multiplying the section cascade.
//! - **Descending-power form:** Coefficient ordering from highest power to
//!   lowest power.
//! - **DF2T:** Transposed direct-form II runtime realization used by the
//!   ordinary discrete execution path.

use super::error::LtiError;
use super::transfer_function::TransferFunction;
use super::util::{
    CompositionDomain, cast_real_scalar, identity_section, poly_mul, root_sections,
    validate_sample_time,
};
use super::zpk::Zpk;
use super::{ContinuousStateSpace, ContinuousTime, DiscreteStateSpace, DiscreteTime};
use crate::scalar::complex_horner_step_real;
use alloc::vec::Vec;
use faer::complex::Complex;
use faer_traits::RealField;
use num_traits::{Float, NumCast};

/// One second-order section in descending-power coefficient form.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SecondOrderSection<R> {
    numerator: [R; 3],
    denominator: [R; 3],
}

impl<R> SecondOrderSection<R>
where
    R: Float + RealField,
{
    /// Creates a normalized second-order section.
    ///
    /// The section storage is also used for padded first-order and identity
    /// factors during `Zpk <-> Sos` conversion. For that reason, normalization
    /// uses the first nonzero denominator coefficient rather than assuming the
    /// quadratic term is present.
    pub fn new(numerator: [R; 3], denominator: [R; 3]) -> Result<Self, LtiError> {
        let leading = denominator
            .into_iter()
            .find(|&value| value != R::zero())
            .ok_or(LtiError::ZeroLeadingCoefficient {
                which: "sos.denominator",
            })?;
        if leading == R::zero() {
            return Err(LtiError::ZeroLeadingCoefficient {
                which: "sos.denominator",
            });
        }
        let scale = leading.recip();
        Ok(Self {
            numerator: numerator.map(|value| value * scale),
            denominator: denominator.map(|value| value * scale),
        })
    }

    /// Numerator coefficients in descending-power order.
    #[must_use]
    pub fn numerator(&self) -> [R; 3] {
        self.numerator
    }

    /// Denominator coefficients in descending-power order.
    #[must_use]
    pub fn denominator(&self) -> [R; 3] {
        self.denominator
    }

    /// Casts the section coefficients to another real scalar dtype.
    ///
    /// The numerator/denominator storage layout is preserved exactly.
    pub fn try_cast<S>(&self) -> Result<SecondOrderSection<S>, LtiError>
    where
        S: Float + RealField + NumCast,
    {
        SecondOrderSection::new(
            [
                cast_real_scalar(self.numerator[0], "sos.section.numerator")?,
                cast_real_scalar(self.numerator[1], "sos.section.numerator")?,
                cast_real_scalar(self.numerator[2], "sos.section.numerator")?,
            ],
            [
                cast_real_scalar(self.denominator[0], "sos.section.denominator")?,
                cast_real_scalar(self.denominator[1], "sos.section.denominator")?,
                cast_real_scalar(self.denominator[2], "sos.section.denominator")?,
            ],
        )
    }

    #[must_use]
    fn evaluate(&self, point: Complex<R>) -> Complex<R> {
        let num = self
            .numerator
            .iter()
            .fold(Complex::new(R::zero(), R::zero()), |acc, &coef| {
                complex_horner_step_real(acc, point, coef)
            });
        let den = self
            .denominator
            .iter()
            .fold(Complex::new(R::zero(), R::zero()), |acc, &coef| {
                complex_horner_step_real(acc, point, coef)
            });
        num / den
    }
}

/// Real-coefficient SISO second-order-section cascade.
///
/// This is the canonical sectioned representation for realized IIR filters in
/// this crate. It is the form used for design outputs, conversion between
/// alternate SISO representations, and ordinary sectionwise execution. When a
/// discrete filter needs a better-conditioned runtime basis at very low
/// normalized cutoff, derive [`super::DeltaSos`] from [`DiscreteSos`] instead
/// of replacing `Sos` itself.
#[derive(Clone, Debug, PartialEq)]
pub struct Sos<R, Domain> {
    sections: Vec<SecondOrderSection<R>>,
    gain: R,
    domain: Domain,
}

/// Continuous-time SISO second-order-section cascade.
///
/// This is the preferred realized continuous-time filter form. Unlike
/// [`DiscreteSos`], there is no delta-operator companion type because the
/// delta basis is specifically a discrete-time runtime technique.
pub type ContinuousSos<R> = Sos<R, ContinuousTime>;

/// Discrete-time SISO second-order-section cascade.
///
/// This is the canonical stored and design-facing digital IIR form. It is also
/// the ordinary sectionwise execution form used by the DF2T runtime. When
/// executing very low-cutoff filters, derive [`super::DeltaSos`] with
/// [`super::DiscreteSos::to_delta_sos`] to move the runtime recurrence into a
/// better-conditioned basis near `z = 1`.
pub type DiscreteSos<R> = Sos<R, DiscreteTime<R>>;

impl<R, Domain> Sos<R, Domain>
where
    R: Float + RealField,
    Domain: Clone,
{
    /// Creates a second-order-section cascade with explicit overall gain.
    ///
    /// This constructor creates the canonical stored section representation,
    /// not the specialized delta-operator runtime form.
    pub fn new(
        sections: impl Into<Vec<SecondOrderSection<R>>>,
        gain: R,
        domain: Domain,
    ) -> Result<Self, LtiError> {
        let sections = sections.into();
        if sections.is_empty() {
            return Err(LtiError::EmptySos);
        }
        Ok(Self {
            sections,
            gain,
            domain,
        })
    }

    /// Sections in cascade order.
    #[must_use]
    pub fn sections(&self) -> &[SecondOrderSection<R>] {
        &self.sections
    }

    /// Overall gain applied to the cascade.
    #[must_use]
    pub fn gain(&self) -> R {
        self.gain
    }

    /// Domain metadata carried by the representation.
    #[must_use]
    pub fn domain(&self) -> &Domain {
        &self.domain
    }

    /// Evaluates the cascade at the supplied complex point.
    ///
    /// This is transfer-map evaluation of the stored SOS representation; it is
    /// not tied to a particular runtime recurrence such as DF2T or delta-SOS.
    #[must_use]
    pub fn evaluate(&self, point: Complex<R>) -> Complex<R> {
        self.sections.iter().fold(
            Complex::new(self.gain, R::zero()),
            |acc: Complex<R>, section: &SecondOrderSection<R>| acc * section.evaluate(point),
        )
    }

    /// Converts the cascade into coefficient form.
    ///
    /// The section numerators and denominators are multiplied in cascade order,
    /// then wrapped back into the normalized `TransferFunction` hub type.
    pub fn to_transfer_function(&self) -> Result<TransferFunction<R, Domain>, LtiError> {
        let mut numerator = vec![self.gain];
        let mut denominator = vec![R::one()];
        for section in &self.sections {
            numerator = poly_mul(&numerator, &section.numerator);
            denominator = poly_mul(&denominator, &section.denominator);
        }
        TransferFunction::new(numerator, denominator, self.domain.clone())
    }

    /// Converts the cascade into zero/pole/gain form through coefficient form.
    pub fn to_zpk(&self) -> Result<Zpk<R, Domain>, LtiError> {
        self.to_transfer_function()?.to_zpk()
    }

    /// Builds a section cascade from zero/pole/gain data.
    ///
    /// Real roots become padded first-order sections, complex-conjugate pairs
    /// become true quadratic sections, and whichever side has fewer sections is
    /// padded with identity factors so the numerator and denominator cascades
    /// stay aligned section-by-section. The result is the ordinary stored SOS
    /// representation from which the delta runtime form can be derived later if
    /// needed.
    pub fn from_zpk(zpk: &Zpk<R, Domain>) -> Result<Self, LtiError> {
        let numerator_sections = root_sections(zpk.zeros(), "zeros")?;
        let denominator_sections = root_sections(zpk.poles(), "poles")?;
        let count = numerator_sections
            .len()
            .max(denominator_sections.len())
            .max(1);

        let mut sections = Vec::with_capacity(count);
        for i in 0..count {
            let numerator = numerator_sections
                .get(i)
                .copied()
                .unwrap_or_else(identity_section);
            let denominator = denominator_sections
                .get(i)
                .copied()
                .unwrap_or_else(identity_section);
            sections.push(SecondOrderSection::new(numerator, denominator)?);
        }

        Self::new(sections, zpk.gain(), zpk.domain().clone())
    }
}

impl<R, Domain> Sos<R, Domain>
where
    R: Float + RealField,
    Domain: CompositionDomain<R>,
{
    /// Forms the parallel composition `self + rhs`.
    ///
    /// As with `Zpk`, the actual arithmetic is delegated to the
    /// `TransferFunction` representation and then converted back into section
    /// form. That keeps the algebra in one place and makes the section layer a
    /// pure storage/conditioning choice.
    pub fn add(&self, rhs: &Self) -> Result<Self, LtiError> {
        self.to_transfer_function()?
            .add(&rhs.to_transfer_function()?)?
            .to_sos()
    }

    /// Forms the parallel difference `self - rhs`.
    pub fn sub(&self, rhs: &Self) -> Result<Self, LtiError> {
        self.to_transfer_function()?
            .sub(&rhs.to_transfer_function()?)?
            .to_sos()
    }

    /// Forms the series composition `self * rhs`.
    pub fn mul(&self, rhs: &Self) -> Result<Self, LtiError> {
        self.to_transfer_function()?
            .mul(&rhs.to_transfer_function()?)?
            .to_sos()
    }

    /// Forms the quotient `self / rhs`.
    pub fn div(&self, rhs: &Self) -> Result<Self, LtiError> {
        self.to_transfer_function()?
            .div(&rhs.to_transfer_function()?)?
            .to_sos()
    }

    /// Returns the inverse `1 / self`.
    pub fn inv(&self) -> Result<Self, LtiError> {
        self.to_transfer_function()?.inv()?.to_sos()
    }

    /// Forms the standard negative-feedback closure `self / (1 + self * rhs)`.
    pub fn feedback(&self, rhs: &Self) -> Result<Self, LtiError> {
        self.to_transfer_function()?
            .feedback(&rhs.to_transfer_function()?)?
            .to_sos()
    }

    /// Forms the positive-feedback closure `self / (1 - self * rhs)`.
    pub fn positive_feedback(&self, rhs: &Self) -> Result<Self, LtiError> {
        self.to_transfer_function()?
            .positive_feedback(&rhs.to_transfer_function()?)?
            .to_sos()
    }

    /// Forms the standard unity negative-feedback closure `self / (1 + self)`.
    pub fn unity_feedback(&self) -> Result<Self, LtiError> {
        self.to_transfer_function()?.unity_feedback()?.to_sos()
    }

    /// Forms the unity positive-feedback closure `self / (1 - self)`.
    pub fn positive_unity_feedback(&self) -> Result<Self, LtiError> {
        self.to_transfer_function()?
            .positive_unity_feedback()?
            .to_sos()
    }
}

impl<R> ContinuousSos<R>
where
    R: Float + RealField,
{
    /// Creates a continuous-time second-order-section cascade.
    pub fn continuous(
        sections: impl Into<Vec<SecondOrderSection<R>>>,
        gain: R,
    ) -> Result<Self, LtiError> {
        Self::new(sections, gain, ContinuousTime)
    }

    /// Returns the steady-state gain `G(0)`.
    ///
    /// This evaluates the cascade directly at `s = 0` and rejects poles at
    /// the origin through `NonFiniteResult`.
    pub fn dc_gain(&self) -> Result<Complex<R>, LtiError> {
        let gain = self.evaluate(Complex::new(R::zero(), R::zero()));
        if gain.re.is_finite() && gain.im.is_finite() {
            Ok(gain)
        } else {
            Err(LtiError::NonFiniteResult { which: "dc_gain" })
        }
    }

    /// Converts the section cascade to continuous-time state space through
    /// `TransferFunction`.
    ///
    /// This stays a chained conversion on purpose so there is only one
    /// `TransferFunction -> StateSpace` realization implementation to maintain.
    pub fn to_state_space(&self) -> Result<ContinuousStateSpace<R>, LtiError> {
        self.to_transfer_function()?.to_state_space()
    }

    /// Casts the continuous-time SOS cascade to another real scalar dtype.
    ///
    /// This preserves the existing section factorization rather than
    /// re-synthesizing sections in the target dtype.
    pub fn try_cast<S>(&self) -> Result<ContinuousSos<S>, LtiError>
    where
        S: Float + RealField + NumCast,
    {
        ContinuousSos::continuous(
            self.sections()
                .iter()
                .map(|section| section.try_cast())
                .collect::<Result<Vec<_>, _>>()?,
            cast_real_scalar(self.gain(), "sos.gain")?,
        )
    }
}

impl<R> DiscreteSos<R>
where
    R: Float + RealField,
{
    /// Creates a discrete-time second-order-section cascade.
    pub fn discrete(
        sections: impl Into<Vec<SecondOrderSection<R>>>,
        gain: R,
        sample_time: R,
    ) -> Result<Self, LtiError> {
        validate_sample_time(sample_time)?;
        Self::new(sections, gain, DiscreteTime::new(sample_time))
    }

    /// Sample interval carried by the discrete-time representation.
    #[must_use]
    pub fn sample_time(&self) -> R {
        self.domain.sample_time()
    }

    /// Creates the exact `samples`-step pure delay `z^-samples`.
    ///
    /// The section cascade is built from the exact zero/pole description of
    /// the delay so the result stays aligned with the crate's other discrete
    /// LTI representations.
    pub fn delay(samples: usize, sample_time: R) -> Result<Self, LtiError> {
        super::DiscreteZpk::delay(samples, sample_time)?.to_sos()
    }

    /// Returns the steady-state gain `G(1)`.
    ///
    /// This evaluates the cascade directly at the discrete steady-state point
    /// `z = 1`.
    pub fn dc_gain(&self) -> Result<Complex<R>, LtiError> {
        let gain = self.evaluate(Complex::new(R::one(), R::zero()));
        if gain.re.is_finite() && gain.im.is_finite() {
            Ok(gain)
        } else {
            Err(LtiError::NonFiniteResult { which: "dc_gain" })
        }
    }

    /// Converts the section cascade to discrete-time state space through
    /// `TransferFunction`.
    ///
    /// The discrete sample time is preserved by the intermediate transfer
    /// function and then carried into the realized state-space model.
    pub fn to_state_space(&self) -> Result<DiscreteStateSpace<R>, LtiError> {
        self.to_transfer_function()?.to_state_space()
    }

    /// Casts the discrete-time SOS cascade and sample time to another real
    /// scalar dtype.
    ///
    /// The conversion keeps the same section ordering and overall gain, which
    /// makes it suitable for direct runtime comparisons between dtypes.
    pub fn try_cast<S>(&self) -> Result<DiscreteSos<S>, LtiError>
    where
        S: Float + RealField + NumCast,
    {
        DiscreteSos::discrete(
            self.sections()
                .iter()
                .map(|section| section.try_cast())
                .collect::<Result<Vec<_>, _>>()?,
            cast_real_scalar(self.gain(), "sos.gain")?,
            cast_real_scalar(self.sample_time(), "sos.sample_time")?,
        )
    }
}
