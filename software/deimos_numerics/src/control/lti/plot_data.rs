//! Plotting-oriented LTI data helpers.
//!
//! This module deliberately stops at data generation. Rendering stays outside
//! the crate, but callers can reuse these helpers to produce consistent Bode
//! and pole-zero inputs from any supported SISO LTI representation. Bode phase
//! is returned in unwrapped form so downstream plotting code can decide
//! whether to display continuity-preserving or wrapped traces. Because that
//! phase is unwrapped, the input frequency grid must be monotone
//! nondecreasing.
//!
//! # Glossary
//!
//! - **Bode data:** Frequency response reported as magnitude and phase versus
//!   frequency.
//! - **Unwrapped phase:** Phase adjusted by `±360 deg` jumps to preserve
//!   continuity.
//! - **Pole-zero data:** Root locations of the system numerator and
//!   denominator.

use super::{
    ContinuousSos, ContinuousStateSpace, ContinuousTransferFunction, ContinuousZpk, DiscreteSos,
    DiscreteStateSpace, DiscreteTransferFunction, DiscreteZpk, LtiError,
    util::{unwrap_phase_deg, validate_nonnegative_monotone_grid},
};
use alloc::vec::Vec;
use faer::complex::Complex;
use faer_traits::RealField;
use faer_traits::ext::ComplexFieldExt;
use num_traits::Float;

/// Bode-plot data evaluated on a monotone nondecreasing angular-frequency
/// grid.
#[derive(Clone, Debug, PartialEq)]
pub struct BodeData<R> {
    /// Angular frequencies at which the transfer map was evaluated.
    pub angular_frequencies: Vec<R>,
    /// Magnitude in dB.
    pub magnitude_db: Vec<R>,
    /// Phase in degrees, unwrapped to preserve continuity across `±180 deg`
    /// branch cuts on the supplied frequency grid.
    pub phase_deg: Vec<R>,
}

/// Pole-zero data for a SISO LTI representation.
#[derive(Clone, Debug, PartialEq)]
pub struct PoleZeroData<R> {
    /// Poles of the represented transfer map.
    pub poles: Vec<Complex<R>>,
    /// Zeros of the represented transfer map.
    pub zeros: Vec<Complex<R>>,
}

impl<R> ContinuousTransferFunction<R>
where
    R: Float + RealField,
{
    /// Evaluates bode magnitude and phase on an angular-frequency grid.
    ///
    /// The grid is interpreted in continuous-time angular frequency units and
    /// evaluated at `s = j * omega`. Because phase is returned in unwrapped
    /// form, the frequency grid must be monotone nondecreasing.
    pub fn bode_data(&self, angular_frequencies: &[R]) -> Result<BodeData<R>, LtiError> {
        bode_from_evaluator(angular_frequencies, |omega| {
            Ok(self.evaluate(Complex::new(R::zero(), omega)))
        })
    }

    /// Returns poles and zeros of the transfer function.
    pub fn pole_zero_data(&self) -> Result<PoleZeroData<R>, LtiError> {
        let zpk = self.to_zpk()?;
        Ok(PoleZeroData {
            poles: zpk.poles().to_vec(),
            zeros: zpk.zeros().to_vec(),
        })
    }
}

impl<R> DiscreteTransferFunction<R>
where
    R: Float + RealField,
{
    /// Evaluates bode magnitude and phase on an angular-frequency grid.
    ///
    /// The grid is interpreted in physical angular frequency units and mapped
    /// to the unit circle as `z = exp(j * omega * dt)`. Because phase is
    /// returned in unwrapped form, the frequency grid must be monotone
    /// nondecreasing.
    pub fn bode_data(&self, angular_frequencies: &[R]) -> Result<BodeData<R>, LtiError> {
        let dt = self.sample_time();
        bode_from_evaluator(angular_frequencies, |omega| {
            let phase = omega * dt;
            Ok(self.evaluate(Complex::new(phase.cos(), phase.sin())))
        })
    }

    /// Returns poles and zeros of the transfer function.
    pub fn pole_zero_data(&self) -> Result<PoleZeroData<R>, LtiError> {
        let zpk = self.to_zpk()?;
        Ok(PoleZeroData {
            poles: zpk.poles().to_vec(),
            zeros: zpk.zeros().to_vec(),
        })
    }
}

impl<R> ContinuousZpk<R>
where
    R: Float + RealField,
{
    /// Evaluates bode magnitude and phase on an angular-frequency grid.
    pub fn bode_data(&self, angular_frequencies: &[R]) -> Result<BodeData<R>, LtiError> {
        bode_from_evaluator(angular_frequencies, |omega| {
            Ok(self.evaluate(Complex::new(R::zero(), omega)))
        })
    }

    /// Returns poles and zeros directly from `Zpk` storage.
    #[must_use]
    pub fn pole_zero_data(&self) -> PoleZeroData<R> {
        PoleZeroData {
            poles: self.poles().to_vec(),
            zeros: self.zeros().to_vec(),
        }
    }
}

impl<R> DiscreteZpk<R>
where
    R: Float + RealField,
{
    /// Evaluates bode magnitude and phase on an angular-frequency grid.
    pub fn bode_data(&self, angular_frequencies: &[R]) -> Result<BodeData<R>, LtiError> {
        let dt = self.sample_time();
        bode_from_evaluator(angular_frequencies, |omega| {
            let phase = omega * dt;
            Ok(self.evaluate(Complex::new(phase.cos(), phase.sin())))
        })
    }

    /// Returns poles and zeros directly from `Zpk` storage.
    #[must_use]
    pub fn pole_zero_data(&self) -> PoleZeroData<R> {
        PoleZeroData {
            poles: self.poles().to_vec(),
            zeros: self.zeros().to_vec(),
        }
    }
}

impl<R> ContinuousSos<R>
where
    R: Float + RealField,
{
    /// Evaluates bode magnitude and phase on an angular-frequency grid.
    pub fn bode_data(&self, angular_frequencies: &[R]) -> Result<BodeData<R>, LtiError> {
        bode_from_evaluator(angular_frequencies, |omega| {
            Ok(self.evaluate(Complex::new(R::zero(), omega)))
        })
    }

    /// Returns poles and zeros by chaining through `Zpk`.
    pub fn pole_zero_data(&self) -> Result<PoleZeroData<R>, LtiError> {
        Ok(self.to_zpk()?.pole_zero_data())
    }
}

impl<R> DiscreteSos<R>
where
    R: Float + RealField,
{
    /// Evaluates bode magnitude and phase on an angular-frequency grid.
    pub fn bode_data(&self, angular_frequencies: &[R]) -> Result<BodeData<R>, LtiError> {
        let dt = self.domain().sample_time();
        bode_from_evaluator(angular_frequencies, |omega| {
            let phase = omega * dt;
            Ok(self.evaluate(Complex::new(phase.cos(), phase.sin())))
        })
    }

    /// Returns poles and zeros by chaining through `Zpk`.
    pub fn pole_zero_data(&self) -> Result<PoleZeroData<R>, LtiError> {
        Ok(self.to_zpk()?.pole_zero_data())
    }
}

impl<R> ContinuousStateSpace<R>
where
    R: Float + RealField,
{
    /// Evaluates bode magnitude and phase for a dense real SISO model.
    ///
    /// The state-space helper stays SISO-only because the plotting-oriented
    /// output format here is a single complex response value per frequency.
    pub fn bode_data(&self, angular_frequencies: &[R]) -> Result<BodeData<R>, LtiError> {
        if !self.is_siso() {
            return Err(LtiError::NonSisoStateSpace {
                ninputs: self.ninputs(),
                noutputs: self.noutputs(),
            });
        }
        bode_from_evaluator(angular_frequencies, |omega| {
            self.transfer_at(Complex::new(R::zero(), omega))
                .map(|value| value[(0, 0)])
        })
    }

    /// Returns poles and zeros of the represented transfer map.
    pub fn pole_zero_data(&self) -> Result<PoleZeroData<R>, LtiError> {
        let zpk = self.to_zpk()?;
        Ok(PoleZeroData {
            poles: zpk.poles().to_vec(),
            zeros: zpk.zeros().to_vec(),
        })
    }
}

impl<R> DiscreteStateSpace<R>
where
    R: Float + RealField,
{
    /// Evaluates bode magnitude and phase for a dense real SISO model.
    ///
    /// The discrete path uses the same unit-circle mapping as the transfer-
    /// function and ZPK helpers.
    pub fn bode_data(&self, angular_frequencies: &[R]) -> Result<BodeData<R>, LtiError> {
        if !self.is_siso() {
            return Err(LtiError::NonSisoStateSpace {
                ninputs: self.ninputs(),
                noutputs: self.noutputs(),
            });
        }
        let dt = self.sample_time();
        bode_from_evaluator(angular_frequencies, |omega| {
            let phase = omega * dt;
            self.transfer_at(Complex::new(phase.cos(), phase.sin()))
                .map(|value| value[(0, 0)])
        })
    }

    /// Returns poles and zeros of the represented transfer map.
    pub fn pole_zero_data(&self) -> Result<PoleZeroData<R>, LtiError> {
        let zpk = self.to_zpk()?;
        Ok(PoleZeroData {
            poles: zpk.poles().to_vec(),
            zeros: zpk.zeros().to_vec(),
        })
    }
}

fn bode_from_evaluator<R, F>(
    angular_frequencies: &[R],
    mut evaluate: F,
) -> Result<BodeData<R>, LtiError>
where
    R: Float + RealField,
    F: FnMut(R) -> Result<Complex<R>, LtiError>,
{
    // Keep the plotting layer opinionated but minimal: validate the grid, turn
    // complex samples into magnitude/phase, then unwrap the phase trace once
    // at the end so the result is continuous on the caller's grid.
    validate_nonnegative_monotone_grid(angular_frequencies, "bode_data")?;
    let mut magnitude_db = Vec::with_capacity(angular_frequencies.len());
    let mut phase_deg_wrapped = Vec::with_capacity(angular_frequencies.len());
    for &omega in angular_frequencies {
        let value = evaluate(omega)?;
        magnitude_db.push(R::from(20.0).unwrap() * value.abs().log10());
        phase_deg_wrapped.push(value.im.atan2(value.re).to_degrees());
    }
    Ok(BodeData {
        angular_frequencies: angular_frequencies.to_vec(),
        magnitude_db,
        phase_deg: unwrap_phase_deg(&phase_deg_wrapped),
    })
}

#[cfg(test)]
mod tests {
    use super::{ContinuousTransferFunction, DiscreteTransferFunction};
    use crate::control::lti::LtiError;
    use faer::complex::Complex;
    use nalgebra::ComplexField;

    fn assert_close(lhs: f64, rhs: f64, tol: f64) {
        let err = (lhs - rhs).abs();
        assert!(err <= tol, "lhs={lhs}, rhs={rhs}, err={err}, tol={tol}");
    }

    #[test]
    fn bode_data_matches_direct_transfer_function_evaluation() {
        let tf = ContinuousTransferFunction::continuous(vec![1.0], vec![1.0, 1.0]).unwrap();
        let omega = 2.0;
        let bode = tf.bode_data(&[omega]).unwrap();
        let value = tf.evaluate(Complex::new(0.0, omega));
        assert_close(bode.magnitude_db[0], 20.0 * value.abs().log10(), 1.0e-12);
        assert_close(
            bode.phase_deg[0],
            value.im.atan2(value.re).to_degrees(),
            1.0e-12,
        );
    }

    #[test]
    fn bode_phase_is_unwrapped_across_frequency_grid() {
        let tf =
            ContinuousTransferFunction::continuous(vec![1.0], vec![1.0, 3.0, 3.0, 1.0]).unwrap();
        let grid = vec![1.0e-2, 1.0e-1, 1.0, 10.0, 100.0];
        let bode = tf.bode_data(&grid).unwrap();

        assert!(bode.phase_deg.last().copied().unwrap() < -200.0);
        assert!(
            bode.phase_deg
                .windows(2)
                .all(|window| window[1] <= window[0])
        );
    }

    #[test]
    fn bode_data_rejects_unsorted_frequency_grid() {
        let tf =
            ContinuousTransferFunction::continuous(vec![1.0], vec![1.0, 3.0, 3.0, 1.0]).unwrap();

        assert!(matches!(
            tf.bode_data(&[100.0, 1.0]),
            Err(LtiError::InvalidSampleGrid { which: "bode_data" })
        ));
    }

    #[test]
    fn pole_zero_data_matches_zpk_conversion() {
        let tf =
            ContinuousTransferFunction::continuous(vec![1.0, 2.0], vec![1.0, 3.0, 2.0]).unwrap();
        let direct = tf.pole_zero_data().unwrap();
        let zpk = tf.to_zpk().unwrap();
        assert_eq!(direct.poles, zpk.poles().to_vec());
        assert_eq!(direct.zeros, zpk.zeros().to_vec());
    }

    #[test]
    fn discrete_bode_uses_unit_circle_mapping() {
        let tf = DiscreteTransferFunction::discrete(vec![1.0], vec![1.0, -0.5], 0.1).unwrap();
        let omega = 3.0;
        let bode = tf.bode_data(&[omega]).unwrap();
        let phase: f64 = omega * 0.1;
        let value = tf.evaluate(Complex::new(phase.cos(), phase.sin()));
        assert_close(bode.magnitude_db[0], 20.0 * value.abs().log10(), 1.0e-12);
    }
}
