//! Fixed-size sampled PID controllers for embedded deployment.
//!
//! # Glossary
//!
//! - **PID:** Proportional-integral-derivative controller.
//! - **Anti-windup:** Logic that limits or corrects the integral state when
//!   the output saturates.
//! - **Derivative filter:** Internal state used to smooth the derivative
//!   contribution.

use crate::embedded::error::EmbeddedError;
use num_traits::Float;

/// Anti-windup policy used by [`Pid`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum AntiWindup<T> {
    /// No anti-windup logic.
    None,
    /// Internal output clamping.
    Clamp { low: T, high: T },
    /// Back-calculation using an externally supplied applied command.
    BackCalculation { gain: T },
}

/// Per-lane runtime state for a sampled PID controller.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PidState<T, const LANES: usize> {
    /// Stored integral contribution for each lane.
    pub integrator: [T; LANES],
    /// Stored derivative-filter state for each lane.
    pub derivative_state: [T; LANES],
}

/// Full sampled PID output for one multichannel timestep.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PidOutput<T, const LANES: usize> {
    /// Unsaturated output.
    pub unsaturated: [T; LANES],
    /// Output after the anti-windup wrapper.
    pub saturated: [T; LANES],
    /// Proportional contribution.
    pub proportional: [T; LANES],
    /// Integral contribution.
    pub integral: [T; LANES],
    /// Derivative contribution.
    pub derivative: [T; LANES],
    /// Applied minus unsaturated mismatch.
    pub windup_error: [T; LANES],
}

/// Fixed-size sampled PID/PIDF controller.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Pid<T, const LANES: usize> {
    kp: T,
    ki: T,
    kd: T,
    sample_time: T,
    derivative_filter: Option<T>,
    anti_windup: AntiWindup<T>,
    integrator_limits: Option<(T, T)>,
}

impl<T, const LANES: usize> PidState<T, LANES>
where
    T: Float,
{
    /// Returns the zero-initialized runtime state.
    #[must_use]
    pub fn zeros() -> Self {
        Self {
            integrator: [T::zero(); LANES],
            derivative_state: [T::zero(); LANES],
        }
    }

    /// Resets the stored state to zero.
    pub fn reset(&mut self) {
        *self = Self::zeros();
    }
}

impl<T, const LANES: usize> Default for PidState<T, LANES>
where
    T: Float,
{
    fn default() -> Self {
        Self::zeros()
    }
}

impl<T, const LANES: usize> Pid<T, LANES>
where
    T: Float,
{
    /// Creates a validated sampled PID/PIDF controller.
    pub fn new(
        kp: T,
        ki: T,
        kd: T,
        sample_time: T,
        derivative_filter: Option<T>,
        anti_windup: AntiWindup<T>,
    ) -> Result<Self, EmbeddedError> {
        if !sample_time.is_finite() || sample_time <= T::zero() {
            return Err(EmbeddedError::InvalidSampleTime);
        }
        if kd != T::zero() {
            match derivative_filter {
                Some(filter) if filter.is_finite() && filter > T::zero() => {}
                _ => {
                    return Err(EmbeddedError::InvalidParameter {
                        which: "pid.derivative_filter",
                    });
                }
            }
        }
        match anti_windup {
            AntiWindup::Clamp { low, high }
                if !low.is_finite() || !high.is_finite() || low > high =>
            {
                return Err(EmbeddedError::InvalidParameter {
                    which: "pid.anti_windup.clamp",
                });
            }
            AntiWindup::BackCalculation { gain } if !gain.is_finite() || gain < T::zero() => {
                return Err(EmbeddedError::InvalidParameter {
                    which: "pid.anti_windup.back_calculation",
                });
            }
            _ => {}
        }

        Ok(Self {
            kp,
            ki,
            kd,
            sample_time,
            derivative_filter,
            anti_windup,
            integrator_limits: None,
        })
    }

    /// Adds explicit limits on the stored integral contribution.
    pub fn with_integrator_limits(mut self, low: T, high: T) -> Result<Self, EmbeddedError> {
        if !low.is_finite() || !high.is_finite() || low > high {
            return Err(EmbeddedError::InvalidParameter {
                which: "pid.integrator_limits",
            });
        }
        self.integrator_limits = Some((low, high));
        Ok(self)
    }

    /// Returns the configured sample interval.
    #[must_use]
    pub fn sample_time(&self) -> T {
        self.sample_time
    }

    /// Returns the zero-initialized runtime state.
    #[must_use]
    pub fn reset_state(&self) -> PidState<T, LANES> {
        PidState::zeros()
    }

    /// Runs one controller step for anti-windup modes that do not require a
    /// tracking signal.
    pub fn step(
        &self,
        state: &mut PidState<T, LANES>,
        setpoint: [T; LANES],
        measurement: [T; LANES],
    ) -> Result<PidOutput<T, LANES>, EmbeddedError> {
        if matches!(self.anti_windup, AntiWindup::BackCalculation { .. }) {
            return Err(EmbeddedError::TrackingRequired);
        }
        self.step_impl(state, setpoint, measurement, None)
    }

    /// Runs one controller step with an externally supplied applied command.
    pub fn step_with_tracking(
        &self,
        state: &mut PidState<T, LANES>,
        setpoint: [T; LANES],
        measurement: [T; LANES],
        applied: [T; LANES],
    ) -> Result<PidOutput<T, LANES>, EmbeddedError> {
        self.step_impl(state, setpoint, measurement, Some(applied))
    }

    /// Executes the common sampled PID update path.
    fn step_impl(
        &self,
        state: &mut PidState<T, LANES>,
        setpoint: [T; LANES],
        measurement: [T; LANES],
        tracking: Option<[T; LANES]>,
    ) -> Result<PidOutput<T, LANES>, EmbeddedError> {
        let mut output = PidOutput {
            unsaturated: [T::zero(); LANES],
            saturated: [T::zero(); LANES],
            proportional: [T::zero(); LANES],
            integral: [T::zero(); LANES],
            derivative: [T::zero(); LANES],
            windup_error: [T::zero(); LANES],
        };

        for lane in 0..LANES {
            let error = setpoint[lane] - measurement[lane];
            let proportional = self.kp * error;
            let derivative = match self.effective_derivative_filter() {
                Some(filter) => self.kd * filter * (error - state.derivative_state[lane]),
                None => T::zero(),
            };
            let integral = state.integrator[lane];
            let unsaturated = proportional + integral + derivative;

            let (saturated, windup_error) = match (self.anti_windup, tracking) {
                (AntiWindup::None, _) => (unsaturated, T::zero()),
                (AntiWindup::Clamp { low, high }, _) => (unsaturated.max(low).min(high), T::zero()),
                (AntiWindup::BackCalculation { .. }, Some(applied)) => {
                    (applied[lane], applied[lane] - unsaturated)
                }
                (AntiWindup::BackCalculation { .. }, None) => {
                    return Err(EmbeddedError::TrackingRequired);
                }
            };

            let next_derivative_state =
                self.next_derivative_state(error, state.derivative_state[lane]);
            let next_integrator = self.next_integrator(
                error,
                proportional,
                derivative,
                unsaturated,
                saturated,
                state.integrator[lane],
            );

            state.derivative_state[lane] = next_derivative_state;
            state.integrator[lane] = next_integrator;

            output.unsaturated[lane] = unsaturated;
            output.saturated[lane] = saturated;
            output.proportional[lane] = proportional;
            output.integral[lane] = integral;
            output.derivative[lane] = derivative;
            output.windup_error[lane] = windup_error;
        }

        Ok(output)
    }

    /// Returns the effective derivative filter if the derivative term is active.
    fn effective_derivative_filter(&self) -> Option<T> {
        if self.kd == T::zero() {
            None
        } else {
            self.derivative_filter
        }
    }

    /// Advances the stored derivative filter state by one exact ZOH step.
    fn next_derivative_state(&self, error: T, derivative_state: T) -> T {
        match self.effective_derivative_filter() {
            Some(filter) => {
                let alpha = (-(filter * self.sample_time)).exp();
                alpha * derivative_state + (T::one() - alpha) * error
            }
            None => derivative_state,
        }
    }

    /// Advances the stored integral contribution by one timestep.
    fn next_integrator(
        &self,
        error: T,
        proportional: T,
        derivative: T,
        unsaturated: T,
        saturated: T,
        integrator: T,
    ) -> T {
        let correction = match self.anti_windup {
            AntiWindup::None => T::zero(),
            AntiWindup::Clamp { .. } => T::zero(),
            AntiWindup::BackCalculation { gain } => gain * (saturated - unsaturated),
        };
        let candidate = integrator + self.sample_time * (self.ki * error + correction);
        let candidate = self.apply_integrator_limits(candidate);

        match self.anti_windup {
            AntiWindup::Clamp { low, high } => {
                let limited = candidate
                    .max(low - proportional - derivative)
                    .min(high - proportional - derivative);
                self.apply_integrator_limits(limited)
            }
            _ => candidate,
        }
    }

    /// Applies any configured direct integrator bounds.
    fn apply_integrator_limits(&self, value: T) -> T {
        match self.integrator_limits {
            Some((low, high)) => value.max(low).min(high),
            None => value,
        }
    }
}

#[cfg(feature = "alloc")]
impl<T, const LANES: usize> Pid<T, LANES>
where
    T: Float + faer_traits::RealField,
{
    /// Builds a fixed-size embedded PID from the dynamic control-side runtime
    /// controller and an explicit sample interval.
    pub fn from_control(
        pid: &crate::control::synthesis::Pid<T>,
        sample_time: T,
    ) -> Result<Self, EmbeddedError> {
        let mut out = Self::new(
            pid.kp(),
            pid.ki(),
            pid.kd(),
            sample_time,
            pid.derivative_filter(),
            match pid.anti_windup() {
                crate::control::synthesis::AntiWindup::None => AntiWindup::None,
                crate::control::synthesis::AntiWindup::Clamp { low, high } => {
                    AntiWindup::Clamp { low, high }
                }
                crate::control::synthesis::AntiWindup::BackCalculation { gain } => {
                    AntiWindup::BackCalculation { gain }
                }
            },
        )?;
        if let Some((low, high)) = pid.integrator_limits() {
            out = out.with_integrator_limits(low, high)?;
        }
        Ok(out)
    }
}
