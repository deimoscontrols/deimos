//! Continuous delay-aware process models.
//!
//! These models keep transport delay explicit instead of forcing it through the
//! rational transfer-function layer. That makes them a better fit for
//! process-control workflows and for PID tuning rules that reason directly in
//! terms of dead time.
//!
//! They are still linear and time-invariant, but they are not finite-order
//! rational transfer functions because of the explicit `exp(-Ls)` delay term.
//!
//! # Two Intuitions
//!
//! 1. **Process-control view.** These models capture the dominant gain, lag,
//!    and delay that matter for everyday tuning and process interpretation.
//! 2. **Delay-honest view.** They also serve as a reminder that pure delay is
//!    linear but not a finite-order rational transfer function, so it deserves
//!    an explicit model surface instead of being hidden in a polynomial proxy.
//!
//! # Glossary
//!
//! - **FOPDT:** First-order-plus-dead-time model.
//! - **SOPDT:** Second-order-plus-dead-time model.
//! - **Dead time / transport delay:** Pure delay factor `exp(-L s)`.
//!
//! # Mathematical Formulation
//!
//! The two model families exposed here are:
//!
//! - `FOPDT: K exp(-L s) / (tau s + 1)`
//! - `SOPDT: K exp(-L s) / ((tau_1 s + 1)(tau_2 s + 1))`
//!
//! Frequency response evaluates the delay exactly, and time-domain step
//! response helpers use the corresponding delayed closed forms.
//!
//! # Implementation Notes
//!
//! - The models are continuous-time only in this layer.
//! - Delay is preserved explicitly in analysis helpers rather than converted to
//!   a rational approximation.
//! - These public types are shared with the PID-design layer so fitted process
//!   models and hand-specified ones follow the same contract.

use super::{BodeData, LtiError, NicholsData, NyquistData, util::unwrap_phase_deg};
use crate::scalar::{mul_add, neg_mul_add, real_complex_mul_add};
use alloc::vec::Vec;
use faer::complex::Complex;
use faer_traits::RealField;
use faer_traits::ext::ComplexFieldExt;
use num_traits::Float;

/// First-order-plus-dead-time process model.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FopdtModel<R> {
    /// Steady-state process gain.
    pub gain: R,
    /// Dominant first-order lag.
    pub time_constant: R,
    /// Input-output dead time.
    pub delay: R,
}

/// Step-response Jacobian of an `FOPDT` model at one sample time.
///
/// Each field is the partial derivative of
/// [`FopdtModel::step_response_value`] with respect to the corresponding model
/// parameter, keeping the step amplitude and baseline fixed.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FopdtStepResponseJacobian<R> {
    /// Partial derivative with respect to the process gain.
    pub gain: R,
    /// Partial derivative with respect to the first-order time constant.
    pub time_constant: R,
    /// Partial derivative with respect to the dead time.
    pub delay: R,
}

impl<R> FopdtModel<R>
where
    R: Float + RealField,
{
    /// Evaluates the continuous-time transfer map
    /// `K * exp(-Ls) / (tau s + 1)`.
    #[must_use]
    pub fn evaluate(&self, s: Complex<R>) -> Complex<R> {
        // Keep the transport delay exact in frequency-domain evaluation rather
        // than approximating it through a rational surrogate.
        let delay_factor = complex_exp(-s * self.delay);
        Complex::new(self.gain, R::zero()) * delay_factor
            / real_complex_mul_add(self.time_constant, s, Complex::new(R::one(), R::zero()))
    }

    /// Returns the steady-state gain.
    #[must_use]
    pub fn dc_gain(&self) -> R {
        self.gain
    }

    /// Evaluates the delayed unit-step response for a step of amplitude
    /// `step_amplitude` and baseline `initial_output`.
    #[must_use]
    pub fn step_response_value(
        &self,
        time_since_step: R,
        step_amplitude: R,
        initial_output: R,
    ) -> R {
        if time_since_step <= self.delay {
            initial_output
        } else {
            let theta = time_since_step - self.delay;
            mul_add(
                self.gain * step_amplitude,
                R::one() - (-(theta / self.time_constant)).exp(),
                initial_output,
            )
        }
    }

    /// Evaluates the delayed unit-step response Jacobian with respect to the
    /// physical `FOPDT` parameters.
    ///
    /// For `time_since_step <= delay`, the delayed model output is still in
    /// its dead-time plateau, so all partial derivatives are zero.
    #[must_use]
    pub fn step_response_jacobian_value(
        &self,
        time_since_step: R,
        step_amplitude: R,
    ) -> FopdtStepResponseJacobian<R> {
        if time_since_step <= self.delay {
            return FopdtStepResponseJacobian {
                gain: R::zero(),
                time_constant: R::zero(),
                delay: R::zero(),
            };
        }
        let theta = time_since_step - self.delay;
        let tau = self.time_constant;
        let exponential = (-(theta / tau)).exp();
        let scale = self.gain * step_amplitude;
        FopdtStepResponseJacobian {
            gain: step_amplitude * (R::one() - exponential),
            time_constant: -(scale * exponential * theta) / (tau * tau),
            delay: -(scale * exponential) / tau,
        }
    }

    /// Samples the delayed step response on the supplied time grid.
    pub fn step_response_values(
        &self,
        sample_times: &[R],
        step_time: R,
        step_amplitude: R,
        initial_output: R,
    ) -> Result<Vec<R>, LtiError> {
        validate_step_grid(sample_times)?;
        Ok(sample_times
            .iter()
            .map(|&time| {
                // The public step-response helper keeps the same "delay is a
                // pure dead zone" convention as the fitting and PID-tuning
                // path.
                self.step_response_value(
                    (time - step_time).max(R::zero()),
                    step_amplitude,
                    initial_output,
                )
            })
            .collect())
    }

    /// Samples Nyquist data on a continuous-time angular-frequency grid.
    pub fn nyquist_data(&self, angular_frequencies: &[R]) -> Result<NyquistData<R>, LtiError> {
        validate_frequency_grid(angular_frequencies, "nyquist_data")?;
        Ok(NyquistData {
            angular_frequencies: angular_frequencies.to_vec(),
            values: angular_frequencies
                .iter()
                .map(|&omega| self.evaluate(Complex::new(R::zero(), omega)))
                .collect(),
        })
    }

    /// Samples Nichols data on a continuous-time angular-frequency grid.
    pub fn nichols_data(&self, angular_frequencies: &[R]) -> Result<NicholsData<R>, LtiError> {
        let bode = self.bode_data(angular_frequencies)?;
        Ok(NicholsData {
            angular_frequencies: bode.angular_frequencies,
            magnitude_db: bode.magnitude_db,
            phase_deg: bode.phase_deg,
        })
    }

    /// Samples Bode magnitude and unwrapped phase on a continuous-time
    /// angular-frequency grid.
    pub fn bode_data(&self, angular_frequencies: &[R]) -> Result<BodeData<R>, LtiError> {
        validate_frequency_grid(angular_frequencies, "bode_data")?;
        let values = angular_frequencies
            .iter()
            .map(|&omega| self.evaluate(Complex::new(R::zero(), omega)))
            .collect::<Vec<_>>();
        let magnitude_db = values
            .iter()
            .map(|value| R::from(20.0).unwrap() * value.abs().log10())
            .collect::<Vec<_>>();
        let phase_deg = unwrap_phase_deg(
            &values
                .iter()
                .map(|value| {
                    value.im.atan2(value.re) * R::from(180.0 / core::f64::consts::PI).unwrap()
                })
                .collect::<Vec<_>>(),
        );
        Ok(BodeData {
            angular_frequencies: angular_frequencies.to_vec(),
            magnitude_db,
            phase_deg,
        })
    }
}

/// Second-order-plus-dead-time process model with two real lags.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SopdtModel<R> {
    /// Steady-state process gain.
    pub gain: R,
    /// Slower first-order lag.
    pub time_constant_1: R,
    /// Faster first-order lag.
    pub time_constant_2: R,
    /// Input-output dead time.
    pub delay: R,
}

/// Step-response Jacobian of an `SOPDT` model at one sample time.
///
/// Each field is the partial derivative of
/// [`SopdtModel::step_response_value`] with respect to the corresponding model
/// parameter, keeping the step amplitude and baseline fixed.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SopdtStepResponseJacobian<R> {
    /// Partial derivative with respect to the process gain.
    pub gain: R,
    /// Partial derivative with respect to the slower lag.
    pub time_constant_1: R,
    /// Partial derivative with respect to the faster lag.
    pub time_constant_2: R,
    /// Partial derivative with respect to the dead time.
    pub delay: R,
}

impl<R> SopdtModel<R>
where
    R: Float + RealField,
{
    /// Evaluates the continuous-time transfer map
    /// `K * exp(-Ls) / ((tau1 s + 1)(tau2 s + 1))`.
    #[must_use]
    pub fn evaluate(&self, s: Complex<R>) -> Complex<R> {
        // As in the FOPDT path, preserve the explicit transport delay exactly
        // in frequency-domain evaluation.
        let delay_factor = complex_exp(-s * self.delay);
        let d1 = real_complex_mul_add(self.time_constant_1, s, Complex::new(R::one(), R::zero()));
        let d2 = real_complex_mul_add(self.time_constant_2, s, Complex::new(R::one(), R::zero()));
        Complex::new(self.gain, R::zero()) * delay_factor / (d1 * d2)
    }

    /// Returns the steady-state gain.
    #[must_use]
    pub fn dc_gain(&self) -> R {
        self.gain
    }

    /// Evaluates the delayed second-order step response for a step of
    /// amplitude `step_amplitude` and baseline `initial_output`.
    #[must_use]
    pub fn step_response_value(
        &self,
        time_since_step: R,
        step_amplitude: R,
        initial_output: R,
    ) -> R {
        if time_since_step <= self.delay {
            return initial_output;
        }
        let theta = time_since_step - self.delay;
        let lag = second_order_lag_step(theta, self.time_constant_1, self.time_constant_2);
        mul_add(self.gain * step_amplitude, lag, initial_output)
    }

    /// Evaluates the delayed unit-step response Jacobian with respect to the
    /// physical `SOPDT` parameters.
    ///
    /// For `time_since_step <= delay`, the delayed model output is still in
    /// its dead-time plateau, so all partial derivatives are zero.
    #[must_use]
    pub fn step_response_jacobian_value(
        &self,
        time_since_step: R,
        step_amplitude: R,
    ) -> SopdtStepResponseJacobian<R> {
        if time_since_step <= self.delay {
            return SopdtStepResponseJacobian {
                gain: R::zero(),
                time_constant_1: R::zero(),
                time_constant_2: R::zero(),
                delay: R::zero(),
            };
        }
        let theta = time_since_step - self.delay;
        let (lag, d_tau1, d_tau2, d_theta) =
            second_order_lag_step_jacobian(theta, self.time_constant_1, self.time_constant_2);
        let scale = self.gain * step_amplitude;
        SopdtStepResponseJacobian {
            gain: step_amplitude * lag,
            time_constant_1: scale * d_tau1,
            time_constant_2: scale * d_tau2,
            delay: -scale * d_theta,
        }
    }

    /// Samples the delayed step response on the supplied time grid.
    pub fn step_response_values(
        &self,
        sample_times: &[R],
        step_time: R,
        step_amplitude: R,
        initial_output: R,
    ) -> Result<Vec<R>, LtiError> {
        validate_step_grid(sample_times)?;
        Ok(sample_times
            .iter()
            .map(|&time| {
                self.step_response_value(
                    (time - step_time).max(R::zero()),
                    step_amplitude,
                    initial_output,
                )
            })
            .collect())
    }

    /// Samples Nyquist data on a continuous-time angular-frequency grid.
    pub fn nyquist_data(&self, angular_frequencies: &[R]) -> Result<NyquistData<R>, LtiError> {
        validate_frequency_grid(angular_frequencies, "nyquist_data")?;
        Ok(NyquistData {
            angular_frequencies: angular_frequencies.to_vec(),
            values: angular_frequencies
                .iter()
                .map(|&omega| self.evaluate(Complex::new(R::zero(), omega)))
                .collect(),
        })
    }

    /// Samples Nichols data on a continuous-time angular-frequency grid.
    pub fn nichols_data(&self, angular_frequencies: &[R]) -> Result<NicholsData<R>, LtiError> {
        let bode = self.bode_data(angular_frequencies)?;
        Ok(NicholsData {
            angular_frequencies: bode.angular_frequencies,
            magnitude_db: bode.magnitude_db,
            phase_deg: bode.phase_deg,
        })
    }

    /// Samples Bode magnitude and unwrapped phase on a continuous-time
    /// angular-frequency grid.
    pub fn bode_data(&self, angular_frequencies: &[R]) -> Result<BodeData<R>, LtiError> {
        validate_frequency_grid(angular_frequencies, "bode_data")?;
        let values = angular_frequencies
            .iter()
            .map(|&omega| self.evaluate(Complex::new(R::zero(), omega)))
            .collect::<Vec<_>>();
        let magnitude_db = values
            .iter()
            .map(|value| R::from(20.0).unwrap() * value.abs().log10())
            .collect::<Vec<_>>();
        let phase_deg = unwrap_phase_deg(
            &values
                .iter()
                .map(|value| {
                    value.im.atan2(value.re) * R::from(180.0 / core::f64::consts::PI).unwrap()
                })
                .collect::<Vec<_>>(),
        );
        Ok(BodeData {
            angular_frequencies: angular_frequencies.to_vec(),
            magnitude_db,
            phase_deg,
        })
    }
}

fn validate_step_grid<R>(sample_times: &[R]) -> Result<(), LtiError>
where
    R: Float + RealField,
{
    if sample_times.is_empty()
        || sample_times
            .windows(2)
            .any(|window| !window[0].is_finite() || !window[1].is_finite() || window[1] < window[0])
        || sample_times.iter().any(|&time| !time.is_finite())
    {
        return Err(LtiError::InvalidSampleGrid {
            which: "step_response_values",
        });
    }
    Ok(())
}

fn validate_frequency_grid<R>(
    angular_frequencies: &[R],
    which: &'static str,
) -> Result<(), LtiError>
where
    R: Float + RealField,
{
    if angular_frequencies.is_empty()
        || angular_frequencies
            .iter()
            .any(|&omega| !omega.is_finite() || omega < R::zero())
        || angular_frequencies
            .windows(2)
            .any(|window| window[1] < window[0])
    {
        return Err(LtiError::InvalidSampleGrid { which });
    }
    Ok(())
}

fn second_order_lag_step<R>(time: R, tau1: R, tau2: R) -> R
where
    R: Float + RealField,
{
    second_order_lag_step_jacobian(time, tau1, tau2).0
}

fn second_order_lag_step_jacobian<R>(time: R, tau1: R, tau2: R) -> (R, R, R, R)
where
    R: Float + RealField,
{
    // Near a repeated pole, the distinct-lag formula loses accuracy through
    // subtraction. Switch to the repeated-pole limit in that regime.
    let tol = (tau1.abs() + tau2.abs() + R::one()) * R::from(1.0e-8).unwrap();
    if (tau1 - tau2).abs() <= tol {
        let tau = (tau1 + tau2) / R::from(2.0).unwrap();
        let scaled = time / tau;
        let exponential = (-scaled).exp();
        let lag = neg_mul_add(R::one() + scaled, exponential, R::one());
        let repeated_tau_derivative = -(scaled * scaled * exponential) / tau;
        let d_theta = (scaled * exponential) / tau;
        (
            lag,
            repeated_tau_derivative / R::from(2.0).unwrap(),
            repeated_tau_derivative / R::from(2.0).unwrap(),
            d_theta,
        )
    } else {
        let e1 = (-(time / tau1)).exp();
        let e2 = (-(time / tau2)).exp();
        let numerator = tau1 * e1 - tau2 * e2;
        let denominator = tau1 - tau2;
        let lag = R::one() - numerator / denominator;
        let d_numerator_tau1 = e1 * (R::one() + time / tau1);
        let d_numerator_tau2 = -e2 * (R::one() + time / tau2);
        let denom_sq = denominator * denominator;
        let d_tau1 = -((d_numerator_tau1 * denominator) - numerator) / denom_sq;
        let d_tau2 = -((d_numerator_tau2 * denominator) + numerator) / denom_sq;
        let d_theta = (e1 - e2) / denominator;
        (lag, d_tau1, d_tau2, d_theta)
    }
}

fn complex_exp<R>(value: Complex<R>) -> Complex<R>
where
    R: Float + RealField,
{
    let scale = value.re.exp();
    Complex::new(scale * value.im.cos(), scale * value.im.sin())
}

#[cfg(test)]
mod tests {
    use super::{FopdtModel, FopdtStepResponseJacobian, SopdtModel, SopdtStepResponseJacobian};

    fn assert_close(lhs: f64, rhs: f64, tol: f64) {
        let err = (lhs - rhs).abs();
        assert!(err <= tol, "lhs={lhs}, rhs={rhs}, err={err}, tol={tol}");
    }

    fn assert_fopdt_jacobian_close(
        actual: FopdtStepResponseJacobian<f64>,
        expected: FopdtStepResponseJacobian<f64>,
        tol: f64,
    ) {
        assert_close(actual.gain, expected.gain, tol);
        assert_close(actual.time_constant, expected.time_constant, tol);
        assert_close(actual.delay, expected.delay, tol);
    }

    fn assert_sopdt_jacobian_close(
        actual: SopdtStepResponseJacobian<f64>,
        expected: SopdtStepResponseJacobian<f64>,
        tol: f64,
    ) {
        assert_close(actual.gain, expected.gain, tol);
        assert_close(actual.time_constant_1, expected.time_constant_1, tol);
        assert_close(actual.time_constant_2, expected.time_constant_2, tol);
        assert_close(actual.delay, expected.delay, tol);
    }

    fn central_difference(eval: impl Fn(f64) -> f64, center: f64, step: f64) -> f64 {
        (eval(center + step) - eval(center - step)) / (2.0 * step)
    }

    #[test]
    fn fopdt_has_expected_dc_gain_and_delay_behavior() {
        let model = FopdtModel {
            gain: 2.0,
            time_constant: 3.0,
            delay: 1.5,
        };
        assert_close(model.dc_gain(), 2.0, 1.0e-12);
        assert_close(model.step_response_value(1.0, 1.0, 0.25), 0.25, 1.0e-12);
        let late = model.step_response_value(10.0, 1.0, 0.25);
        assert!(late > 2.1);
        assert!(late < 2.25);
    }

    #[test]
    fn sopdt_has_expected_dc_gain_and_monotone_step_value() {
        let model = SopdtModel {
            gain: 1.5,
            time_constant_1: 4.0,
            time_constant_2: 1.0,
            delay: 0.5,
        };
        assert_close(model.dc_gain(), 1.5, 1.0e-12);
        assert_close(model.step_response_value(0.25, 1.0, 0.0), 0.0, 1.0e-12);
        let mid = model.step_response_value(3.0, 1.0, 0.0);
        let late = model.step_response_value(20.0, 1.0, 0.0);
        assert!(late > mid);
        assert!(late < 1.5 + 1.0e-4);
    }

    #[test]
    fn fopdt_step_response_jacobian_matches_central_difference() {
        let model = FopdtModel {
            gain: 1.8,
            time_constant: 2.4,
            delay: 0.75,
        };
        let time_since_step = 3.2;
        let step_amplitude = 1.3;
        let jacobian = model.step_response_jacobian_value(time_since_step, step_amplitude);
        let step = 1.0e-6;
        let expected = FopdtStepResponseJacobian {
            gain: central_difference(
                |gain| {
                    FopdtModel { gain, ..model }.step_response_value(
                        time_since_step,
                        step_amplitude,
                        0.2,
                    )
                },
                model.gain,
                step,
            ),
            time_constant: central_difference(
                |time_constant| {
                    FopdtModel {
                        time_constant,
                        ..model
                    }
                    .step_response_value(time_since_step, step_amplitude, 0.2)
                },
                model.time_constant,
                step,
            ),
            delay: central_difference(
                |delay| {
                    FopdtModel { delay, ..model }.step_response_value(
                        time_since_step,
                        step_amplitude,
                        0.2,
                    )
                },
                model.delay,
                step,
            ),
        };
        assert_fopdt_jacobian_close(jacobian, expected, 1.0e-6);
    }

    #[test]
    fn sopdt_step_response_jacobian_matches_central_difference() {
        let model = SopdtModel {
            gain: 1.25,
            time_constant_1: 3.0,
            time_constant_2: 1.2,
            delay: 0.45,
        };
        let time_since_step = 4.0;
        let step_amplitude = 0.9;
        let jacobian = model.step_response_jacobian_value(time_since_step, step_amplitude);
        let step = 1.0e-6;
        let expected = SopdtStepResponseJacobian {
            gain: central_difference(
                |gain| {
                    SopdtModel { gain, ..model }.step_response_value(
                        time_since_step,
                        step_amplitude,
                        -0.1,
                    )
                },
                model.gain,
                step,
            ),
            time_constant_1: central_difference(
                |time_constant_1| {
                    SopdtModel {
                        time_constant_1,
                        ..model
                    }
                    .step_response_value(time_since_step, step_amplitude, -0.1)
                },
                model.time_constant_1,
                step,
            ),
            time_constant_2: central_difference(
                |time_constant_2| {
                    SopdtModel {
                        time_constant_2,
                        ..model
                    }
                    .step_response_value(time_since_step, step_amplitude, -0.1)
                },
                model.time_constant_2,
                step,
            ),
            delay: central_difference(
                |delay| {
                    SopdtModel { delay, ..model }.step_response_value(
                        time_since_step,
                        step_amplitude,
                        -0.1,
                    )
                },
                model.delay,
                step,
            ),
        };
        assert_sopdt_jacobian_close(jacobian, expected, 1.0e-6);
    }

    #[test]
    fn repeated_pole_sopdt_step_response_jacobian_matches_central_difference() {
        let model = SopdtModel {
            gain: 0.95,
            time_constant_1: 1.8,
            time_constant_2: 1.8,
            delay: 0.35,
        };
        let time_since_step = 2.75;
        let step_amplitude = 1.1;
        let jacobian = model.step_response_jacobian_value(time_since_step, step_amplitude);
        let step = 1.0e-6;
        let expected = SopdtStepResponseJacobian {
            gain: central_difference(
                |gain| {
                    SopdtModel { gain, ..model }.step_response_value(
                        time_since_step,
                        step_amplitude,
                        0.0,
                    )
                },
                model.gain,
                step,
            ),
            time_constant_1: central_difference(
                |time_constant_1| {
                    SopdtModel {
                        time_constant_1,
                        ..model
                    }
                    .step_response_value(time_since_step, step_amplitude, 0.0)
                },
                model.time_constant_1,
                step,
            ),
            time_constant_2: central_difference(
                |time_constant_2| {
                    SopdtModel {
                        time_constant_2,
                        ..model
                    }
                    .step_response_value(time_since_step, step_amplitude, 0.0)
                },
                model.time_constant_2,
                step,
            ),
            delay: central_difference(
                |delay| {
                    SopdtModel { delay, ..model }.step_response_value(
                        time_since_step,
                        step_amplitude,
                        0.0,
                    )
                },
                model.delay,
                step,
            ),
        };
        assert_sopdt_jacobian_close(jacobian, expected, 5.0e-6);
    }
}
