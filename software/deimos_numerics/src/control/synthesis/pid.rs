//! Discrete runtime PID / PIDF controllers with explicit anti-windup policy.
//!
//! The design here keeps a strict boundary between:
//!
//! - a linear unsaturated PIDF core
//! - nonlinear runtime anti-windup wrappers around that core
//!
//! That boundary matters because only the anti-windup-free controller can be
//! exported cleanly as a linear transfer function or state-space model.
//!
//! The runtime controller is discrete-time first. The linear export helpers are
//! a separate view of the same unsaturated controller, expressed as continuous
//! error dynamics and discretized through the existing state-space conversion
//! path when the caller asks for a sampled LTI model.
//!
//! # Two Intuitions
//!
//! 1. **Operator view.** PID is the most practical low-order controller: react
//!    to the current error, accumulated error, and error trend.
//! 2. **State-machine view.** The implementation here is a sampled controller
//!    with a small explicit runtime state, optional saturation logic, and an
//!    optional filtered derivative path.
//!
//! # Glossary
//!
//! - **Anti-windup:** Strategy used to keep the integrator from accumulating
//!   unusable error under actuator limits.
//! - **Back-calculation:** Uses the difference between commanded and applied
//!   control to unwind the integrator.
//! - **PIDF:** PID with a first-order filtered derivative term.
//!
//! # Mathematical Formulation
//!
//! The runtime controller computes a sampled approximation of:
//!
//! - `u = K_p e + K_i integral(e) + K_d * filtered(de/dt)`
//!
//! with nonlinear wrappers for clamp or back-calculation anti-windup.
//!
//! # Implementation Notes
//!
//! - Linear export helpers are only valid for the linear `AntiWindup::None`
//!   mode.
//! - Back-calculation is modeled around an external applied-command signal,
//!   which makes it more general than a built-in clamp-only correction.

use crate::control::lti::state_space::{
    ContinuousStateSpace, DiscreteStateSpace, DiscretizationMethod, StateSpaceError,
};
use crate::control::lti::{ContinuousTransferFunction, DiscreteTransferFunction, LtiError};
use crate::scalar::mul_add;
use crate::sparse::compensated::CompensatedField;
use core::fmt;
use faer::Mat;
use faer_traits::RealField;
use num_traits::Float;

/// Anti-windup policy for [`Pid`].
///
/// `None` keeps the controller linear. `Clamp` and `BackCalculation` are
/// nonlinear runtime wrappers because they depend on actuator limiting or an
/// externally supplied applied-command signal.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum AntiWindup<R> {
    /// No anti-windup. The controller remains a purely linear PIDF core.
    None,
    /// Internal actuator clamping with explicit limits.
    Clamp {
        /// Lower actuator limit.
        low: R,
        /// Upper actuator limit.
        high: R,
    },
    /// Back-calculation using an externally supplied applied-command signal.
    BackCalculation {
        /// Gain that feeds the applied-command tracking error into the integrator.
        gain: R,
    },
}

/// Runtime state for a sampled PID / PIDF controller.
#[derive(Clone, Debug, PartialEq)]
pub struct PidState<R> {
    /// Current integral contribution used by the controller output.
    pub integrator: R,
    /// Internal state of the first-order derivative filter.
    pub derivative_state: R,
}

impl<R> Default for PidState<R>
where
    R: Float,
{
    fn default() -> Self {
        Self {
            integrator: R::zero(),
            derivative_state: R::zero(),
        }
    }
}

impl<R> PidState<R>
where
    R: Float,
{
    /// Resets the runtime state to zero.
    pub fn reset(&mut self) {
        self.integrator = R::zero();
        self.derivative_state = R::zero();
    }
}

/// One sampled PID / PIDF runtime output.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PidOutput<R> {
    /// Raw unsaturated controller output.
    pub unsaturated: R,
    /// Output actually passed onward by the chosen anti-windup policy.
    pub saturated: R,
    /// Proportional contribution used in `unsaturated`.
    pub proportional: R,
    /// Integral contribution used in `unsaturated`.
    pub integral: R,
    /// Derivative contribution used in `unsaturated`.
    pub derivative: R,
    /// Tracking / saturation mismatch used by the anti-windup path.
    pub windup_error: R,
}

/// Errors produced by [`Pid`] construction, runtime stepping, or linear export.
#[derive(Debug)]
pub enum PidError {
    /// Sample time must be positive and finite.
    InvalidSampleTime,
    /// `kd != 0` requires a positive finite derivative filter parameter.
    InvalidDerivativeFilter,
    /// Integrator limits must satisfy `low <= high`.
    InvalidIntegratorLimits,
    /// Clamp limits must satisfy `low <= high`.
    InvalidClampLimits,
    /// Back-calculation gain must be finite and nonnegative.
    InvalidBackCalculationGain,
    /// `step()` cannot be used when back-calculation requires a tracking input.
    TrackingRequiredForBackCalculation,
    /// Linear export is only defined for `AntiWindup::None`.
    NonlinearControllerExport,
    /// Lower-level LTI conversion failed.
    Lti(LtiError),
    /// Lower-level state-space construction or discretization failed.
    StateSpace(StateSpaceError),
}

impl fmt::Display for PidError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

impl core::error::Error for PidError {}

impl From<LtiError> for PidError {
    fn from(value: LtiError) -> Self {
        Self::Lti(value)
    }
}

impl From<StateSpaceError> for PidError {
    fn from(value: StateSpaceError) -> Self {
        Self::StateSpace(value)
    }
}

/// Sampled PID / PIDF controller configuration.
///
/// The same type covers:
///
/// - `P` with `ki = kd = 0`
/// - `PI` with `kd = 0`
/// - `PIDF` with all terms active
///
/// A zero gain disables the corresponding term.
///
/// The derivative path uses a first-order filtered form. The parameter
/// `derivative_filter` is the filter pole / bandwidth term `N` in the common
/// PIDF realization `kd * (N s) / (s + N)`.
#[derive(Clone, Debug, PartialEq)]
pub struct Pid<R> {
    kp: R,
    ki: R,
    kd: R,
    derivative_filter: Option<R>,
    anti_windup: AntiWindup<R>,
    integrator_limits: Option<(R, R)>,
}

impl<R> Pid<R>
where
    R: Float + RealField,
{
    /// Creates a validated PID / PIDF controller.
    ///
    /// The constructor validates the derivative-filter configuration and the
    /// chosen anti-windup policy, but does not otherwise constrain the PID
    /// gains. Negative gains are allowed because the crate does not assume a
    /// particular feedback sign convention at this layer.
    pub fn new(
        kp: R,
        ki: R,
        kd: R,
        derivative_filter: Option<R>,
        anti_windup: AntiWindup<R>,
    ) -> Result<Self, PidError> {
        validate_derivative_filter(kd, derivative_filter)?;
        validate_anti_windup(anti_windup)?;
        Ok(Self {
            kp,
            ki,
            kd,
            derivative_filter,
            anti_windup,
            integrator_limits: None,
        })
    }

    /// Adds explicit limits to the stored integral contribution.
    ///
    /// These limits are independent of `AntiWindup::Clamp`. Clamp limits bound
    /// the commanded output, while integrator limits bound the internal
    /// integrator state directly.
    pub fn with_integrator_limits(mut self, low: R, high: R) -> Result<Self, PidError> {
        if !low.is_finite() || !high.is_finite() || low > high {
            return Err(PidError::InvalidIntegratorLimits);
        }
        self.integrator_limits = Some((low, high));
        Ok(self)
    }

    /// Proportional gain.
    #[must_use]
    pub fn kp(&self) -> R {
        self.kp
    }

    /// Integral gain.
    #[must_use]
    pub fn ki(&self) -> R {
        self.ki
    }

    /// Derivative gain.
    #[must_use]
    pub fn kd(&self) -> R {
        self.kd
    }

    /// Optional derivative filter parameter.
    #[must_use]
    pub fn derivative_filter(&self) -> Option<R> {
        self.derivative_filter
    }

    /// Anti-windup policy.
    #[must_use]
    pub fn anti_windup(&self) -> AntiWindup<R> {
        self.anti_windup
    }

    /// Optional integrator limits.
    #[must_use]
    pub fn integrator_limits(&self) -> Option<(R, R)> {
        self.integrator_limits
    }

    /// Runs one sampled controller step.
    ///
    /// This supports:
    ///
    /// - [`AntiWindup::None`]
    /// - [`AntiWindup::Clamp`]
    ///
    /// Use [`step_with_tracking`](Self::step_with_tracking) when the controller
    /// is configured with [`AntiWindup::BackCalculation`].
    ///
    /// The controller interprets the input signal as the control error
    /// `setpoint - measurement`.
    pub fn step(
        &self,
        state: &mut PidState<R>,
        dt: R,
        setpoint: R,
        measurement: R,
    ) -> Result<PidOutput<R>, PidError> {
        if matches!(self.anti_windup, AntiWindup::BackCalculation { .. }) {
            return Err(PidError::TrackingRequiredForBackCalculation);
        }
        self.step_impl(state, dt, setpoint, measurement, None)
    }

    /// Runs one sampled controller step with an externally supplied applied
    /// command.
    ///
    /// This is the intended runtime path for
    /// [`AntiWindup::BackCalculation`]. For the other anti-windup modes the
    /// tracking signal is ignored and the method reduces to [`step`](Self::step).
    ///
    /// `u_applied` is the command actually sent onward after any downstream
    /// saturation, selector, rate limiter, or manual/auto logic. The
    /// back-calculation correction uses `u_applied - u_unsat`.
    pub fn step_with_tracking(
        &self,
        state: &mut PidState<R>,
        dt: R,
        setpoint: R,
        measurement: R,
        u_applied: R,
    ) -> Result<PidOutput<R>, PidError> {
        self.step_impl(state, dt, setpoint, measurement, Some(u_applied))
    }

    fn step_impl(
        &self,
        state: &mut PidState<R>,
        dt: R,
        setpoint: R,
        measurement: R,
        tracking: Option<R>,
    ) -> Result<PidOutput<R>, PidError> {
        validate_dt(dt)?;
        let error = setpoint - measurement;
        let (proportional, derivative) = self.output_terms(error, state.derivative_state);
        let integral = state.integrator;
        let unsaturated = proportional + integral + derivative;

        let (saturated, windup_error) = match (self.anti_windup, tracking) {
            (AntiWindup::None, _) => (unsaturated, R::zero()),
            (AntiWindup::Clamp { low, high }, _) => (unsaturated.max(low).min(high), R::zero()),
            (AntiWindup::BackCalculation { .. }, Some(u_applied)) => {
                (u_applied, u_applied - unsaturated)
            }
            (AntiWindup::BackCalculation { .. }, None) => {
                return Err(PidError::TrackingRequiredForBackCalculation);
            }
        };

        // The reported output is computed from the current stored controller
        // state. The derivative filter and integrator are then advanced for the
        // next sample.
        let next_derivative_state = self.next_derivative_state(error, dt, state.derivative_state);
        let next_integrator = self.next_integrator(
            dt,
            error,
            proportional,
            derivative,
            unsaturated,
            saturated,
            state.integrator,
        );

        state.derivative_state = next_derivative_state;
        state.integrator = next_integrator;

        Ok(PidOutput {
            unsaturated,
            saturated,
            proportional,
            integral,
            derivative,
            windup_error,
        })
    }

    /// Returns the proportional and derivative contributions for the current
    /// error and derivative-filter state.
    ///
    /// The derivative path is realized as `kd * filter * (e - x_d)` where
    /// `x_d` is the low-pass filter state tracking the error.
    fn output_terms(&self, error: R, derivative_state: R) -> (R, R) {
        let proportional = self.kp * error;
        let derivative = match effective_derivative_filter(self.kd, self.derivative_filter) {
            Some(filter) => self.kd * filter * (error - derivative_state),
            None => R::zero(),
        };
        (proportional, derivative)
    }

    /// Advances the internal derivative-filter state by one exact ZOH step.
    ///
    /// The maintained state satisfies `x_dot = filter * (e - x)`. Using the
    /// exact scalar update avoids introducing a second ad hoc finite-difference
    /// discretization into the runtime controller.
    fn next_derivative_state(&self, error: R, dt: R, derivative_state: R) -> R {
        match effective_derivative_filter(self.kd, self.derivative_filter) {
            Some(filter) => {
                // Exact ZOH update of the first-order low-pass state
                // `x_dot = filter * (e - x)`.
                let alpha = (-(filter * dt)).exp();
                mul_add(alpha, derivative_state, (R::one() - alpha) * error)
            }
            None => derivative_state,
        }
    }

    /// Advances the stored integral contribution for the next sample.
    ///
    /// Clamp anti-windup is handled by projecting the candidate integrator
    /// state into the range consistent with the actuator limits and the current
    /// proportional / derivative contributions. Back-calculation instead adds a
    /// tracking correction driven by the applied-command mismatch.
    fn next_integrator(
        &self,
        dt: R,
        error: R,
        proportional: R,
        derivative: R,
        unsaturated: R,
        saturated: R,
        integrator: R,
    ) -> R {
        let correction = match self.anti_windup {
            AntiWindup::None => R::zero(),
            AntiWindup::Clamp { .. } => R::zero(),
            AntiWindup::BackCalculation { gain } => gain * (saturated - unsaturated),
        };
        let candidate = mul_add(dt, self.ki * error + correction, integrator);
        let candidate = apply_integrator_limits(candidate, self.integrator_limits);

        match self.anti_windup {
            AntiWindup::Clamp { low, high } => {
                let limited = candidate
                    .max(low - proportional - derivative)
                    .min(high - proportional - derivative);
                apply_integrator_limits(limited, self.integrator_limits)
            }
            _ => candidate,
        }
    }

    /// Exports the unsaturated anti-windup-free controller as a continuous-time
    /// SISO state-space realization driven by the control error.
    ///
    /// The exported model represents the linear controller core only. It does
    /// not attempt to encode saturation or anti-windup behavior.
    pub fn to_state_space_continuous(&self) -> Result<ContinuousStateSpace<R>, PidError> {
        self.ensure_linear_export()?;
        let (a, b, c, d) = self.continuous_core_matrices()?;
        Ok(ContinuousStateSpace::new(a, b, c, d)?)
    }

    /// Exports the unsaturated anti-windup-free controller as a continuous-time
    /// SISO transfer function driven by the control error.
    pub fn to_transfer_function_continuous(
        &self,
    ) -> Result<ContinuousTransferFunction<R>, PidError> {
        Ok(self.to_state_space_continuous()?.to_transfer_function()?)
    }

    /// Exports the unsaturated anti-windup-free controller as a discrete-time
    /// SISO state-space realization using exact ZOH discretization of the
    /// continuous linear core.
    pub fn to_state_space_discrete(&self, sample_time: R) -> Result<DiscreteStateSpace<R>, PidError>
    where
        R: CompensatedField,
    {
        self.ensure_linear_export()?;
        validate_dt(sample_time)?;
        Ok(self
            .to_state_space_continuous()?
            .discretize(sample_time, DiscretizationMethod::ZeroOrderHold)?)
    }

    /// Exports the unsaturated anti-windup-free controller as a discrete-time
    /// SISO transfer function using exact ZOH discretization of the continuous
    /// linear core.
    pub fn to_transfer_function_discrete(
        &self,
        sample_time: R,
    ) -> Result<DiscreteTransferFunction<R>, PidError>
    where
        R: CompensatedField,
    {
        Ok(self
            .to_state_space_discrete(sample_time)?
            .to_transfer_function()?)
    }

    fn ensure_linear_export(&self) -> Result<(), PidError> {
        if matches!(self.anti_windup, AntiWindup::None) {
            Ok(())
        } else {
            Err(PidError::NonlinearControllerExport)
        }
    }

    /// Builds the continuous-time linear PIDF core realization driven by the
    /// control error.
    ///
    /// The state ordering is:
    ///
    /// - integrator state, if `ki != 0`
    /// - derivative-filter state, if `kd != 0`
    ///
    /// This helper intentionally omits any nonlinear anti-windup behavior.
    fn continuous_core_matrices(&self) -> Result<(Mat<R>, Mat<R>, Mat<R>, Mat<R>), PidError> {
        let has_integrator = self.ki != R::zero();
        let derivative_filter = effective_derivative_filter(self.kd, self.derivative_filter);
        let has_derivative = derivative_filter.is_some();

        let nstates = (has_integrator as usize) + (has_derivative as usize);
        let mut a = Mat::<R>::zeros(nstates, nstates);
        let mut b = Mat::<R>::zeros(nstates, 1);
        let mut c = Mat::<R>::zeros(1, nstates);
        let mut d = Mat::<R>::from_fn(1, 1, |_, _| self.kp);

        let mut next_state = 0usize;
        if has_integrator {
            b[(next_state, 0)] = self.ki;
            c[(0, next_state)] = R::one();
            next_state += 1;
        }

        if let Some(filter) = derivative_filter {
            a[(next_state, next_state)] = -filter;
            b[(next_state, 0)] = filter;
            c[(0, next_state)] = -(self.kd * filter);
            d[(0, 0)] += self.kd * filter;
        }

        Ok((a, b, c, d))
    }
}

/// Validates a runtime sample period.
fn validate_dt<R>(dt: R) -> Result<(), PidError>
where
    R: Float,
{
    if dt.is_finite() && dt > R::zero() {
        Ok(())
    } else {
        Err(PidError::InvalidSampleTime)
    }
}

/// Validates the derivative-filter configuration implied by `kd`.
fn validate_derivative_filter<R>(kd: R, derivative_filter: Option<R>) -> Result<(), PidError>
where
    R: Float,
{
    if kd == R::zero() {
        return Ok(());
    }
    match derivative_filter {
        Some(value) if value.is_finite() && value > R::zero() => Ok(()),
        _ => Err(PidError::InvalidDerivativeFilter),
    }
}

/// Validates anti-windup configuration parameters.
fn validate_anti_windup<R>(anti_windup: AntiWindup<R>) -> Result<(), PidError>
where
    R: Float,
{
    match anti_windup {
        AntiWindup::None => Ok(()),
        AntiWindup::Clamp { low, high } => {
            if low.is_finite() && high.is_finite() && low <= high {
                Ok(())
            } else {
                Err(PidError::InvalidClampLimits)
            }
        }
        AntiWindup::BackCalculation { gain } => {
            if gain.is_finite() && gain >= R::zero() {
                Ok(())
            } else {
                Err(PidError::InvalidBackCalculationGain)
            }
        }
    }
}

/// Returns the effective derivative filter when the derivative path is active.
///
/// A zero derivative gain disables the derivative branch entirely, regardless
/// of whether a filter value was provided.
fn effective_derivative_filter<R>(kd: R, derivative_filter: Option<R>) -> Option<R>
where
    R: Float,
{
    if kd == R::zero() {
        None
    } else {
        derivative_filter
    }
}

/// Applies optional hard limits to the stored integral contribution.
fn apply_integrator_limits<R>(value: R, limits: Option<(R, R)>) -> R
where
    R: Float,
{
    match limits {
        Some((low, high)) => value.max(low).min(high),
        None => value,
    }
}

#[cfg(test)]
mod test {
    use super::{AntiWindup, Pid, PidError, PidState};
    use faer::complex::Complex;

    fn assert_close(lhs: f64, rhs: f64, tol: f64) {
        let err = (lhs - rhs).abs();
        assert!(err <= tol, "lhs={lhs}, rhs={rhs}, err={err}, tol={tol}");
    }

    #[test]
    fn p_controller_uses_only_proportional_term() {
        let pid = Pid::new(2.0, 0.0, 0.0, None, AntiWindup::None).unwrap();
        let mut state = PidState::default();
        let out = pid.step(&mut state, 0.1, 3.0, 1.0).unwrap();

        assert_close(out.proportional, 4.0, 1.0e-12);
        assert_close(out.integral, 0.0, 1.0e-12);
        assert_close(out.derivative, 0.0, 1.0e-12);
        assert_close(out.unsaturated, 4.0, 1.0e-12);
        assert_close(out.saturated, 4.0, 1.0e-12);
        assert_close(state.integrator, 0.0, 1.0e-12);
    }

    #[test]
    fn pi_controller_accumulates_integral_state() {
        let pid = Pid::new(1.0, 2.0, 0.0, None, AntiWindup::None).unwrap();
        let mut state = PidState::default();

        let out1 = pid.step(&mut state, 0.5, 1.0, 0.0).unwrap();
        let out2 = pid.step(&mut state, 0.5, 1.0, 0.0).unwrap();

        assert_close(out1.unsaturated, 1.0, 1.0e-12);
        assert_close(state.integrator, 2.0, 1.0e-12);
        assert_close(out2.integral, 1.0, 1.0e-12);
        assert_close(out2.unsaturated, 2.0, 1.0e-12);
    }

    #[test]
    fn pidf_derivative_filter_generates_finite_derivative_term() {
        let pid = Pid::new(1.0, 0.0, 0.5, Some(10.0), AntiWindup::None).unwrap();
        let mut state = PidState::default();

        let out = pid.step(&mut state, 0.1, 1.0, 0.0).unwrap();
        assert_close(out.proportional, 1.0, 1.0e-12);
        assert_close(out.derivative, 5.0, 1.0e-12);
        assert!(state.derivative_state > 0.0);
    }

    #[test]
    fn clamp_mode_limits_output_and_integrator_growth() {
        let pid = Pid::new(
            0.0,
            10.0,
            0.0,
            None,
            AntiWindup::Clamp {
                low: -1.0,
                high: 1.0,
            },
        )
        .unwrap();
        let mut state = PidState::default();

        let out1 = pid.step(&mut state, 1.0, 1.0, 0.0).unwrap();
        let out2 = pid.step(&mut state, 1.0, 1.0, 0.0).unwrap();

        assert_close(out1.saturated, 0.0, 1.0e-12);
        assert_close(state.integrator, 1.0, 1.0e-12);
        assert_close(out2.unsaturated, 1.0, 1.0e-12);
        assert_close(out2.saturated, 1.0, 1.0e-12);

        let _ = pid.step(&mut state, 1.0, 1.0, 0.0).unwrap();
        assert_close(state.integrator, 1.0, 1.0e-12);
    }

    #[test]
    fn back_calculation_uses_tracking_signal() {
        let pid = Pid::new(
            0.0,
            1.0,
            0.0,
            None,
            AntiWindup::BackCalculation { gain: 2.0 },
        )
        .unwrap();
        let mut state = PidState {
            integrator: 1.0,
            derivative_state: 0.0,
        };

        let out = pid
            .step_with_tracking(&mut state, 1.0, 1.0, 0.0, 0.0)
            .unwrap();

        assert_close(out.unsaturated, 1.0, 1.0e-12);
        assert_close(out.saturated, 0.0, 1.0e-12);
        assert_close(out.windup_error, -1.0, 1.0e-12);
        assert_close(state.integrator, 0.0, 1.0e-12);
    }

    #[test]
    fn step_requires_tracking_for_back_calculation() {
        let pid = Pid::new(
            0.0,
            1.0,
            0.0,
            None,
            AntiWindup::BackCalculation { gain: 1.0 },
        )
        .unwrap();
        let mut state = PidState::default();
        let err = pid.step(&mut state, 0.1, 1.0, 0.0).unwrap_err();
        assert!(matches!(err, PidError::TrackingRequiredForBackCalculation));
    }

    #[test]
    fn linear_export_matches_pi_transfer_function() {
        let pid = Pid::new(2.0, 3.0, 0.0, None, AntiWindup::None).unwrap();
        let tf = pid.to_transfer_function_continuous().unwrap();
        let s = Complex::new(0.5, 0.25);
        let expected = 2.0 + 3.0 / s;
        let actual = tf.evaluate(s);
        assert_close(actual.re, expected.re, 1.0e-10);
        assert_close(actual.im, expected.im, 1.0e-10);
    }

    #[test]
    fn linear_export_matches_pidf_transfer_function() {
        let pid = Pid::new(1.0, 2.0, 3.0, Some(4.0), AntiWindup::None).unwrap();
        let tf = pid.to_transfer_function_continuous().unwrap();
        let s = Complex::new(0.7, -0.2);
        let expected = 1.0 + 2.0 / s + 3.0 * (4.0 * s) / (s + 4.0);
        let actual = tf.evaluate(s);
        assert_close(actual.re, expected.re, 1.0e-10);
        assert_close(actual.im, expected.im, 1.0e-10);
    }

    #[test]
    fn discrete_linear_export_preserves_sample_time() {
        let pid = Pid::new(1.0, 2.0, 0.5, Some(3.0), AntiWindup::None).unwrap();
        let tf = pid.to_transfer_function_discrete(0.1).unwrap();
        assert_close(tf.sample_time(), 0.1, 1.0e-12);
    }

    #[test]
    fn nonlinear_modes_reject_linear_export() {
        let pid = Pid::new(
            1.0,
            0.0,
            0.0,
            None,
            AntiWindup::Clamp {
                low: -1.0,
                high: 1.0,
            },
        )
        .unwrap();
        let err = pid.to_transfer_function_continuous().unwrap_err();
        assert!(matches!(err, PidError::NonlinearControllerExport));
    }

    #[test]
    fn constructor_rejects_invalid_configuration() {
        assert!(matches!(
            Pid::new(0.0, 0.0, 1.0, None, AntiWindup::None).unwrap_err(),
            PidError::InvalidDerivativeFilter
        ));
        assert!(matches!(
            Pid::new(
                0.0,
                0.0,
                0.0,
                None,
                AntiWindup::Clamp {
                    low: 1.0,
                    high: -1.0
                }
            )
            .unwrap_err(),
            PidError::InvalidClampLimits
        ));
        assert!(matches!(
            Pid::new(
                0.0,
                0.0,
                0.0,
                None,
                AntiWindup::BackCalculation { gain: -1.0 }
            )
            .unwrap_err(),
            PidError::InvalidBackCalculationGain
        ));
    }
}
