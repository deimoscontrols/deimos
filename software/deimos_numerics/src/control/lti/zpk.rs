use super::error::LtiError;
use super::sos::Sos;
use super::transfer_function::TransferFunction;
use super::util::{
    CompositionDomain, cast_real_scalar, real_poly_from_roots, validate_sample_time,
};
use super::{ContinuousStateSpace, ContinuousTime, DiscreteStateSpace, DiscreteTime};
use alloc::vec::Vec;
use faer::complex::Complex;
use faer_traits::RealField;
use num_traits::{Float, NumCast};

/// Real-gain SISO zero/pole/gain representation.
#[derive(Clone, Debug, PartialEq)]
pub struct Zpk<R, Domain> {
    zeros: Vec<Complex<R>>,
    poles: Vec<Complex<R>>,
    gain: R,
    domain: Domain,
}

/// Continuous-time SISO zero/pole/gain representation.
pub type ContinuousZpk<R> = Zpk<R, ContinuousTime>;

/// Discrete-time SISO zero/pole/gain representation.
pub type DiscreteZpk<R> = Zpk<R, DiscreteTime<R>>;

impl<R, Domain> Zpk<R, Domain>
where
    R: Float + RealField,
    Domain: Clone,
{
    /// Creates a zero/pole/gain representation.
    pub fn new(
        zeros: impl Into<Vec<Complex<R>>>,
        poles: impl Into<Vec<Complex<R>>>,
        gain: R,
        domain: Domain,
    ) -> Result<Self, LtiError> {
        Ok(Self {
            zeros: zeros.into(),
            poles: poles.into(),
            gain,
            domain,
        })
    }

    /// Zeros of the transfer function.
    #[must_use]
    pub fn zeros(&self) -> &[Complex<R>] {
        &self.zeros
    }

    /// Poles of the transfer function.
    #[must_use]
    pub fn poles(&self) -> &[Complex<R>] {
        &self.poles
    }

    /// Overall scalar gain.
    #[must_use]
    pub fn gain(&self) -> R {
        self.gain
    }

    /// Domain metadata carried by the representation.
    #[must_use]
    pub fn domain(&self) -> &Domain {
        &self.domain
    }

    /// Evaluates the transfer function at the supplied complex point.
    #[must_use]
    pub fn evaluate(&self, point: Complex<R>) -> Complex<R> {
        let num = self
            .zeros
            .iter()
            .fold(Complex::new(self.gain, R::zero()), |acc, &zero| {
                acc * (point - zero)
            });
        let den = self
            .poles
            .iter()
            .fold(Complex::new(R::one(), R::zero()), |acc, &pole| {
                acc * (point - pole)
            });
        num / den
    }

    /// Converts zero/pole/gain form back into coefficient form.
    ///
    /// This is the central reverse path from root data into the rest of the
    /// representation graph. The state-space conversion methods intentionally
    /// chain through this coefficient form instead of reimplementing a second
    /// realization path here.
    pub fn to_transfer_function(&self) -> Result<TransferFunction<R, Domain>, LtiError> {
        let mut numerator = real_poly_from_roots(&self.zeros, "zeros")?;
        for coeff in &mut numerator {
            *coeff *= self.gain;
        }
        let denominator = real_poly_from_roots(&self.poles, "poles")?;
        TransferFunction::new(numerator, denominator, self.domain.clone())
    }

    /// Converts zero/pole/gain form into a second-order-section cascade.
    ///
    /// The SOS path is implemented through the existing real root-section
    /// builder so the pairing and padding logic stays centralized.
    pub fn to_sos(&self) -> Result<Sos<R, Domain>, LtiError> {
        Sos::from_zpk(self)
    }
}

impl<R, Domain> Zpk<R, Domain>
where
    R: Float + RealField,
    Domain: CompositionDomain<R>,
{
    /// Forms the parallel composition `self + rhs`.
    ///
    /// Composition is routed through `TransferFunction`, which is the
    /// arithmetic hub of the current SISO representation layer.
    pub fn add(&self, rhs: &Self) -> Result<Self, LtiError> {
        self.to_transfer_function()?
            .add(&rhs.to_transfer_function()?)?
            .to_zpk()
    }

    /// Forms the parallel difference `self - rhs`.
    pub fn sub(&self, rhs: &Self) -> Result<Self, LtiError> {
        self.to_transfer_function()?
            .sub(&rhs.to_transfer_function()?)?
            .to_zpk()
    }

    /// Forms the series composition `self * rhs`.
    pub fn mul(&self, rhs: &Self) -> Result<Self, LtiError> {
        self.to_transfer_function()?
            .mul(&rhs.to_transfer_function()?)?
            .to_zpk()
    }

    /// Forms the quotient `self / rhs`.
    pub fn div(&self, rhs: &Self) -> Result<Self, LtiError> {
        self.to_transfer_function()?
            .div(&rhs.to_transfer_function()?)?
            .to_zpk()
    }

    /// Returns the inverse `1 / self`.
    pub fn inv(&self) -> Result<Self, LtiError> {
        self.to_transfer_function()?.inv()?.to_zpk()
    }

    /// Forms the standard negative-feedback closure `self / (1 + self * rhs)`.
    pub fn feedback(&self, rhs: &Self) -> Result<Self, LtiError> {
        self.to_transfer_function()?
            .feedback(&rhs.to_transfer_function()?)?
            .to_zpk()
    }

    /// Forms the positive-feedback closure `self / (1 - self * rhs)`.
    pub fn positive_feedback(&self, rhs: &Self) -> Result<Self, LtiError> {
        self.to_transfer_function()?
            .positive_feedback(&rhs.to_transfer_function()?)?
            .to_zpk()
    }

    /// Forms the standard unity negative-feedback closure `self / (1 + self)`.
    pub fn unity_feedback(&self) -> Result<Self, LtiError> {
        self.to_transfer_function()?.unity_feedback()?.to_zpk()
    }

    /// Forms the unity positive-feedback closure `self / (1 - self)`.
    pub fn positive_unity_feedback(&self) -> Result<Self, LtiError> {
        self.to_transfer_function()?
            .positive_unity_feedback()?
            .to_zpk()
    }
}

impl<R> ContinuousZpk<R>
where
    R: Float + RealField,
{
    /// Creates a continuous-time zero/pole/gain representation.
    pub fn continuous(
        zeros: impl Into<Vec<Complex<R>>>,
        poles: impl Into<Vec<Complex<R>>>,
        gain: R,
    ) -> Result<Self, LtiError> {
        Self::new(zeros, poles, gain, ContinuousTime)
    }

    /// Returns the steady-state gain `G(0)`.
    ///
    /// This evaluates the factored transfer map directly at `s = 0` and
    /// rejects poles at the origin through `NonFiniteResult`.
    pub fn dc_gain(&self) -> Result<Complex<R>, LtiError> {
        let gain = self.evaluate(Complex::new(R::zero(), R::zero()));
        if gain.re.is_finite() && gain.im.is_finite() {
            Ok(gain)
        } else {
            Err(LtiError::NonFiniteResult { which: "dc_gain" })
        }
    }

    /// Converts zero/pole/gain form to continuous-time state space through
    /// `TransferFunction`.
    ///
    /// This is intentionally a chained conversion. `TransferFunction` is the
    /// hub of the current SISO conversion graph.
    pub fn to_state_space(&self) -> Result<ContinuousStateSpace<R>, LtiError> {
        self.to_transfer_function()?.to_state_space()
    }

    /// Casts the continuous-time zero/pole/gain representation to another
    /// real scalar dtype.
    ///
    /// Real and imaginary parts of each root are converted independently.
    pub fn try_cast<S>(&self) -> Result<ContinuousZpk<S>, LtiError>
    where
        S: Float + RealField + NumCast,
    {
        ContinuousZpk::continuous(
            self.zeros()
                .iter()
                .copied()
                .map(|value| {
                    Ok(Complex::new(
                        cast_real_scalar(value.re, "zpk.zeros")?,
                        cast_real_scalar(value.im, "zpk.zeros")?,
                    ))
                })
                .collect::<Result<Vec<_>, LtiError>>()?,
            self.poles()
                .iter()
                .copied()
                .map(|value| {
                    Ok(Complex::new(
                        cast_real_scalar(value.re, "zpk.poles")?,
                        cast_real_scalar(value.im, "zpk.poles")?,
                    ))
                })
                .collect::<Result<Vec<_>, LtiError>>()?,
            cast_real_scalar(self.gain(), "zpk.gain")?,
        )
    }
}

impl<R> DiscreteZpk<R>
where
    R: Float + RealField,
{
    /// Creates a discrete-time zero/pole/gain representation.
    pub fn discrete(
        zeros: impl Into<Vec<Complex<R>>>,
        poles: impl Into<Vec<Complex<R>>>,
        gain: R,
        sample_time: R,
    ) -> Result<Self, LtiError> {
        validate_sample_time(sample_time)?;
        Self::new(zeros, poles, gain, DiscreteTime::new(sample_time))
    }

    /// Sample interval carried by the discrete-time representation.
    #[must_use]
    pub fn sample_time(&self) -> R {
        self.domain.sample_time()
    }

    /// Creates the exact `samples`-step pure delay `z^-samples`.
    ///
    /// In zero/pole/gain form this is represented by `samples` repeated poles
    /// at the origin, no zeros, and unit gain.
    pub fn delay(samples: usize, sample_time: R) -> Result<Self, LtiError> {
        let zero = Complex::new(R::zero(), R::zero());
        Self::discrete(Vec::new(), vec![zero; samples], R::one(), sample_time)
    }

    /// Returns the steady-state gain `G(1)`.
    ///
    /// This evaluates the factored transfer map directly at the discrete
    /// steady-state point `z = 1`.
    pub fn dc_gain(&self) -> Result<Complex<R>, LtiError> {
        let gain = self.evaluate(Complex::new(R::one(), R::zero()));
        if gain.re.is_finite() && gain.im.is_finite() {
            Ok(gain)
        } else {
            Err(LtiError::NonFiniteResult { which: "dc_gain" })
        }
    }

    /// Converts zero/pole/gain form to discrete-time state space through
    /// `TransferFunction`.
    ///
    /// This keeps the domain-preserving realization logic centralized in the
    /// transfer-function layer.
    pub fn to_state_space(&self) -> Result<DiscreteStateSpace<R>, LtiError> {
        self.to_transfer_function()?.to_state_space()
    }

    /// Casts the discrete-time zero/pole/gain representation and sample time
    /// to another real scalar dtype.
    ///
    /// This preserves the exact factored structure and only changes the scalar
    /// storage type.
    pub fn try_cast<S>(&self) -> Result<DiscreteZpk<S>, LtiError>
    where
        S: Float + RealField + NumCast,
    {
        DiscreteZpk::discrete(
            self.zeros()
                .iter()
                .copied()
                .map(|value| {
                    Ok(Complex::new(
                        cast_real_scalar(value.re, "zpk.zeros")?,
                        cast_real_scalar(value.im, "zpk.zeros")?,
                    ))
                })
                .collect::<Result<Vec<_>, LtiError>>()?,
            self.poles()
                .iter()
                .copied()
                .map(|value| {
                    Ok(Complex::new(
                        cast_real_scalar(value.re, "zpk.poles")?,
                        cast_real_scalar(value.im, "zpk.poles")?,
                    ))
                })
                .collect::<Result<Vec<_>, LtiError>>()?,
            cast_real_scalar(self.gain(), "zpk.gain")?,
            cast_real_scalar(self.sample_time(), "zpk.sample_time")?,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::ContinuousZpk;
    use alloc::vec::Vec;
    use faer::complex::Complex;

    fn assert_close(lhs: f64, rhs: f64, tol: f64) {
        let err = (lhs - rhs).abs();
        assert!(err <= tol, "lhs={lhs}, rhs={rhs}, err={err}, tol={tol}");
    }

    fn assert_complex_close(lhs: Complex<f64>, rhs: Complex<f64>, tol: f64) {
        assert_close(lhs.re, rhs.re, tol);
        assert_close(lhs.im, rhs.im, tol);
    }

    #[test]
    fn zpk_to_transfer_function_preserves_gain_across_full_numerator() {
        let zpk = ContinuousZpk::continuous(vec![Complex::new(1.0, 0.0)], Vec::new(), 2.0).unwrap();
        let tf = zpk.to_transfer_function().unwrap();

        assert_eq!(tf.numerator(), &[2.0, -2.0]);
        assert_complex_close(
            tf.evaluate(Complex::new(3.0, 0.0)),
            zpk.evaluate(Complex::new(3.0, 0.0)),
            1.0e-12,
        );
        assert_complex_close(
            tf.evaluate(Complex::new(-2.0, 0.0)),
            zpk.evaluate(Complex::new(-2.0, 0.0)),
            1.0e-12,
        );
    }
}
