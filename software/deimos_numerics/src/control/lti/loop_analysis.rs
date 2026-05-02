//! SISO loop-analysis helpers built on the existing LTI evaluation surface.
//!
//! The implementation is intentionally grid-based:
//!
//! - `S` and `T` are formed exactly through transfer-function arithmetic
//! - crossover frequencies and margins are estimated from sampled loop values
//! - Nyquist and Nichols helpers return plotting-oriented data only
//!
//! The important convention shared by every helper here is that phase is
//! unwrapped on the caller's frequency grid before any crossover or margin
//! logic runs. That keeps `-180 deg` crossings numerically meaningful even
//! when the raw pointwise phase jumps across the principal `atan2` branch cut.
//! Any helper that returns or consumes unwrapped phase therefore requires a
//! monotone nondecreasing frequency grid.
//!
//! # Two Intuitions
//!
//! 1. **Robustness view.** Loop analysis asks how much modeling error or extra
//!    loop gain the closed loop can tolerate before it stops behaving well.
//! 2. **Channel view.** The same helpers describe how different signals move
//!    through a feedback loop: disturbances through `S`, sensor noise through
//!    `T`, and controller effort through `KS`.
//!
//! # Glossary
//!
//! - **Loop transfer `L`:** Usually `P C` for plant `P` and controller `C`.
//! - **Sensitivity `S`:** `1 / (1 + L)`.
//! - **Complementary sensitivity `T`:** `L / (1 + L)`.
//! - **Gain margin / phase margin:** Classical robustness margins defined at
//!   phase and gain crossovers.
//!
//! # Mathematical Formulation
//!
//! The module constructs:
//!
//! - `S = 1 / (1 + L)`
//! - `T = L / (1 + L)`
//! - `KS = C / (1 + L)`
//! - `PS = P / (1 + L)`
//!
//! and estimates crossover frequencies and margins from sampled evaluations of
//! `L(jw)` or `L(e^{jw dt})`.
//!
//! # Implementation Notes
//!
//! - Margin and crossover detection is sampled-grid based and therefore only
//!   as accurate as the caller's frequency grid.
//! - Nichols and Nyquist helpers return data only; they do not perform
//!   plotting or winding-number analysis.
//! - Phase unwrapping is shared with the Bode-data path to keep the frequency
//!   analysis conventions aligned.

use super::{
    ContinuousSos, ContinuousStateSpace, ContinuousTime, ContinuousTransferFunction, ContinuousZpk,
    DiscreteSos, DiscreteStateSpace, DiscreteTime, DiscreteTransferFunction, DiscreteZpk, LtiError,
    TransferFunction,
    util::{unwrap_phase_deg, validate_nonnegative_monotone_grid},
};
use crate::scalar::mul_add;
use alloc::vec::Vec;
use faer::complex::Complex;
use faer_traits::RealField;
use faer_traits::ext::ComplexFieldExt;
use faer_traits::math_utils::{eps, from_f64};
use num_traits::Float;

/// Gain- and phase-crossover frequencies detected on a sampled loop grid.
#[derive(Clone, Debug, PartialEq)]
pub struct LoopCrossovers<R> {
    /// Frequencies where the sampled loop magnitude crosses `0 dB`.
    ///
    /// These are estimated by linear interpolation between adjacent sampled
    /// points on the user-provided angular-frequency grid.
    pub gain_crossovers: Vec<R>,
    /// Frequencies where the unwrapped sampled loop phase crosses `-180 deg`
    /// modulo `360 deg`.
    ///
    /// Because the phase trace is first unwrapped, this may yield multiple
    /// crossings separated by whole turns when the sampled loop winds around
    /// the critical point more than once.
    pub phase_crossovers: Vec<R>,
}

/// Classical SISO gain and phase margins estimated from a sampled loop grid.
#[derive(Clone, Debug, PartialEq)]
pub struct LoopMargins<R> {
    /// Gain margin as an absolute multiplicative factor.
    ///
    /// This is derived from the first detected phase crossover and is `None`
    /// when the sampled grid never reaches a `-180 deg` crossing.
    pub gain_margin_abs: Option<R>,
    /// Gain margin in dB.
    pub gain_margin_db: Option<R>,
    /// Phase margin in degrees.
    ///
    /// This is derived from the first detected gain crossover and is `None`
    /// when the sampled grid never reaches `0 dB`.
    pub phase_margin_deg: Option<R>,
    /// First detected gain-crossover frequency.
    pub gain_crossover: Option<R>,
    /// First detected phase-crossover frequency.
    pub phase_crossover: Option<R>,
}

/// Nyquist-plot data sampled on an angular-frequency grid.
#[derive(Clone, Debug, PartialEq)]
pub struct NyquistData<R> {
    /// Angular frequencies at which the loop transfer map was evaluated.
    pub angular_frequencies: Vec<R>,
    /// Complex loop values at those frequencies.
    ///
    /// The Nyquist helper is intentionally just sampled loop data:
    /// it does not mirror the negative-frequency branch or perform winding
    /// analysis on its own.
    pub values: Vec<Complex<R>>,
}

/// Nichols-plot data sampled on a monotone nondecreasing angular-frequency
/// grid.
#[derive(Clone, Debug, PartialEq)]
pub struct NicholsData<R> {
    /// Angular frequencies at which the loop transfer map was evaluated.
    pub angular_frequencies: Vec<R>,
    /// Loop magnitude in dB.
    pub magnitude_db: Vec<R>,
    /// Unwrapped loop phase in degrees.
    ///
    /// This is the same phase convention used internally for crossover and
    /// margin detection, so plotting code sees the same continuous trace that
    /// the numerical analysis layer uses.
    pub phase_deg: Vec<R>,
}

#[derive(Clone, Debug)]
struct LoopSamples<R> {
    angular_frequencies: Vec<R>,
    values: Vec<Complex<R>>,
    magnitude_abs: Vec<R>,
    magnitude_db: Vec<R>,
    phase_deg_unwrapped: Vec<R>,
}

#[derive(Clone, Copy, Debug)]
struct GainCrossover<R> {
    frequency: R,
    phase_deg: R,
}

#[derive(Clone, Copy, Debug)]
struct PhaseCrossover<R> {
    frequency: R,
    magnitude_db: R,
}

impl<R> ContinuousTransferFunction<R>
where
    R: Float + RealField,
{
    /// Returns the unity-feedback sensitivity transfer function
    /// `S = 1 / (1 + L)`.
    ///
    /// This is the standard disturbance-sensitivity transfer for a unity
    /// negative-feedback loop with open-loop transfer `L`.
    pub fn sensitivity(&self) -> Result<Self, LtiError> {
        identity_transfer(ContinuousTime)?.feedback(self)
    }

    /// Returns the unity-feedback complementary sensitivity transfer function
    /// `T = L / (1 + L)`.
    ///
    /// Together with `S`, this satisfies `S + T = 1` for the unity-feedback
    /// loop represented by `self`.
    pub fn complementary_sensitivity(&self) -> Result<Self, LtiError> {
        self.unity_feedback()
    }

    /// Returns the control-sensitivity channel
    /// `KS = C / (1 + P C)` for the supplied controller `C`.
    pub fn control_sensitivity_with(&self, controller: &Self) -> Result<Self, LtiError> {
        controller.feedback(self)
    }

    /// Returns the plant-sensitivity channel
    /// `PS = P / (1 + P C)` for the supplied controller `C`.
    pub fn plant_sensitivity_with(&self, controller: &Self) -> Result<Self, LtiError> {
        self.feedback(controller)
    }

    /// Evaluates the loop transfer on an angular-frequency grid for Nyquist
    /// plotting.
    ///
    /// The returned data is just the sampled positive-frequency branch. Any
    /// mirrored branch or encirclement interpretation is left to downstream
    /// plotting or analysis code.
    pub fn nyquist_data(&self, angular_frequencies: &[R]) -> Result<NyquistData<R>, LtiError> {
        let samples =
            loop_samples_from_evaluator(angular_frequencies, "nyquist_data", false, |w| {
                Ok(self.evaluate(Complex::new(R::zero(), w)))
            })?;
        Ok(NyquistData {
            angular_frequencies: samples.angular_frequencies,
            values: samples.values,
        })
    }

    /// Evaluates the loop transfer on an angular-frequency grid for Nichols
    /// plotting.
    ///
    /// Phase is unwrapped on the provided grid so the returned trace is
    /// continuous across principal-angle branch cuts. Because of that
    /// unwrapping, the frequency grid must be monotone nondecreasing.
    pub fn nichols_data(&self, angular_frequencies: &[R]) -> Result<NicholsData<R>, LtiError> {
        let samples =
            loop_samples_from_evaluator(angular_frequencies, "nichols_data", true, |w| {
                Ok(self.evaluate(Complex::new(R::zero(), w)))
            })?;
        Ok(NicholsData {
            angular_frequencies: samples.angular_frequencies,
            magnitude_db: samples.magnitude_db,
            phase_deg: samples.phase_deg_unwrapped,
        })
    }

    /// Estimates gain- and phase-crossover frequencies from a sampled loop
    /// grid.
    ///
    /// The frequency grid must be monotone. Crossings are linearly
    /// interpolated between adjacent sampled points; no exact root solve is
    /// attempted here.
    pub fn loop_crossovers(
        &self,
        angular_frequencies: &[R],
    ) -> Result<LoopCrossovers<R>, LtiError> {
        let samples =
            loop_samples_from_evaluator(angular_frequencies, "loop_crossovers", true, |w| {
                Ok(self.evaluate(Complex::new(R::zero(), w)))
            })?;
        loop_crossovers_from_samples(&samples)
    }

    /// Estimates classical gain and phase margins from a sampled loop grid.
    ///
    /// This uses the first detected gain and phase crossovers on the supplied
    /// grid. If the grid does not resolve a crossover, the corresponding
    /// margin is returned as `None`.
    pub fn loop_margins(&self, angular_frequencies: &[R]) -> Result<LoopMargins<R>, LtiError> {
        let samples =
            loop_samples_from_evaluator(angular_frequencies, "loop_margins", true, |w| {
                Ok(self.evaluate(Complex::new(R::zero(), w)))
            })?;
        loop_margins_from_samples(&samples)
    }
}

impl<R> DiscreteTransferFunction<R>
where
    R: Float + RealField,
{
    /// Returns the unity-feedback sensitivity transfer function
    /// `S = 1 / (1 + L)`.
    pub fn sensitivity(&self) -> Result<Self, LtiError> {
        identity_transfer(DiscreteTime::new(self.sample_time()))?.feedback(self)
    }

    /// Returns the unity-feedback complementary sensitivity transfer function
    /// `T = L / (1 + L)`.
    pub fn complementary_sensitivity(&self) -> Result<Self, LtiError> {
        self.unity_feedback()
    }

    /// Returns the control-sensitivity channel
    /// `KS = C / (1 + P C)` for the supplied controller `C`.
    pub fn control_sensitivity_with(&self, controller: &Self) -> Result<Self, LtiError> {
        controller.feedback(self)
    }

    /// Returns the plant-sensitivity channel
    /// `PS = P / (1 + P C)` for the supplied controller `C`.
    pub fn plant_sensitivity_with(&self, controller: &Self) -> Result<Self, LtiError> {
        self.feedback(controller)
    }

    /// Evaluates the loop transfer on an angular-frequency grid for Nyquist
    /// plotting.
    pub fn nyquist_data(&self, angular_frequencies: &[R]) -> Result<NyquistData<R>, LtiError> {
        let dt = self.sample_time();
        let samples =
            loop_samples_from_evaluator(angular_frequencies, "nyquist_data", false, |w| {
                let phase = w * dt;
                Ok(self.evaluate(Complex::new(phase.cos(), phase.sin())))
            })?;
        Ok(NyquistData {
            angular_frequencies: samples.angular_frequencies,
            values: samples.values,
        })
    }

    /// Evaluates the loop transfer on an angular-frequency grid for Nichols
    /// plotting.
    pub fn nichols_data(&self, angular_frequencies: &[R]) -> Result<NicholsData<R>, LtiError> {
        let dt = self.sample_time();
        let samples =
            loop_samples_from_evaluator(angular_frequencies, "nichols_data", true, |w| {
                let phase = w * dt;
                Ok(self.evaluate(Complex::new(phase.cos(), phase.sin())))
            })?;
        Ok(NicholsData {
            angular_frequencies: samples.angular_frequencies,
            magnitude_db: samples.magnitude_db,
            phase_deg: samples.phase_deg_unwrapped,
        })
    }

    /// Estimates gain- and phase-crossover frequencies from a sampled loop
    /// grid.
    pub fn loop_crossovers(
        &self,
        angular_frequencies: &[R],
    ) -> Result<LoopCrossovers<R>, LtiError> {
        let dt = self.sample_time();
        let samples =
            loop_samples_from_evaluator(angular_frequencies, "loop_crossovers", true, |w| {
                let phase = w * dt;
                Ok(self.evaluate(Complex::new(phase.cos(), phase.sin())))
            })?;
        loop_crossovers_from_samples(&samples)
    }

    /// Estimates classical gain and phase margins from a sampled loop grid.
    pub fn loop_margins(&self, angular_frequencies: &[R]) -> Result<LoopMargins<R>, LtiError> {
        let dt = self.sample_time();
        let samples =
            loop_samples_from_evaluator(angular_frequencies, "loop_margins", true, |w| {
                let phase = w * dt;
                Ok(self.evaluate(Complex::new(phase.cos(), phase.sin())))
            })?;
        loop_margins_from_samples(&samples)
    }
}

impl<R> ContinuousZpk<R>
where
    R: Float + RealField,
{
    /// Returns the unity-feedback sensitivity transfer function
    /// `S = 1 / (1 + L)`.
    pub fn sensitivity(&self) -> Result<Self, LtiError> {
        self.to_transfer_function()?.sensitivity()?.to_zpk()
    }

    /// Returns the unity-feedback complementary sensitivity transfer function
    /// `T = L / (1 + L)`.
    pub fn complementary_sensitivity(&self) -> Result<Self, LtiError> {
        self.to_transfer_function()?
            .complementary_sensitivity()?
            .to_zpk()
    }

    /// Returns the control-sensitivity channel
    /// `KS = C / (1 + P C)` for the supplied controller `C`.
    pub fn control_sensitivity_with(&self, controller: &Self) -> Result<Self, LtiError> {
        self.to_transfer_function()?
            .control_sensitivity_with(&controller.to_transfer_function()?)?
            .to_zpk()
    }

    /// Returns the plant-sensitivity channel
    /// `PS = P / (1 + P C)` for the supplied controller `C`.
    pub fn plant_sensitivity_with(&self, controller: &Self) -> Result<Self, LtiError> {
        self.to_transfer_function()?
            .plant_sensitivity_with(&controller.to_transfer_function()?)?
            .to_zpk()
    }

    /// Evaluates Nyquist data directly from the factored loop transfer.
    pub fn nyquist_data(&self, angular_frequencies: &[R]) -> Result<NyquistData<R>, LtiError> {
        let samples =
            loop_samples_from_evaluator(angular_frequencies, "nyquist_data", false, |w| {
                Ok(self.evaluate(Complex::new(R::zero(), w)))
            })?;
        Ok(NyquistData {
            angular_frequencies: samples.angular_frequencies,
            values: samples.values,
        })
    }

    /// Evaluates Nichols data directly from the factored loop transfer.
    pub fn nichols_data(&self, angular_frequencies: &[R]) -> Result<NicholsData<R>, LtiError> {
        let samples =
            loop_samples_from_evaluator(angular_frequencies, "nichols_data", true, |w| {
                Ok(self.evaluate(Complex::new(R::zero(), w)))
            })?;
        Ok(NicholsData {
            angular_frequencies: samples.angular_frequencies,
            magnitude_db: samples.magnitude_db,
            phase_deg: samples.phase_deg_unwrapped,
        })
    }

    /// Estimates gain- and phase-crossover frequencies from a sampled loop
    /// grid.
    pub fn loop_crossovers(
        &self,
        angular_frequencies: &[R],
    ) -> Result<LoopCrossovers<R>, LtiError> {
        let samples =
            loop_samples_from_evaluator(angular_frequencies, "loop_crossovers", true, |w| {
                Ok(self.evaluate(Complex::new(R::zero(), w)))
            })?;
        loop_crossovers_from_samples(&samples)
    }

    /// Estimates classical gain and phase margins from a sampled loop grid.
    pub fn loop_margins(&self, angular_frequencies: &[R]) -> Result<LoopMargins<R>, LtiError> {
        let samples =
            loop_samples_from_evaluator(angular_frequencies, "loop_margins", true, |w| {
                Ok(self.evaluate(Complex::new(R::zero(), w)))
            })?;
        loop_margins_from_samples(&samples)
    }
}

impl<R> DiscreteZpk<R>
where
    R: Float + RealField,
{
    /// Returns the unity-feedback sensitivity transfer function
    /// `S = 1 / (1 + L)`.
    pub fn sensitivity(&self) -> Result<Self, LtiError> {
        self.to_transfer_function()?.sensitivity()?.to_zpk()
    }

    /// Returns the unity-feedback complementary sensitivity transfer function
    /// `T = L / (1 + L)`.
    pub fn complementary_sensitivity(&self) -> Result<Self, LtiError> {
        self.to_transfer_function()?
            .complementary_sensitivity()?
            .to_zpk()
    }

    /// Returns the control-sensitivity channel
    /// `KS = C / (1 + P C)` for the supplied controller `C`.
    pub fn control_sensitivity_with(&self, controller: &Self) -> Result<Self, LtiError> {
        self.to_transfer_function()?
            .control_sensitivity_with(&controller.to_transfer_function()?)?
            .to_zpk()
    }

    /// Returns the plant-sensitivity channel
    /// `PS = P / (1 + P C)` for the supplied controller `C`.
    pub fn plant_sensitivity_with(&self, controller: &Self) -> Result<Self, LtiError> {
        self.to_transfer_function()?
            .plant_sensitivity_with(&controller.to_transfer_function()?)?
            .to_zpk()
    }

    /// Evaluates Nyquist data directly from the factored loop transfer.
    pub fn nyquist_data(&self, angular_frequencies: &[R]) -> Result<NyquistData<R>, LtiError> {
        let dt = self.sample_time();
        let samples =
            loop_samples_from_evaluator(angular_frequencies, "nyquist_data", false, |w| {
                let phase = w * dt;
                Ok(self.evaluate(Complex::new(phase.cos(), phase.sin())))
            })?;
        Ok(NyquistData {
            angular_frequencies: samples.angular_frequencies,
            values: samples.values,
        })
    }

    /// Evaluates Nichols data directly from the factored loop transfer.
    pub fn nichols_data(&self, angular_frequencies: &[R]) -> Result<NicholsData<R>, LtiError> {
        let dt = self.sample_time();
        let samples =
            loop_samples_from_evaluator(angular_frequencies, "nichols_data", true, |w| {
                let phase = w * dt;
                Ok(self.evaluate(Complex::new(phase.cos(), phase.sin())))
            })?;
        Ok(NicholsData {
            angular_frequencies: samples.angular_frequencies,
            magnitude_db: samples.magnitude_db,
            phase_deg: samples.phase_deg_unwrapped,
        })
    }

    /// Estimates gain- and phase-crossover frequencies from a sampled loop
    /// grid.
    pub fn loop_crossovers(
        &self,
        angular_frequencies: &[R],
    ) -> Result<LoopCrossovers<R>, LtiError> {
        let dt = self.sample_time();
        let samples =
            loop_samples_from_evaluator(angular_frequencies, "loop_crossovers", true, |w| {
                let phase = w * dt;
                Ok(self.evaluate(Complex::new(phase.cos(), phase.sin())))
            })?;
        loop_crossovers_from_samples(&samples)
    }

    /// Estimates classical gain and phase margins from a sampled loop grid.
    pub fn loop_margins(&self, angular_frequencies: &[R]) -> Result<LoopMargins<R>, LtiError> {
        let dt = self.sample_time();
        let samples =
            loop_samples_from_evaluator(angular_frequencies, "loop_margins", true, |w| {
                let phase = w * dt;
                Ok(self.evaluate(Complex::new(phase.cos(), phase.sin())))
            })?;
        loop_margins_from_samples(&samples)
    }
}

impl<R> ContinuousSos<R>
where
    R: Float + RealField,
{
    /// Returns the unity-feedback sensitivity transfer function
    /// `S = 1 / (1 + L)`.
    pub fn sensitivity(&self) -> Result<Self, LtiError> {
        self.to_transfer_function()?.sensitivity()?.to_sos()
    }

    /// Returns the unity-feedback complementary sensitivity transfer function
    /// `T = L / (1 + L)`.
    pub fn complementary_sensitivity(&self) -> Result<Self, LtiError> {
        self.to_transfer_function()?
            .complementary_sensitivity()?
            .to_sos()
    }

    /// Returns the control-sensitivity channel
    /// `KS = C / (1 + P C)` for the supplied controller `C`.
    pub fn control_sensitivity_with(&self, controller: &Self) -> Result<Self, LtiError> {
        self.to_transfer_function()?
            .control_sensitivity_with(&controller.to_transfer_function()?)?
            .to_sos()
    }

    /// Returns the plant-sensitivity channel
    /// `PS = P / (1 + P C)` for the supplied controller `C`.
    pub fn plant_sensitivity_with(&self, controller: &Self) -> Result<Self, LtiError> {
        self.to_transfer_function()?
            .plant_sensitivity_with(&controller.to_transfer_function()?)?
            .to_sos()
    }

    /// Evaluates Nyquist data directly from the section cascade.
    pub fn nyquist_data(&self, angular_frequencies: &[R]) -> Result<NyquistData<R>, LtiError> {
        let samples =
            loop_samples_from_evaluator(angular_frequencies, "nyquist_data", false, |w| {
                Ok(self.evaluate(Complex::new(R::zero(), w)))
            })?;
        Ok(NyquistData {
            angular_frequencies: samples.angular_frequencies,
            values: samples.values,
        })
    }

    /// Evaluates Nichols data directly from the section cascade.
    pub fn nichols_data(&self, angular_frequencies: &[R]) -> Result<NicholsData<R>, LtiError> {
        let samples =
            loop_samples_from_evaluator(angular_frequencies, "nichols_data", true, |w| {
                Ok(self.evaluate(Complex::new(R::zero(), w)))
            })?;
        Ok(NicholsData {
            angular_frequencies: samples.angular_frequencies,
            magnitude_db: samples.magnitude_db,
            phase_deg: samples.phase_deg_unwrapped,
        })
    }

    /// Estimates gain- and phase-crossover frequencies from a sampled loop
    /// grid.
    pub fn loop_crossovers(
        &self,
        angular_frequencies: &[R],
    ) -> Result<LoopCrossovers<R>, LtiError> {
        let samples =
            loop_samples_from_evaluator(angular_frequencies, "loop_crossovers", true, |w| {
                Ok(self.evaluate(Complex::new(R::zero(), w)))
            })?;
        loop_crossovers_from_samples(&samples)
    }

    /// Estimates classical gain and phase margins from a sampled loop grid.
    pub fn loop_margins(&self, angular_frequencies: &[R]) -> Result<LoopMargins<R>, LtiError> {
        let samples =
            loop_samples_from_evaluator(angular_frequencies, "loop_margins", true, |w| {
                Ok(self.evaluate(Complex::new(R::zero(), w)))
            })?;
        loop_margins_from_samples(&samples)
    }
}

impl<R> DiscreteSos<R>
where
    R: Float + RealField,
{
    /// Returns the unity-feedback sensitivity transfer function
    /// `S = 1 / (1 + L)`.
    pub fn sensitivity(&self) -> Result<Self, LtiError> {
        self.to_transfer_function()?.sensitivity()?.to_sos()
    }

    /// Returns the unity-feedback complementary sensitivity transfer function
    /// `T = L / (1 + L)`.
    pub fn complementary_sensitivity(&self) -> Result<Self, LtiError> {
        self.to_transfer_function()?
            .complementary_sensitivity()?
            .to_sos()
    }

    /// Returns the control-sensitivity channel
    /// `KS = C / (1 + P C)` for the supplied controller `C`.
    pub fn control_sensitivity_with(&self, controller: &Self) -> Result<Self, LtiError> {
        self.to_transfer_function()?
            .control_sensitivity_with(&controller.to_transfer_function()?)?
            .to_sos()
    }

    /// Returns the plant-sensitivity channel
    /// `PS = P / (1 + P C)` for the supplied controller `C`.
    pub fn plant_sensitivity_with(&self, controller: &Self) -> Result<Self, LtiError> {
        self.to_transfer_function()?
            .plant_sensitivity_with(&controller.to_transfer_function()?)?
            .to_sos()
    }

    /// Evaluates Nyquist data directly from the section cascade.
    pub fn nyquist_data(&self, angular_frequencies: &[R]) -> Result<NyquistData<R>, LtiError> {
        let dt = self.sample_time();
        let samples =
            loop_samples_from_evaluator(angular_frequencies, "nyquist_data", false, |w| {
                let phase = w * dt;
                Ok(self.evaluate(Complex::new(phase.cos(), phase.sin())))
            })?;
        Ok(NyquistData {
            angular_frequencies: samples.angular_frequencies,
            values: samples.values,
        })
    }

    /// Evaluates Nichols data directly from the section cascade.
    pub fn nichols_data(&self, angular_frequencies: &[R]) -> Result<NicholsData<R>, LtiError> {
        let dt = self.sample_time();
        let samples =
            loop_samples_from_evaluator(angular_frequencies, "nichols_data", true, |w| {
                let phase = w * dt;
                Ok(self.evaluate(Complex::new(phase.cos(), phase.sin())))
            })?;
        Ok(NicholsData {
            angular_frequencies: samples.angular_frequencies,
            magnitude_db: samples.magnitude_db,
            phase_deg: samples.phase_deg_unwrapped,
        })
    }

    /// Estimates gain- and phase-crossover frequencies from a sampled loop
    /// grid.
    pub fn loop_crossovers(
        &self,
        angular_frequencies: &[R],
    ) -> Result<LoopCrossovers<R>, LtiError> {
        let dt = self.sample_time();
        let samples =
            loop_samples_from_evaluator(angular_frequencies, "loop_crossovers", true, |w| {
                let phase = w * dt;
                Ok(self.evaluate(Complex::new(phase.cos(), phase.sin())))
            })?;
        loop_crossovers_from_samples(&samples)
    }

    /// Estimates classical gain and phase margins from a sampled loop grid.
    pub fn loop_margins(&self, angular_frequencies: &[R]) -> Result<LoopMargins<R>, LtiError> {
        let dt = self.sample_time();
        let samples =
            loop_samples_from_evaluator(angular_frequencies, "loop_margins", true, |w| {
                let phase = w * dt;
                Ok(self.evaluate(Complex::new(phase.cos(), phase.sin())))
            })?;
        loop_margins_from_samples(&samples)
    }
}

impl<R> ContinuousStateSpace<R>
where
    R: Float + RealField,
{
    /// Returns the unity-feedback sensitivity transfer function
    /// `S = 1 / (1 + L)` for the represented SISO loop transfer.
    pub fn sensitivity(&self) -> Result<ContinuousTransferFunction<R>, LtiError> {
        self.to_transfer_function()?.sensitivity()
    }

    /// Returns the unity-feedback complementary sensitivity transfer function
    /// `T = L / (1 + L)` for the represented SISO loop transfer.
    pub fn complementary_sensitivity(&self) -> Result<ContinuousTransferFunction<R>, LtiError> {
        self.to_transfer_function()?.complementary_sensitivity()
    }

    /// Returns the control-sensitivity channel
    /// `KS = C / (1 + P C)` for the supplied controller `C`.
    pub fn control_sensitivity_with(
        &self,
        controller: &ContinuousTransferFunction<R>,
    ) -> Result<ContinuousTransferFunction<R>, LtiError> {
        self.to_transfer_function()?
            .control_sensitivity_with(controller)
    }

    /// Returns the plant-sensitivity channel
    /// `PS = P / (1 + P C)` for the supplied controller `C`.
    pub fn plant_sensitivity_with(
        &self,
        controller: &ContinuousTransferFunction<R>,
    ) -> Result<ContinuousTransferFunction<R>, LtiError> {
        self.to_transfer_function()?
            .plant_sensitivity_with(controller)
    }

    /// Evaluates Nyquist data for a dense real SISO loop transfer.
    pub fn nyquist_data(&self, angular_frequencies: &[R]) -> Result<NyquistData<R>, LtiError> {
        ensure_siso_state_space(self)?;
        let samples =
            loop_samples_from_evaluator(angular_frequencies, "nyquist_data", false, |w| {
                self.transfer_at(Complex::new(R::zero(), w))
                    .map(|value| value[(0, 0)])
            })?;
        Ok(NyquistData {
            angular_frequencies: samples.angular_frequencies,
            values: samples.values,
        })
    }

    /// Evaluates Nichols data for a dense real SISO loop transfer.
    pub fn nichols_data(&self, angular_frequencies: &[R]) -> Result<NicholsData<R>, LtiError> {
        ensure_siso_state_space(self)?;
        let samples =
            loop_samples_from_evaluator(angular_frequencies, "nichols_data", true, |w| {
                self.transfer_at(Complex::new(R::zero(), w))
                    .map(|value| value[(0, 0)])
            })?;
        Ok(NicholsData {
            angular_frequencies: samples.angular_frequencies,
            magnitude_db: samples.magnitude_db,
            phase_deg: samples.phase_deg_unwrapped,
        })
    }

    /// Estimates gain- and phase-crossover frequencies from a sampled loop
    /// grid.
    pub fn loop_crossovers(
        &self,
        angular_frequencies: &[R],
    ) -> Result<LoopCrossovers<R>, LtiError> {
        ensure_siso_state_space(self)?;
        let samples =
            loop_samples_from_evaluator(angular_frequencies, "loop_crossovers", true, |w| {
                self.transfer_at(Complex::new(R::zero(), w))
                    .map(|value| value[(0, 0)])
            })?;
        loop_crossovers_from_samples(&samples)
    }

    /// Estimates classical gain and phase margins from a sampled loop grid.
    pub fn loop_margins(&self, angular_frequencies: &[R]) -> Result<LoopMargins<R>, LtiError> {
        ensure_siso_state_space(self)?;
        let samples =
            loop_samples_from_evaluator(angular_frequencies, "loop_margins", true, |w| {
                self.transfer_at(Complex::new(R::zero(), w))
                    .map(|value| value[(0, 0)])
            })?;
        loop_margins_from_samples(&samples)
    }
}

impl<R> DiscreteStateSpace<R>
where
    R: Float + RealField,
{
    /// Returns the unity-feedback sensitivity transfer function
    /// `S = 1 / (1 + L)` for the represented SISO loop transfer.
    pub fn sensitivity(&self) -> Result<DiscreteTransferFunction<R>, LtiError> {
        self.to_transfer_function()?.sensitivity()
    }

    /// Returns the unity-feedback complementary sensitivity transfer function
    /// `T = L / (1 + L)` for the represented SISO loop transfer.
    pub fn complementary_sensitivity(&self) -> Result<DiscreteTransferFunction<R>, LtiError> {
        self.to_transfer_function()?.complementary_sensitivity()
    }

    /// Returns the control-sensitivity channel
    /// `KS = C / (1 + P C)` for the supplied controller `C`.
    pub fn control_sensitivity_with(
        &self,
        controller: &DiscreteTransferFunction<R>,
    ) -> Result<DiscreteTransferFunction<R>, LtiError> {
        self.to_transfer_function()?
            .control_sensitivity_with(controller)
    }

    /// Returns the plant-sensitivity channel
    /// `PS = P / (1 + P C)` for the supplied controller `C`.
    pub fn plant_sensitivity_with(
        &self,
        controller: &DiscreteTransferFunction<R>,
    ) -> Result<DiscreteTransferFunction<R>, LtiError> {
        self.to_transfer_function()?
            .plant_sensitivity_with(controller)
    }

    /// Evaluates Nyquist data for a dense real SISO loop transfer.
    pub fn nyquist_data(&self, angular_frequencies: &[R]) -> Result<NyquistData<R>, LtiError> {
        ensure_siso_state_space(self)?;
        let dt = self.sample_time();
        let samples =
            loop_samples_from_evaluator(angular_frequencies, "nyquist_data", false, |w| {
                let phase = w * dt;
                self.transfer_at(Complex::new(phase.cos(), phase.sin()))
                    .map(|value| value[(0, 0)])
            })?;
        Ok(NyquistData {
            angular_frequencies: samples.angular_frequencies,
            values: samples.values,
        })
    }

    /// Evaluates Nichols data for a dense real SISO loop transfer.
    pub fn nichols_data(&self, angular_frequencies: &[R]) -> Result<NicholsData<R>, LtiError> {
        ensure_siso_state_space(self)?;
        let dt = self.sample_time();
        let samples =
            loop_samples_from_evaluator(angular_frequencies, "nichols_data", true, |w| {
                let phase = w * dt;
                self.transfer_at(Complex::new(phase.cos(), phase.sin()))
                    .map(|value| value[(0, 0)])
            })?;
        Ok(NicholsData {
            angular_frequencies: samples.angular_frequencies,
            magnitude_db: samples.magnitude_db,
            phase_deg: samples.phase_deg_unwrapped,
        })
    }

    /// Estimates gain- and phase-crossover frequencies from a sampled loop
    /// grid.
    pub fn loop_crossovers(
        &self,
        angular_frequencies: &[R],
    ) -> Result<LoopCrossovers<R>, LtiError> {
        ensure_siso_state_space(self)?;
        let dt = self.sample_time();
        let samples =
            loop_samples_from_evaluator(angular_frequencies, "loop_crossovers", true, |w| {
                let phase = w * dt;
                self.transfer_at(Complex::new(phase.cos(), phase.sin()))
                    .map(|value| value[(0, 0)])
            })?;
        loop_crossovers_from_samples(&samples)
    }

    /// Estimates classical gain and phase margins from a sampled loop grid.
    pub fn loop_margins(&self, angular_frequencies: &[R]) -> Result<LoopMargins<R>, LtiError> {
        ensure_siso_state_space(self)?;
        let dt = self.sample_time();
        let samples =
            loop_samples_from_evaluator(angular_frequencies, "loop_margins", true, |w| {
                let phase = w * dt;
                self.transfer_at(Complex::new(phase.cos(), phase.sin()))
                    .map(|value| value[(0, 0)])
            })?;
        loop_margins_from_samples(&samples)
    }
}

fn identity_transfer<R, Domain>(domain: Domain) -> Result<TransferFunction<R, Domain>, LtiError>
where
    R: Float + RealField,
    Domain: Clone,
{
    TransferFunction::new(vec![R::one()], vec![R::one()], domain)
}

fn ensure_siso_state_space<R, Domain>(
    system: &super::StateSpace<R, Domain>,
) -> Result<(), LtiError> {
    if system.is_siso() {
        Ok(())
    } else {
        Err(LtiError::NonSisoStateSpace {
            ninputs: system.ninputs(),
            noutputs: system.noutputs(),
        })
    }
}

fn loop_samples_from_evaluator<R, F>(
    angular_frequencies: &[R],
    which: &'static str,
    require_monotone: bool,
    mut evaluate: F,
) -> Result<LoopSamples<R>, LtiError>
where
    R: Float + RealField,
    F: FnMut(R) -> Result<Complex<R>, LtiError>,
{
    validate_frequency_grid(angular_frequencies, which, require_monotone)?;

    let mut values = Vec::with_capacity(angular_frequencies.len());
    let mut magnitude_abs = Vec::with_capacity(angular_frequencies.len());
    let mut magnitude_db = Vec::with_capacity(angular_frequencies.len());
    let mut phase_deg_wrapped = Vec::with_capacity(angular_frequencies.len());

    // Sample once and cache every derived quantity that the later Nyquist,
    // Nichols, crossover, and margin helpers need. This keeps all of those
    // views numerically aligned to the exact same evaluated loop data.
    for &omega in angular_frequencies {
        let value = evaluate(omega)?;
        if !value.re.is_finite() || !value.im.is_finite() {
            return Err(LtiError::NonFiniteResult { which });
        }
        let magnitude = value.abs();
        values.push(value);
        magnitude_abs.push(magnitude);
        magnitude_db.push(from_f64::<R>(20.0) * magnitude.log10());
        phase_deg_wrapped.push(value.im.atan2(value.re).to_degrees());
    }

    Ok(LoopSamples {
        angular_frequencies: angular_frequencies.to_vec(),
        values,
        magnitude_abs,
        magnitude_db,
        phase_deg_unwrapped: unwrap_phase_deg(&phase_deg_wrapped),
    })
}

fn validate_frequency_grid<R>(
    angular_frequencies: &[R],
    which: &'static str,
    require_monotone: bool,
) -> Result<(), LtiError>
where
    R: Float + RealField,
{
    if require_monotone {
        validate_nonnegative_monotone_grid(angular_frequencies, which)
    } else {
        for &omega in angular_frequencies {
            if !omega.is_finite() || omega < R::zero() {
                return Err(LtiError::InvalidSamplePoint { which });
            }
        }
        Ok(())
    }
}

fn loop_crossovers_from_samples<R>(samples: &LoopSamples<R>) -> Result<LoopCrossovers<R>, LtiError>
where
    R: Float + RealField,
{
    Ok(LoopCrossovers {
        gain_crossovers: gain_crossovers(
            &samples.angular_frequencies,
            &samples.magnitude_abs,
            &samples.phase_deg_unwrapped,
        )
        .into_iter()
        .map(|item| item.frequency)
        .collect(),
        phase_crossovers: phase_crossovers(
            &samples.angular_frequencies,
            &samples.phase_deg_unwrapped,
            &samples.magnitude_db,
        )
        .into_iter()
        .map(|item| item.frequency)
        .collect(),
    })
}

fn loop_margins_from_samples<R>(samples: &LoopSamples<R>) -> Result<LoopMargins<R>, LtiError>
where
    R: Float + RealField,
{
    // Classical gain and phase margins are defined from the first unity-gain
    // and `-180 deg` crossings, respectively, so we derive both crossover sets
    // first and then pick the leading event from each.
    let gain_crossovers = gain_crossovers(
        &samples.angular_frequencies,
        &samples.magnitude_abs,
        &samples.phase_deg_unwrapped,
    );
    let phase_crossovers = phase_crossovers(
        &samples.angular_frequencies,
        &samples.phase_deg_unwrapped,
        &samples.magnitude_db,
    );

    let phase_margin = gain_crossovers
        .first()
        .map(|cross| from_f64::<R>(180.0) + cross.phase_deg);
    let (gain_margin_abs, gain_margin_db, phase_crossover) = match phase_crossovers.first() {
        Some(cross) => {
            let gm_db = -cross.magnitude_db;
            let gm_abs = from_f64::<R>(10.0).powf(gm_db / from_f64::<R>(20.0));
            (Some(gm_abs), Some(gm_db), Some(cross.frequency))
        }
        None => (None, None, None),
    };

    Ok(LoopMargins {
        gain_margin_abs,
        gain_margin_db,
        phase_margin_deg: phase_margin,
        gain_crossover: gain_crossovers.first().map(|cross| cross.frequency),
        phase_crossover,
    })
}

fn gain_crossovers<R>(
    angular_frequencies: &[R],
    magnitude_abs: &[R],
    phase_deg_unwrapped: &[R],
) -> Vec<GainCrossover<R>>
where
    R: Float + RealField,
{
    let mut out = Vec::new();
    let one = R::one();
    for idx in 0..magnitude_abs.len().saturating_sub(1) {
        let y0 = magnitude_abs[idx] - one;
        let y1 = magnitude_abs[idx + 1] - one;
        let w0 = angular_frequencies[idx];
        let w1 = angular_frequencies[idx + 1];
        let tol = scalar_tol(y0.abs().max(y1.abs()));

        if y0.abs() <= tol {
            push_unique_frequency(
                &mut out,
                GainCrossover {
                    frequency: w0,
                    phase_deg: phase_deg_unwrapped[idx],
                },
            );
        }
        if (y0 < R::zero() && y1 > R::zero()) || (y0 > R::zero() && y1 < R::zero()) {
            // Interpolate the unity-gain crossing on the sampled magnitude
            // trace, then read the local phase from the same grid interval so
            // the phase-margin calculation uses a consistent point.
            let frequency = interpolate_scalar(y0, y1, w0, w1, R::zero());
            let phase_deg = interpolate_scalar(
                w0,
                w1,
                phase_deg_unwrapped[idx],
                phase_deg_unwrapped[idx + 1],
                frequency,
            );
            push_unique_frequency(
                &mut out,
                GainCrossover {
                    frequency,
                    phase_deg,
                },
            );
        }
    }

    if let (Some(&w_last), Some(&y_last)) = (angular_frequencies.last(), magnitude_abs.last()) {
        let y_last = y_last - one;
        if y_last.abs() <= scalar_tol(y_last.abs()) {
            push_unique_frequency(
                &mut out,
                GainCrossover {
                    frequency: w_last,
                    phase_deg: *phase_deg_unwrapped.last().unwrap_or(&R::zero()),
                },
            );
        }
    }

    out
}

fn phase_crossovers<R>(
    angular_frequencies: &[R],
    phase_deg_unwrapped: &[R],
    magnitude_db: &[R],
) -> Vec<PhaseCrossover<R>>
where
    R: Float + RealField,
{
    let mut out = Vec::new();
    let one_eighty = from_f64::<R>(180.0);
    let three_sixty = from_f64::<R>(360.0);

    for idx in 0..phase_deg_unwrapped.len().saturating_sub(1) {
        let p0 = phase_deg_unwrapped[idx];
        let p1 = phase_deg_unwrapped[idx + 1];
        let w0 = angular_frequencies[idx];
        let w1 = angular_frequencies[idx + 1];
        let m0 = magnitude_db[idx];
        let m1 = magnitude_db[idx + 1];

        let lo = p0.min(p1);
        let hi = p0.max(p1);
        let k_min = ((-hi - one_eighty) / three_sixty)
            .ceil()
            .to_i64()
            .unwrap_or(0);
        let k_max = ((-lo - one_eighty) / three_sixty)
            .floor()
            .to_i64()
            .unwrap_or(-1);

        if approx_eq(p0, p1) {
            continue;
        }

        // Unwrapping makes the phase trace continuous, so every `-180 - 360k`
        // crossing in this interval can be enumerated explicitly and then
        // interpolated back onto the frequency and magnitude traces.
        for k in k_min..=k_max {
            let target = -one_eighty - three_sixty * from_f64::<R>(k as f64);
            let frequency = interpolate_scalar(p0, p1, w0, w1, target);
            let magnitude_db = interpolate_scalar(p0, p1, m0, m1, target);
            push_unique_phase_crossover(
                &mut out,
                PhaseCrossover {
                    frequency,
                    magnitude_db,
                },
            );
        }
    }

    out
}

fn interpolate_scalar<R>(x0: R, x1: R, y0: R, y1: R, target_x: R) -> R
where
    R: Float + RealField,
{
    if approx_eq(x0, x1) {
        return y0;
    }
    let alpha = (target_x - x0) / (x1 - x0);
    mul_add(alpha, y1 - y0, y0)
}

fn approx_eq<R>(lhs: R, rhs: R) -> bool
where
    R: Float + RealField,
{
    let scale = lhs.abs().max(rhs.abs()).max(R::one());
    (lhs - rhs).abs() <= scalar_tol(scale)
}

fn scalar_tol<R>(scale: R) -> R
where
    R: Float + RealField,
{
    scale * from_f64::<R>(128.0) * eps::<R>().sqrt()
}

fn push_unique_frequency<R>(out: &mut Vec<GainCrossover<R>>, value: GainCrossover<R>)
where
    R: Float + RealField,
{
    if out
        .iter()
        .any(|existing| approx_eq(existing.frequency, value.frequency))
    {
        return;
    }
    out.push(value);
}

fn push_unique_phase_crossover<R>(out: &mut Vec<PhaseCrossover<R>>, value: PhaseCrossover<R>)
where
    R: Float + RealField,
{
    if out
        .iter()
        .any(|existing| approx_eq(existing.frequency, value.frequency))
    {
        return;
    }
    out.push(value);
}

#[cfg(test)]
mod tests {
    use super::{ContinuousTransferFunction, DiscreteTransferFunction};
    use crate::control::lti::LtiError;
    use alloc::vec::Vec;
    use nalgebra::ComplexField;

    fn assert_close(lhs: f64, rhs: f64, tol: f64) {
        let err = (lhs - rhs).abs();
        assert!(err <= tol, "lhs={lhs}, rhs={rhs}, err={err}, tol={tol}");
    }

    #[test]
    fn sensitivity_and_complementary_sensitivity_sum_to_one() {
        let loop_tf =
            ContinuousTransferFunction::continuous(vec![2.0], vec![1.0, 3.0, 2.0]).unwrap();
        let s = loop_tf.sensitivity().unwrap();
        let t = loop_tf.complementary_sensitivity().unwrap();
        let point = faer::complex::Complex::new(0.0, 1.5);
        let sum = s.evaluate(point) + t.evaluate(point);
        assert!((sum - faer::complex::Complex::new(1.0, 0.0)).abs() <= 1.0e-12);
    }

    #[test]
    fn continuous_loop_margins_match_known_third_order_example() {
        let loop_tf =
            ContinuousTransferFunction::continuous(vec![2.0], vec![1.0, 3.0, 3.0, 1.0]).unwrap();
        let grid = (0..4001)
            .map(|idx| 5.0 * idx as f64 / 4000.0)
            .collect::<Vec<_>>();
        let margins = loop_tf.loop_margins(&grid).unwrap();

        let gain_crossover = (2.0f64.powf(2.0 / 3.0) - 1.0).sqrt();
        let phase_margin = 180.0 - 3.0 * gain_crossover.atan().to_degrees();
        let phase_crossover = 3.0f64.sqrt();
        let gain_margin_abs = 4.0f64;
        let gain_margin_db = 20.0 * gain_margin_abs.log10();

        assert_close(margins.gain_crossover.unwrap(), gain_crossover, 2.0e-3);
        assert_close(margins.phase_margin_deg.unwrap(), phase_margin, 2.0e-2);
        assert_close(margins.phase_crossover.unwrap(), phase_crossover, 2.0e-3);
        assert_close(margins.gain_margin_abs.unwrap(), gain_margin_abs, 2.0e-2);
        assert_close(margins.gain_margin_db.unwrap(), gain_margin_db, 2.0e-2);
    }

    #[test]
    fn discrete_loop_margins_return_none_when_no_crossings_are_found() {
        let loop_tf = DiscreteTransferFunction::discrete(vec![0.2], vec![1.0, -0.6], 0.1).unwrap();
        let grid = (0..2001)
            .map(|idx| 30.0 * idx as f64 / 2000.0)
            .collect::<Vec<_>>();
        let margins = loop_tf.loop_margins(&grid).unwrap();
        assert!(margins.gain_margin_abs.is_none());
        assert!(margins.gain_margin_db.is_none());
        assert!(margins.phase_margin_deg.is_none());
        assert!(margins.gain_crossover.is_none());
        assert!(margins.phase_crossover.is_none());
    }

    #[test]
    fn loop_data_and_margins_match_across_representations() {
        let loop_tf =
            ContinuousTransferFunction::continuous(vec![2.0], vec![1.0, 3.0, 3.0, 1.0]).unwrap();
        let loop_zpk = loop_tf.to_zpk().unwrap();
        let loop_sos = loop_tf.to_sos().unwrap();
        let loop_ss = loop_tf.to_state_space().unwrap();
        let grid = (0..801)
            .map(|idx| 5.0 * idx as f64 / 800.0)
            .collect::<Vec<_>>();

        let tf_nyquist = loop_tf.nyquist_data(&grid).unwrap();
        let zpk_nyquist = loop_zpk.nyquist_data(&grid).unwrap();
        let sos_nyquist = loop_sos.nyquist_data(&grid).unwrap();
        let ss_nyquist = loop_ss.nyquist_data(&grid).unwrap();
        for idx in 0..grid.len() {
            assert!((tf_nyquist.values[idx] - zpk_nyquist.values[idx]).abs() <= 1.0e-10);
            assert!((tf_nyquist.values[idx] - sos_nyquist.values[idx]).abs() <= 1.0e-10);
            assert!((tf_nyquist.values[idx] - ss_nyquist.values[idx]).abs() <= 1.0e-10);
        }

        let tf_nichols = loop_tf.nichols_data(&grid).unwrap();
        let zpk_nichols = loop_zpk.nichols_data(&grid).unwrap();
        let sos_nichols = loop_sos.nichols_data(&grid).unwrap();
        let ss_nichols = loop_ss.nichols_data(&grid).unwrap();
        for idx in 0..grid.len() {
            assert_close(
                tf_nichols.magnitude_db[idx],
                zpk_nichols.magnitude_db[idx],
                1.0e-10,
            );
            assert_close(
                tf_nichols.magnitude_db[idx],
                sos_nichols.magnitude_db[idx],
                1.0e-10,
            );
            assert_close(
                tf_nichols.magnitude_db[idx],
                ss_nichols.magnitude_db[idx],
                1.0e-10,
            );
            assert_close(
                tf_nichols.phase_deg[idx],
                zpk_nichols.phase_deg[idx],
                1.0e-10,
            );
            assert_close(
                tf_nichols.phase_deg[idx],
                sos_nichols.phase_deg[idx],
                1.0e-10,
            );
            assert_close(
                tf_nichols.phase_deg[idx],
                ss_nichols.phase_deg[idx],
                1.0e-10,
            );
        }

        let tf_margins = loop_tf.loop_margins(&grid).unwrap();
        let zpk_margins = loop_zpk.loop_margins(&grid).unwrap();
        let sos_margins = loop_sos.loop_margins(&grid).unwrap();
        let ss_margins = loop_ss.loop_margins(&grid).unwrap();

        assert_close(
            tf_margins.phase_margin_deg.unwrap(),
            zpk_margins.phase_margin_deg.unwrap(),
            1.0e-8,
        );
        assert_close(
            tf_margins.phase_margin_deg.unwrap(),
            sos_margins.phase_margin_deg.unwrap(),
            1.0e-8,
        );
        assert_close(
            tf_margins.phase_margin_deg.unwrap(),
            ss_margins.phase_margin_deg.unwrap(),
            1.0e-8,
        );
        assert_close(
            tf_margins.gain_margin_db.unwrap(),
            zpk_margins.gain_margin_db.unwrap(),
            1.0e-8,
        );
    }

    #[test]
    fn plant_and_control_sensitivity_match_closed_form_arithmetic() {
        let plant = ContinuousTransferFunction::continuous(vec![2.0], vec![1.0, 1.0]).unwrap();
        let controller =
            ContinuousTransferFunction::continuous(vec![1.0, 3.0], vec![1.0, 4.0]).unwrap();

        let ps = plant.plant_sensitivity_with(&controller).unwrap();
        let ks = plant.control_sensitivity_with(&controller).unwrap();
        let loop_tf = plant.mul(&controller).unwrap();
        let expected_ps = plant.feedback(&controller).unwrap();
        let expected_ks = controller.feedback(&plant).unwrap();
        let point = faer::complex::Complex::new(0.0, 2.5);

        assert!((ps.evaluate(point) - expected_ps.evaluate(point)).abs() <= 1.0e-12);
        assert!((ks.evaluate(point) - expected_ks.evaluate(point)).abs() <= 1.0e-12);
        let lhs = ks.evaluate(point);
        let rhs = controller.evaluate(point)
            / (faer::complex::Complex::new(1.0, 0.0) + loop_tf.evaluate(point));
        assert!((lhs - rhs).abs() <= 1.0e-12);
    }

    #[test]
    fn nichols_data_rejects_unsorted_frequency_grid() {
        let loop_tf =
            ContinuousTransferFunction::continuous(vec![1.0], vec![1.0, 3.0, 3.0, 1.0]).unwrap();

        assert!(matches!(
            loop_tf.nichols_data(&[100.0, 1.0]),
            Err(LtiError::InvalidSampleGrid {
                which: "nichols_data"
            })
        ));
    }
}
