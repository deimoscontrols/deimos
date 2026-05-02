//! PID tuning helpers based on low-order process models.
//!
//! The implementation focuses on the pragmatic workflow that is easiest
//! to validate and explain:
//!
//! - fit `FOPDT` / `SOPDT` process models from sampled step-response data
//! - tune `PI` / `PIDF` controllers from those fitted models with SIMC-style
//!   formulas
//!
//! The module also includes:
//!
//! - direct frequency-domain tuning from SISO transfer-function / state-space
//!   plant models
//! - discrete-time step-response optimization from arbitrary identified or
//!   modeled plants
//! - a pragmatic `OKID -> ERA -> PID` bridge for tuning from sampled I/O data
//!
//! # Two Intuitions
//!
//! 1. **Process-model view.** PID tuning is easiest when the plant is reduced
//!    to a small interpretable surrogate like FOPDT or SOPDT.
//! 2. **Closed-loop-objective view.** The richer design paths in this module
//!    tune gains by directly evaluating the modeled closed-loop behavior rather
//!    than by relying only on hand-derived formulas.
//!
//! # Glossary
//!
//! - **SIMC:** Simple internal model control tuning rules.
//! - **`lambda`:** Desired closed-loop time scale in the SIMC tuning rules.
//! - **FOPDT / SOPDT:** Low-order process models with explicit delay.
//! - **OKID/ERA path:** Identification-driven path from sampled I/O data to a
//!   discrete model and then to a tuned PID controller.
//!
//! # Mathematical Formulation
//!
//! The module combines:
//!
//! - low-order process-model fitting from step data
//! - SIMC-style analytical tuning rules
//! - sampled frequency-domain and step-response objective functions
//! - identification-driven tuning on an ERA-realized discrete model
//!
//! # Implementation Notes
//!
//! - FOPDT and SOPDT fitting use structured seeds followed by
//!   Levenberg-Marquardt refinement.
//! - The optimization-backed paths remain SISO and `f64`-oriented in this
//!   implementation.
//! - Public FOPDT / SOPDT model types are shared with the LTI layer to avoid
//!   parallel process-model type hierarchies.

use super::pid::{AntiWindup, Pid, PidError};
use crate::control::identification::{
    EraError, EraParams, OkidError, OkidParams, era_from_markov, okid,
};
use crate::control::lti::state_space::{ContinuousStateSpace, DiscreteStateSpace, StateSpaceError};
use crate::control::lti::{ContinuousTransferFunction, DiscreteTransferFunction, LtiError};
use crate::control::realization::max_square_era_block_dim;
use alloc::vec::Vec;
use core::fmt;
use faer::Mat;
use faer::complex::Complex;
use faer_traits::RealField;
use faer_traits::ext::ComplexFieldExt;
use levenberg_marquardt::{LeastSquaresProblem, LevenbergMarquardt, differentiate_numerically};
use nalgebra::storage::Owned;
use nalgebra::{Dyn, OMatrix, OVector, U1, U2, U3, U4, VecStorage, Vector2, Vector3, Vector4};
use num_traits::Float;

/// Errors produced by PID process-model fitting and SIMC-style tuning.
#[derive(Debug)]
pub enum PidDesignError {
    /// The sampled data is structurally invalid.
    InvalidData {
        /// Identifies the structural property that failed validation.
        which: &'static str,
    },
    /// The detected input step is too small to identify a process gain.
    InvalidStepAmplitude,
    /// The observed output change is too small to identify a process gain.
    InvalidResponseAmplitude,
    /// A delay estimate could not be formed from the sampled response.
    InvalidDelayEstimate,
    /// The fitted process parameters are not physically meaningful.
    InvalidProcessFit,
    /// The requested tuning parameter is invalid.
    InvalidTuningParameter {
        /// Identifies the invalid tuning parameter.
        which: &'static str,
    },
    /// The requested frequency-domain design target is invalid.
    InvalidFrequencyTarget {
        /// Identifies the invalid crossover or margin target.
        which: &'static str,
    },
    /// A model-based or identification-based tuning solve did not converge to
    /// a usable controller.
    OptimizationFailed {
        /// Identifies the optimization stage that failed.
        which: &'static str,
    },
    /// LTI analysis or conversion failed.
    Lti(LtiError),
    /// State-space conversion or validation failed.
    StateSpace(StateSpaceError),
    /// OKID identification failed.
    Okid(OkidError),
    /// ERA realization failed.
    Era(EraError),
    /// PID construction failed after the tuning rule produced gains.
    Pid(PidError),
}

impl fmt::Display for PidDesignError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

impl core::error::Error for PidDesignError {}

impl From<PidError> for PidDesignError {
    fn from(value: PidError) -> Self {
        Self::Pid(value)
    }
}

impl From<LtiError> for PidDesignError {
    fn from(value: LtiError) -> Self {
        Self::Lti(value)
    }
}

impl From<StateSpaceError> for PidDesignError {
    fn from(value: StateSpaceError) -> Self {
        Self::StateSpace(value)
    }
}

impl From<OkidError> for PidDesignError {
    fn from(value: OkidError) -> Self {
        Self::Okid(value)
    }
}

impl From<EraError> for PidDesignError {
    fn from(value: EraError) -> Self {
        Self::Era(value)
    }
}

/// Sampled SISO step-response experiment data.
///
/// The implementation assumes one dominant input step over the record.
#[derive(Clone, Debug, PartialEq)]
pub struct StepResponseData<R> {
    time: Vec<R>,
    input: Vec<R>,
    output: Vec<R>,
}

impl<R> StepResponseData<R>
where
    R: Float,
{
    /// Creates validated step-response data.
    pub fn new(time: Vec<R>, input: Vec<R>, output: Vec<R>) -> Result<Self, PidDesignError> {
        if time.len() < 4 {
            return Err(PidDesignError::InvalidData { which: "time.len" });
        }
        if time.len() != input.len() || time.len() != output.len() {
            return Err(PidDesignError::InvalidData {
                which: "length_mismatch",
            });
        }
        if time
            .iter()
            .chain(input.iter())
            .chain(output.iter())
            .any(|value| !value.is_finite())
        {
            return Err(PidDesignError::InvalidData {
                which: "nonfinite_sample",
            });
        }
        if time
            .windows(2)
            .any(|window| window[1].partial_cmp(&window[0]) != Some(core::cmp::Ordering::Greater))
        {
            return Err(PidDesignError::InvalidData {
                which: "nonmonotone_time",
            });
        }
        Ok(Self {
            time,
            input,
            output,
        })
    }

    /// Sample times.
    #[must_use]
    pub fn time(&self) -> &[R] {
        &self.time
    }

    /// Input samples.
    #[must_use]
    pub fn input(&self) -> &[R] {
        &self.input
    }

    /// Output samples.
    #[must_use]
    pub fn output(&self) -> &[R] {
        &self.output
    }
}

pub use crate::control::lti::{FopdtModel, SopdtModel};

/// Result of fitting a low-order process model to sampled data.
#[derive(Clone, Debug, PartialEq)]
pub struct ProcessFitResult<M, R> {
    /// Structured initial estimate derived directly from response landmarks.
    pub initial: M,
    /// Refined model after local nonlinear least-squares improvement.
    pub model: M,
    /// Sum of squared residuals of the final fit.
    pub objective: R,
}

/// Solver options for the nonlinear least-squares process-model fitters.
///
/// The fitting path uses Levenberg-Marquardt under the hood. This options
/// struct exposes only the two knobs that matter most in practice for the
/// crate's small FOPDT/SOPDT problems:
///
/// - relative termination tolerance
/// - evaluation patience
///
/// Leaving a field as `None` preserves the solver crate's built-in default.
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct ProcessModelFitOptions {
    /// Optional LM tolerance applied through `with_tol(...)`.
    ///
    /// Smaller values drive a more rigorous fit but can cost more iterations.
    pub tolerance: Option<f64>,
    /// Optional LM patience factor applied through `with_patience(...)`.
    ///
    /// The solver interprets this as a multiplier on the parameter count when
    /// bounding function evaluations.
    pub patience: Option<usize>,
}

/// PID controller family produced by a tuning rule.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PidControllerKind {
    /// `PI` controller. The derivative path is disabled.
    Pi,
    /// `PIDF` controller with a first-order filtered derivative term.
    Pid,
}

/// SIMC-style tuning parameters shared by the low-order process-model rules.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SimcPidParams<R> {
    /// Desired closed-loop time constant / aggressiveness parameter.
    pub lambda: R,
    /// Whether to return a `PI` or `PIDF` controller.
    pub controller: PidControllerKind,
    /// Derivative filter ratio `N_f`, interpreted as `derivative_filter = N_f / Td`.
    pub derivative_filter_ratio: R,
    /// Runtime anti-windup policy to attach to the returned controller.
    pub anti_windup: AntiWindup<R>,
    /// Optional direct limits on the stored integral contribution.
    pub integrator_limits: Option<(R, R)>,
}

impl<R> SimcPidParams<R>
where
    R: Float,
{
    /// Creates validated tuning parameters.
    pub fn new(
        lambda: R,
        controller: PidControllerKind,
        derivative_filter_ratio: R,
        anti_windup: AntiWindup<R>,
    ) -> Result<Self, PidDesignError> {
        if !lambda.is_finite() || lambda <= R::zero() {
            return Err(PidDesignError::InvalidTuningParameter { which: "lambda" });
        }
        if !derivative_filter_ratio.is_finite() || derivative_filter_ratio <= R::zero() {
            return Err(PidDesignError::InvalidTuningParameter {
                which: "derivative_filter_ratio",
            });
        }
        Ok(Self {
            lambda,
            controller,
            derivative_filter_ratio,
            anti_windup,
            integrator_limits: None,
        })
    }

    /// Adds explicit integrator limits to the tuned controller.
    pub fn with_integrator_limits(mut self, low: R, high: R) -> Result<Self, PidDesignError> {
        if !low.is_finite() || !high.is_finite() || low > high {
            return Err(PidDesignError::InvalidTuningParameter {
                which: "integrator_limits",
            });
        }
        self.integrator_limits = Some((low, high));
        Ok(self)
    }
}

/// Result of tuning a PID controller from a low-order process model.
#[derive(Clone, Debug, PartialEq)]
pub struct ProcessPidDesign<R, M> {
    /// Tuned runtime controller.
    pub pid: Pid<R>,
    /// Process model the tuning rule used.
    pub model: M,
    /// Tuning parameters applied by the rule.
    pub params: SimcPidParams<R>,
}

/// Result of tuning from sampled step-response data.
#[derive(Clone, Debug, PartialEq)]
pub struct StepFitPidDesign<R, M> {
    /// Fitted process model.
    pub fit: ProcessFitResult<M, R>,
    /// Tuned controller using the fitted model.
    pub design: ProcessPidDesign<R, M>,
}

/// Frequency-domain PID tuning parameters for SISO plant models.
///
/// The implementation solves for:
///
/// - `Kp`
/// - `Ti`
///
/// and, for `Pid`, derives `Td = Ti / ti_over_td_ratio`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FrequencyPidParams<R> {
    /// Target crossover frequency in rad/s.
    pub crossover_frequency: R,
    /// Desired phase margin in degrees.
    pub phase_margin_deg: R,
    /// Whether to return a `PI` or `PIDF` controller.
    pub controller: PidControllerKind,
    /// Derivative filter ratio `N_f`, interpreted as `derivative_filter = N_f / Td`.
    pub derivative_filter_ratio: R,
    /// Fixed ratio `Ti / Td` used by the first PID frequency-design pass.
    pub ti_over_td_ratio: R,
    /// Runtime anti-windup policy attached to the returned controller.
    pub anti_windup: AntiWindup<R>,
    /// Optional direct limits on the stored integral contribution.
    pub integrator_limits: Option<(R, R)>,
}

impl<R> FrequencyPidParams<R>
where
    R: Float,
{
    /// Creates validated frequency-domain tuning parameters.
    pub fn new(
        crossover_frequency: R,
        phase_margin_deg: R,
        controller: PidControllerKind,
        derivative_filter_ratio: R,
        ti_over_td_ratio: R,
        anti_windup: AntiWindup<R>,
    ) -> Result<Self, PidDesignError> {
        if !crossover_frequency.is_finite() || crossover_frequency <= R::zero() {
            return Err(PidDesignError::InvalidFrequencyTarget {
                which: "crossover_frequency",
            });
        }
        if !phase_margin_deg.is_finite()
            || phase_margin_deg <= R::zero()
            || phase_margin_deg >= R::from(180.0).unwrap()
        {
            return Err(PidDesignError::InvalidFrequencyTarget {
                which: "phase_margin_deg",
            });
        }
        if !derivative_filter_ratio.is_finite() || derivative_filter_ratio <= R::zero() {
            return Err(PidDesignError::InvalidTuningParameter {
                which: "derivative_filter_ratio",
            });
        }
        if !ti_over_td_ratio.is_finite() || ti_over_td_ratio <= R::zero() {
            return Err(PidDesignError::InvalidTuningParameter {
                which: "ti_over_td_ratio",
            });
        }
        Ok(Self {
            crossover_frequency,
            phase_margin_deg,
            controller,
            derivative_filter_ratio,
            ti_over_td_ratio,
            anti_windup,
            integrator_limits: None,
        })
    }

    /// Adds explicit integrator limits to the tuned controller.
    pub fn with_integrator_limits(mut self, low: R, high: R) -> Result<Self, PidDesignError> {
        if !low.is_finite() || !high.is_finite() || low > high {
            return Err(PidDesignError::InvalidTuningParameter {
                which: "integrator_limits",
            });
        }
        self.integrator_limits = Some((low, high));
        Ok(self)
    }
}

/// Result of frequency-domain PID tuning.
#[derive(Clone, Debug, PartialEq)]
pub struct FrequencyPidDesign<R> {
    /// Tuned runtime controller.
    pub pid: Pid<R>,
    /// Applied frequency-domain tuning parameters.
    pub params: FrequencyPidParams<R>,
    /// Final open-loop value `L(jω_c)` or `L(e^{j ω_c dt})` at the target crossover.
    pub loop_value: Complex<R>,
    /// Achieved phase margin inferred from the final loop phase at the target.
    pub achieved_phase_margin_deg: R,
}

/// Step-response optimization tuning parameters for discrete-time plant models.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StepOptimizationPidParams<R> {
    /// Desired closed-loop time constant used to build the reference step response.
    pub lambda: R,
    /// Number of discrete samples used in the optimization horizon.
    pub horizon_steps: usize,
    /// Whether to return a `PI` or `PIDF` controller.
    pub controller: PidControllerKind,
    /// Derivative filter ratio `N_f`, interpreted as `derivative_filter = N_f / Td`.
    pub derivative_filter_ratio: R,
    /// Fixed ratio `Ti / Td` used by the first PID optimization pass.
    pub ti_over_td_ratio: R,
    /// Runtime anti-windup policy attached to the returned controller.
    pub anti_windup: AntiWindup<R>,
    /// Optional direct limits on the stored integral contribution.
    pub integrator_limits: Option<(R, R)>,
    /// Weight on the control-effort residual channel.
    pub control_effort_weight: R,
}

impl<R> StepOptimizationPidParams<R>
where
    R: Float,
{
    /// Creates validated discrete-time step-optimization tuning parameters.
    pub fn new(
        lambda: R,
        horizon_steps: usize,
        controller: PidControllerKind,
        derivative_filter_ratio: R,
        ti_over_td_ratio: R,
        anti_windup: AntiWindup<R>,
    ) -> Result<Self, PidDesignError> {
        if !lambda.is_finite() || lambda <= R::zero() {
            return Err(PidDesignError::InvalidTuningParameter { which: "lambda" });
        }
        if horizon_steps == 0 {
            return Err(PidDesignError::InvalidTuningParameter {
                which: "horizon_steps",
            });
        }
        if !derivative_filter_ratio.is_finite() || derivative_filter_ratio <= R::zero() {
            return Err(PidDesignError::InvalidTuningParameter {
                which: "derivative_filter_ratio",
            });
        }
        if !ti_over_td_ratio.is_finite() || ti_over_td_ratio <= R::zero() {
            return Err(PidDesignError::InvalidTuningParameter {
                which: "ti_over_td_ratio",
            });
        }
        Ok(Self {
            lambda,
            horizon_steps,
            controller,
            derivative_filter_ratio,
            ti_over_td_ratio,
            anti_windup,
            integrator_limits: None,
            control_effort_weight: R::zero(),
        })
    }

    /// Adds explicit integrator limits to the tuned controller.
    pub fn with_integrator_limits(mut self, low: R, high: R) -> Result<Self, PidDesignError> {
        if !low.is_finite() || !high.is_finite() || low > high {
            return Err(PidDesignError::InvalidTuningParameter {
                which: "integrator_limits",
            });
        }
        self.integrator_limits = Some((low, high));
        Ok(self)
    }

    /// Sets the control-effort penalty weight.
    pub fn with_control_effort_weight(mut self, weight: R) -> Result<Self, PidDesignError> {
        if !weight.is_finite() || weight < R::zero() {
            return Err(PidDesignError::InvalidTuningParameter {
                which: "control_effort_weight",
            });
        }
        self.control_effort_weight = weight;
        Ok(self)
    }
}

/// Result of discrete-time step-response PID optimization.
#[derive(Clone, Debug, PartialEq)]
pub struct StepOptimizationPidDesign<R> {
    /// Tuned runtime controller.
    pub pid: Pid<R>,
    /// Applied optimization parameters.
    pub params: StepOptimizationPidParams<R>,
    /// Sum of squared tracking residuals over the optimization horizon.
    pub tracking_cost: R,
    /// Sum of squared control-effort residuals over the optimization horizon.
    pub control_cost: R,
}

/// SISO sampled input/output data used by the `OKID -> ERA` PID path.
#[derive(Clone, Debug, PartialEq)]
pub struct SampledIoData<R> {
    sample_time: R,
    input: Vec<R>,
    output: Vec<R>,
}

impl<R> SampledIoData<R>
where
    R: Float,
{
    /// Creates validated sampled SISO input/output data.
    pub fn new(sample_time: R, input: Vec<R>, output: Vec<R>) -> Result<Self, PidDesignError> {
        if !sample_time.is_finite() || sample_time <= R::zero() {
            return Err(PidDesignError::InvalidData {
                which: "sample_time",
            });
        }
        if input.len() < 2 || input.len() != output.len() {
            return Err(PidDesignError::InvalidData {
                which: "length_mismatch",
            });
        }
        if input
            .iter()
            .chain(output.iter())
            .any(|value| !value.is_finite())
        {
            return Err(PidDesignError::InvalidData {
                which: "nonfinite_sample",
            });
        }
        Ok(Self {
            sample_time,
            input,
            output,
        })
    }

    /// Sample interval.
    #[must_use]
    pub fn sample_time(&self) -> R {
        self.sample_time
    }

    /// Input samples.
    #[must_use]
    pub fn input(&self) -> &[R] {
        &self.input
    }

    /// Output samples.
    #[must_use]
    pub fn output(&self) -> &[R] {
        &self.output
    }
}

/// Result of the `OKID -> ERA -> PID` tuning path.
#[derive(Clone, Debug, PartialEq)]
pub struct OkidEraPidDesign<R: faer_traits::ComplexField> {
    /// Identified discrete-time plant realization.
    pub identified_plant: DiscreteStateSpace<R>,
    /// Final tuned controller on that identified model.
    pub design: StepOptimizationPidDesign<R>,
    /// ERA retained order of the identified model.
    pub retained_order: usize,
}

/// Fits an `FOPDT` model to sampled step-response data.
pub fn fit_fopdt_from_step_response(
    data: &StepResponseData<f64>,
) -> Result<ProcessFitResult<FopdtModel<f64>, f64>, PidDesignError> {
    fit_fopdt_from_step_response_with_options(data, ProcessModelFitOptions::default())
}

/// Fits an `FOPDT` model to sampled step-response data with explicit solver
/// options.
pub fn fit_fopdt_from_step_response_with_options(
    data: &StepResponseData<f64>,
    options: ProcessModelFitOptions,
) -> Result<ProcessFitResult<FopdtModel<f64>, f64>, PidDesignError> {
    let prepared = PreparedStepResponse::from_data(data)?;
    let initial = initial_fopdt_estimate(&prepared)?;
    let initial_objective = fopdt_objective(&prepared, initial);
    let model = match refine_fopdt(&prepared, initial, options) {
        Ok(refined) => {
            let refined_objective = fopdt_objective(&prepared, refined);
            if refined_objective <= initial_objective {
                refined
            } else {
                initial
            }
        }
        Err(_) => initial,
    };
    let objective = fopdt_objective(&prepared, model);
    Ok(ProcessFitResult {
        initial,
        model,
        objective,
    })
}

/// Fits an `SOPDT` model to sampled step-response data.
pub fn fit_sopdt_from_step_response(
    data: &StepResponseData<f64>,
) -> Result<ProcessFitResult<SopdtModel<f64>, f64>, PidDesignError> {
    fit_sopdt_from_step_response_with_options(data, ProcessModelFitOptions::default())
}

/// Fits an `SOPDT` model to sampled step-response data with explicit solver
/// options.
pub fn fit_sopdt_from_step_response_with_options(
    data: &StepResponseData<f64>,
    options: ProcessModelFitOptions,
) -> Result<ProcessFitResult<SopdtModel<f64>, f64>, PidDesignError> {
    let prepared = PreparedStepResponse::from_data(data)?;
    let fopdt = initial_fopdt_estimate(&prepared)?;
    let initial = initial_sopdt_estimate(&prepared, fopdt);
    let initial_objective = sopdt_objective(&prepared, initial);
    let model = match refine_sopdt(&prepared, initial, options) {
        Ok(refined) => {
            let refined_objective = sopdt_objective(&prepared, refined);
            if refined_objective <= initial_objective {
                refined
            } else {
                initial
            }
        }
        Err(_) => initial,
    };
    let objective = sopdt_objective(&prepared, model);
    Ok(ProcessFitResult {
        initial,
        model,
        objective,
    })
}

/// Tunes a `PI` or `PIDF` controller from an `FOPDT` process model.
pub fn design_pid_from_fopdt<R>(
    model: FopdtModel<R>,
    params: SimcPidParams<R>,
) -> Result<ProcessPidDesign<R, FopdtModel<R>>, PidDesignError>
where
    R: Float + RealField,
{
    validate_process_gain(model.gain)?;
    validate_positive(model.time_constant, "time_constant")?;
    validate_nonnegative(model.delay, "delay")?;

    let two = R::from(2.0).unwrap();
    let four = R::from(4.0).unwrap();
    let kp = model.time_constant / (model.gain * (params.lambda + model.delay));
    let (ki, kd, derivative_filter) = match params.controller {
        PidControllerKind::Pi => {
            let ti = model
                .time_constant
                .min(four * (params.lambda + model.delay));
            (kp / ti, R::zero(), None)
        }
        PidControllerKind::Pid => {
            let ti = model.time_constant + model.delay / two;
            let td =
                (model.time_constant * model.delay) / (two * model.time_constant + model.delay);
            let derivative_filter = if td > R::zero() {
                Some(params.derivative_filter_ratio / td)
            } else {
                None
            };
            (kp / ti, kp * td, derivative_filter)
        }
    };

    let mut pid = Pid::new(kp, ki, kd, derivative_filter, params.anti_windup)?;
    if let Some((low, high)) = params.integrator_limits {
        pid = pid.with_integrator_limits(low, high)?;
    }

    Ok(ProcessPidDesign { pid, model, params })
}

/// Tunes a `PI` or `PIDF` controller from an `SOPDT` process model.
pub fn design_pid_from_sopdt<R>(
    model: SopdtModel<R>,
    params: SimcPidParams<R>,
) -> Result<ProcessPidDesign<R, SopdtModel<R>>, PidDesignError>
where
    R: Float + RealField,
{
    validate_process_gain(model.gain)?;
    validate_positive(model.time_constant_1, "time_constant_1")?;
    validate_positive(model.time_constant_2, "time_constant_2")?;
    validate_nonnegative(model.delay, "delay")?;

    let lag_sum = model.time_constant_1 + model.time_constant_2;
    let four = R::from(4.0).unwrap();
    let kp = lag_sum / (model.gain * (params.lambda + model.delay));
    let (ki, kd, derivative_filter) = match params.controller {
        PidControllerKind::Pi => {
            let ti = lag_sum.min(four * (params.lambda + model.delay));
            (kp / ti, R::zero(), None)
        }
        PidControllerKind::Pid => {
            let ti = lag_sum;
            let td = (model.time_constant_1 * model.time_constant_2) / lag_sum;
            let derivative_filter = if td > R::zero() {
                Some(params.derivative_filter_ratio / td)
            } else {
                None
            };
            (kp / ti, kp * td, derivative_filter)
        }
    };

    let mut pid = Pid::new(kp, ki, kd, derivative_filter, params.anti_windup)?;
    if let Some((low, high)) = params.integrator_limits {
        pid = pid.with_integrator_limits(low, high)?;
    }

    Ok(ProcessPidDesign { pid, model, params })
}

/// Fits an `FOPDT` model from sampled step data and tunes from the result.
pub fn design_pid_from_step_response_fopdt(
    data: &StepResponseData<f64>,
    params: SimcPidParams<f64>,
) -> Result<StepFitPidDesign<f64, FopdtModel<f64>>, PidDesignError> {
    let fit = fit_fopdt_from_step_response(data)?;
    let design = design_pid_from_fopdt(fit.model, params)?;
    Ok(StepFitPidDesign { fit, design })
}

/// Fits an `SOPDT` model from sampled step data and tunes from the result.
pub fn design_pid_from_step_response_sopdt(
    data: &StepResponseData<f64>,
    params: SimcPidParams<f64>,
) -> Result<StepFitPidDesign<f64, SopdtModel<f64>>, PidDesignError> {
    let fit = fit_sopdt_from_step_response(data)?;
    let design = design_pid_from_sopdt(fit.model, params)?;
    Ok(StepFitPidDesign { fit, design })
}

/// Tunes a `PI` or `PIDF` controller against a continuous-time SISO transfer
/// function at one target crossover frequency and phase margin.
pub fn design_pid_from_continuous_tf_frequency(
    plant: &ContinuousTransferFunction<f64>,
    params: FrequencyPidParams<f64>,
) -> Result<FrequencyPidDesign<f64>, PidDesignError> {
    design_frequency_pid(FrequencyPlant::Continuous(plant), params)
}

/// Tunes a `PI` or `PIDF` controller against a discrete-time SISO transfer
/// function at one target crossover frequency and phase margin.
pub fn design_pid_from_discrete_tf_frequency(
    plant: &DiscreteTransferFunction<f64>,
    params: FrequencyPidParams<f64>,
) -> Result<FrequencyPidDesign<f64>, PidDesignError> {
    design_frequency_pid(FrequencyPlant::Discrete(plant), params)
}

/// Frequency-domain PID tuning convenience wrapper for a continuous-time SISO
/// state-space plant.
pub fn design_pid_from_continuous_state_space_frequency(
    plant: &ContinuousStateSpace<f64>,
    params: FrequencyPidParams<f64>,
) -> Result<FrequencyPidDesign<f64>, PidDesignError> {
    let tf = plant.to_transfer_function()?;
    design_pid_from_continuous_tf_frequency(&tf, params)
}

/// Frequency-domain PID tuning convenience wrapper for a discrete-time SISO
/// state-space plant.
pub fn design_pid_from_discrete_state_space_frequency(
    plant: &DiscreteStateSpace<f64>,
    params: FrequencyPidParams<f64>,
) -> Result<FrequencyPidDesign<f64>, PidDesignError> {
    let tf = plant.to_transfer_function()?;
    design_pid_from_discrete_tf_frequency(&tf, params)
}

/// Optimizes a `PI` or `PIDF` controller against the step response of a
/// discrete-time SISO transfer-function plant.
pub fn design_pid_from_discrete_tf_step_optimization(
    plant: &DiscreteTransferFunction<f64>,
    params: StepOptimizationPidParams<f64>,
) -> Result<StepOptimizationPidDesign<f64>, PidDesignError> {
    design_step_optimized_pid(plant, params)
}

/// Step-response optimization convenience wrapper for a discrete-time SISO
/// state-space plant.
pub fn design_pid_from_discrete_state_space_step_optimization(
    plant: &DiscreteStateSpace<f64>,
    params: StepOptimizationPidParams<f64>,
) -> Result<StepOptimizationPidDesign<f64>, PidDesignError> {
    let tf = plant.to_transfer_function()?;
    design_pid_from_discrete_tf_step_optimization(&tf, params)
}

/// Identifies a discrete-time plant with `OKID -> ERA` and tunes a `PI` or
/// `PIDF` controller against the identified model with step-response
/// optimization.
pub fn design_pid_from_okid_era(
    data: &SampledIoData<f64>,
    okid_params: &OkidParams,
    mut era_params: EraParams<f64>,
    pid_params: StepOptimizationPidParams<f64>,
) -> Result<OkidEraPidDesign<f64>, PidDesignError> {
    let nsamples = data.input().len();
    let inputs = Mat::from_fn(1, nsamples, |_, col| data.input()[col]);
    let outputs = Mat::from_fn(1, nsamples, |_, col| data.output()[col]);
    let okid_result = okid(outputs.as_ref(), inputs.as_ref(), okid_params)?;
    let q = max_square_era_block_dim(okid_result.markov.len());
    if q == 0 {
        return Err(PidDesignError::InvalidData {
            which: "okid_era.block_dim",
        });
    }
    era_params.sample_time = data.sample_time();
    let era_result = era_from_markov(&okid_result.markov, q, q, &era_params)?;
    let design =
        design_pid_from_discrete_state_space_step_optimization(&era_result.realized, pid_params)?;
    Ok(OkidEraPidDesign {
        identified_plant: era_result.realized,
        design,
        retained_order: era_result.retained_order,
    })
}

#[derive(Clone, Copy)]
enum FrequencyPlant<'a> {
    Continuous(&'a ContinuousTransferFunction<f64>),
    Discrete(&'a DiscreteTransferFunction<f64>),
}

impl FrequencyPlant<'_> {
    /// Evaluates the plant on the appropriate frequency-domain contour.
    fn evaluate_at_omega(self, omega: f64) -> Complex<f64> {
        match self {
            Self::Continuous(plant) => plant.evaluate(Complex::new(0.0, omega)),
            Self::Discrete(plant) => {
                let phase = omega * plant.sample_time();
                plant.evaluate(Complex::new(phase.cos(), phase.sin()))
            }
        }
    }
}

#[derive(Clone, Copy)]
struct LinearPidSpec {
    kp: f64,
    ki: f64,
    kd: f64,
    derivative_filter: Option<f64>,
}

fn design_frequency_pid(
    plant: FrequencyPlant<'_>,
    params: FrequencyPidParams<f64>,
) -> Result<FrequencyPidDesign<f64>, PidDesignError> {
    let floor = positive_floor(1.0 / params.crossover_frequency.max(1.0));
    let plant_value = plant.evaluate_at_omega(params.crossover_frequency);
    let mag_seed = (1.0 / plant_value.abs().max(1.0e-6)).max(floor);
    let ti_seed = (1.0 / params.crossover_frequency).max(floor);

    let kp_scales = [0.25, 1.0, 4.0];
    let ti_scales = [0.25, 1.0, 4.0];
    let signs = [1.0, -1.0];
    let mut best: Option<(FrequencyPidLmProblem<'_>, f64)> = None;

    for &sign in &signs {
        for &kp_scale in &kp_scales {
            for &ti_scale in &ti_scales {
                let seed = Vector2::new((mag_seed * kp_scale).ln(), (ti_seed * ti_scale).ln());
                let problem = FrequencyPidLmProblem {
                    plant,
                    params,
                    sign,
                    floor,
                    variables: seed,
                };
                let (problem, _report) = LevenbergMarquardt::new()
                    .with_patience(20)
                    .minimize(problem);
                let objective = frequency_objective(&problem.residual_vector());
                if objective.is_finite()
                    && best
                        .as_ref()
                        .is_none_or(|(_, best_objective)| objective < *best_objective)
                {
                    best = Some((problem, objective));
                }
            }
        }
    }

    let (best_problem, best_objective) = best.ok_or(PidDesignError::OptimizationFailed {
        which: "frequency_design",
    })?;
    if !best_objective.is_finite() {
        return Err(PidDesignError::OptimizationFailed {
            which: "frequency_design",
        });
    }

    let spec = best_problem.linear_pid_spec();
    let pid = build_runtime_pid_from_spec(spec, params.anti_windup, params.integrator_limits)?;
    let loop_value = open_loop_at_frequency(plant, &spec, params.crossover_frequency)?;
    let achieved_phase_margin_deg = phase_margin_deg(loop_value);
    Ok(FrequencyPidDesign {
        pid,
        params,
        loop_value,
        achieved_phase_margin_deg,
    })
}

fn design_step_optimized_pid(
    plant: &DiscreteTransferFunction<f64>,
    params: StepOptimizationPidParams<f64>,
) -> Result<StepOptimizationPidDesign<f64>, PidDesignError> {
    let floor = positive_floor(params.lambda);
    let dc_gain = plant.evaluate(Complex::new(1.0, 0.0));
    let kp_seed_mag = (1.0 / dc_gain.abs().max(1.0e-6)).max(floor);
    let ti_seed = params.lambda.max(floor);
    let predicted_sign = if dc_gain.re.abs() > 1.0e-9 {
        dc_gain.re.signum()
    } else {
        1.0
    };

    let kp_scales = [0.5, 1.0, 2.0];
    let ti_scales = [0.5, 1.0, 2.0];
    let signs = [predicted_sign, -predicted_sign];
    let mut best_seed: Option<(DiscreteStepPidLmProblem<'_>, f64)> = None;

    for &sign in &signs {
        for &kp_scale in &kp_scales {
            for &ti_scale in &ti_scales {
                let seed = Vector2::new((kp_seed_mag * kp_scale).ln(), (ti_seed * ti_scale).ln());
                let problem = DiscreteStepPidLmProblem {
                    plant,
                    params,
                    sign,
                    floor,
                    variables: seed,
                };
                let objective = step_objective(&problem.residual_vector());
                if objective.is_finite()
                    && best_seed
                        .as_ref()
                        .is_none_or(|(_, best_objective)| objective < *best_objective)
                {
                    best_seed = Some((problem, objective));
                }
            }
        }
    }

    let (seed_problem, seed_objective) = best_seed.ok_or(PidDesignError::OptimizationFailed {
        which: "step_optimization",
    })?;
    if !seed_objective.is_finite() {
        return Err(PidDesignError::OptimizationFailed {
            which: "step_optimization",
        });
    }

    let (refined_problem, _report) = LevenbergMarquardt::new()
        .with_patience(8)
        .minimize(seed_problem);
    let refined_objective = step_objective(&refined_problem.residual_vector());
    let best_problem = if refined_objective.is_finite() && refined_objective <= seed_objective {
        refined_problem
    } else {
        seed_problem
    };

    let spec = best_problem.linear_pid_spec();
    let pid = build_runtime_pid_from_spec(spec, params.anti_windup, params.integrator_limits)?;
    let (tracking_cost, control_cost) = discrete_closed_loop_costs(plant, &spec, params)?;
    Ok(StepOptimizationPidDesign {
        pid,
        params,
        tracking_cost,
        control_cost,
    })
}

fn open_loop_at_frequency(
    plant: FrequencyPlant<'_>,
    controller: &LinearPidSpec,
    omega: f64,
) -> Result<Complex<f64>, PidDesignError> {
    let controller = build_linear_pid_core(*controller)?;
    let controller_value = match plant {
        FrequencyPlant::Continuous(_) => controller
            .to_transfer_function_continuous()?
            .evaluate(Complex::new(0.0, omega)),
        FrequencyPlant::Discrete(plant) => controller
            .to_transfer_function_discrete(plant.sample_time())?
            .evaluate(Complex::new(
                (omega * plant.sample_time()).cos(),
                (omega * plant.sample_time()).sin(),
            )),
    };
    Ok(controller_value * plant.evaluate_at_omega(omega))
}

fn build_linear_pid_spec_from_variables(
    kp_sign: f64,
    variables: Vector2<f64>,
    controller: PidControllerKind,
    derivative_filter_ratio: f64,
    ti_over_td_ratio: f64,
    floor: f64,
) -> LinearPidSpec {
    let kp = kp_sign * variables[0].exp().max(floor);
    let ti = variables[1].exp().max(floor);
    let ki = kp / ti;
    match controller {
        PidControllerKind::Pi => LinearPidSpec {
            kp,
            ki,
            kd: 0.0,
            derivative_filter: None,
        },
        PidControllerKind::Pid => {
            let td = (ti / ti_over_td_ratio).max(floor);
            LinearPidSpec {
                kp,
                ki,
                kd: kp * td,
                derivative_filter: Some(derivative_filter_ratio / td),
            }
        }
    }
}

fn build_linear_pid_core(spec: LinearPidSpec) -> Result<Pid<f64>, PidDesignError> {
    Ok(Pid::new(
        spec.kp,
        spec.ki,
        spec.kd,
        spec.derivative_filter,
        AntiWindup::None,
    )?)
}

fn build_runtime_pid_from_spec(
    spec: LinearPidSpec,
    anti_windup: AntiWindup<f64>,
    integrator_limits: Option<(f64, f64)>,
) -> Result<Pid<f64>, PidDesignError> {
    let mut pid = Pid::new(
        spec.kp,
        spec.ki,
        spec.kd,
        spec.derivative_filter,
        anti_windup,
    )?;
    if let Some((low, high)) = integrator_limits {
        pid = pid.with_integrator_limits(low, high)?;
    }
    Ok(pid)
}

fn frequency_objective(residuals: &OVector<f64, Dyn>) -> f64 {
    residuals.iter().map(|value| value * value).sum()
}

fn step_objective(residuals: &OVector<f64, Dyn>) -> f64 {
    residuals.iter().map(|value| value * value).sum()
}

fn wrap_to_pi(angle: f64) -> f64 {
    let two_pi = core::f64::consts::TAU;
    let shifted = (angle + core::f64::consts::PI) / two_pi;
    let wrapped = angle + core::f64::consts::PI - two_pi * shifted.floor() - core::f64::consts::PI;
    if wrapped == -core::f64::consts::PI {
        core::f64::consts::PI
    } else {
        wrapped
    }
}

fn phase_margin_deg(loop_value: Complex<f64>) -> f64 {
    let mut margin = (core::f64::consts::PI + loop_value.im.atan2(loop_value.re)).to_degrees();
    if margin < 0.0 {
        margin += 360.0;
    }
    margin
}

fn reference_step_sample(k: usize, sample_time: f64, lambda: f64) -> f64 {
    1.0 - (-(k as f64 * sample_time) / lambda).exp()
}

fn discrete_closed_loop_costs(
    plant: &DiscreteTransferFunction<f64>,
    spec: &LinearPidSpec,
    params: StepOptimizationPidParams<f64>,
) -> Result<(f64, f64), PidDesignError> {
    let controller = build_linear_pid_core(*spec)?;
    let controller_tf = controller.to_transfer_function_discrete(plant.sample_time())?;
    let open_loop = controller_tf.mul(plant)?;
    let closed_output = open_loop.unity_feedback()?;
    let closed_control = controller_tf.feedback(plant)?;
    let closed_output_ss = closed_output.to_state_space()?;
    if !closed_output_ss.is_asymptotically_stable()? {
        return Err(PidDesignError::OptimizationFailed {
            which: "unstable_closed_loop",
        });
    }
    let closed_control_ss = closed_control.to_state_space()?;
    let output_response = closed_output_ss.step_response(params.horizon_steps);
    let control_response = closed_control_ss.step_response(params.horizon_steps);

    let mut tracking_cost = 0.0;
    let mut control_cost = 0.0;
    let control_weight = params.control_effort_weight.sqrt();
    for k in 0..params.horizon_steps {
        let y = output_response.values[k][(0, 0)];
        let u = control_response.values[k][(0, 0)];
        let y_ref = reference_step_sample(k, plant.sample_time(), params.lambda);
        let track_residual = y - y_ref;
        tracking_cost += track_residual * track_residual;
        let control_residual = control_weight * u;
        control_cost += control_residual * control_residual;
    }
    Ok((tracking_cost, control_cost))
}

#[derive(Clone, Copy)]
struct FrequencyPidLmProblem<'a> {
    plant: FrequencyPlant<'a>,
    params: FrequencyPidParams<f64>,
    sign: f64,
    floor: f64,
    variables: Vector2<f64>,
}

impl FrequencyPidLmProblem<'_> {
    fn linear_pid_spec(&self) -> LinearPidSpec {
        build_linear_pid_spec_from_variables(
            self.sign,
            self.variables,
            self.params.controller,
            self.params.derivative_filter_ratio,
            self.params.ti_over_td_ratio,
            self.floor,
        )
    }

    fn residual_vector(&self) -> OVector<f64, Dyn> {
        let spec = self.linear_pid_spec();
        let residuals =
            match open_loop_at_frequency(self.plant, &spec, self.params.crossover_frequency) {
                Ok(loop_value) => {
                    let desired_phase =
                        -core::f64::consts::PI + self.params.phase_margin_deg.to_radians();
                    vec![
                        loop_value.abs().ln(),
                        wrap_to_pi(loop_value.im.atan2(loop_value.re) - desired_phase),
                    ]
                }
                Err(_) => vec![1.0e6, 1.0e6],
            };
        OVector::<f64, Dyn>::from_vec(residuals)
    }
}

impl LeastSquaresProblem<f64, Dyn, U2> for FrequencyPidLmProblem<'_> {
    type ParameterStorage = Owned<f64, U2>;
    type ResidualStorage = VecStorage<f64, Dyn, U1>;
    type JacobianStorage = Owned<f64, Dyn, U2>;

    fn set_params(&mut self, x: &Vector2<f64>) {
        self.variables = *x;
    }

    fn params(&self) -> Vector2<f64> {
        self.variables
    }

    fn residuals(&self) -> Option<OVector<f64, Dyn>> {
        Some(self.residual_vector())
    }

    fn jacobian(&self) -> Option<OMatrix<f64, Dyn, U2>> {
        let mut clone = *self;
        differentiate_numerically(&mut clone)
    }
}

#[derive(Clone, Copy)]
struct DiscreteStepPidLmProblem<'a> {
    plant: &'a DiscreteTransferFunction<f64>,
    params: StepOptimizationPidParams<f64>,
    sign: f64,
    floor: f64,
    variables: Vector2<f64>,
}

impl DiscreteStepPidLmProblem<'_> {
    fn linear_pid_spec(&self) -> LinearPidSpec {
        build_linear_pid_spec_from_variables(
            self.sign,
            self.variables,
            self.params.controller,
            self.params.derivative_filter_ratio,
            self.params.ti_over_td_ratio,
            self.floor,
        )
    }

    fn residual_vector(&self) -> OVector<f64, Dyn> {
        let spec = self.linear_pid_spec();
        let controller = match build_linear_pid_core(spec) {
            Ok(controller) => controller,
            Err(_) => return step_optimization_penalty_vector(self.params.horizon_steps),
        };
        let controller_tf = match controller.to_transfer_function_discrete(self.plant.sample_time())
        {
            Ok(tf) => tf,
            Err(_) => return step_optimization_penalty_vector(self.params.horizon_steps),
        };
        let open_loop = match controller_tf.mul(self.plant) {
            Ok(tf) => tf,
            Err(_) => return step_optimization_penalty_vector(self.params.horizon_steps),
        };
        let closed_output = match open_loop.unity_feedback() {
            Ok(tf) => tf,
            Err(_) => return step_optimization_penalty_vector(self.params.horizon_steps),
        };
        let closed_control = match controller_tf.feedback(self.plant) {
            Ok(tf) => tf,
            Err(_) => return step_optimization_penalty_vector(self.params.horizon_steps),
        };
        let closed_output_ss = match closed_output.to_state_space() {
            Ok(ss) => ss,
            Err(_) => return step_optimization_penalty_vector(self.params.horizon_steps),
        };
        let stable = closed_output_ss.is_asymptotically_stable().unwrap_or(false);
        if !stable {
            return step_optimization_penalty_vector(self.params.horizon_steps);
        }
        let closed_control_ss = match closed_control.to_state_space() {
            Ok(ss) => ss,
            Err(_) => return step_optimization_penalty_vector(self.params.horizon_steps),
        };
        let output_response = closed_output_ss.step_response(self.params.horizon_steps);
        let control_response = closed_control_ss.step_response(self.params.horizon_steps);
        let mut residuals = Vec::with_capacity(
            self.params.horizon_steps
                + usize::from(self.params.control_effort_weight > 0.0) * self.params.horizon_steps,
        );
        for k in 0..self.params.horizon_steps {
            let y_ref = reference_step_sample(k, self.plant.sample_time(), self.params.lambda);
            residuals.push(output_response.values[k][(0, 0)] - y_ref);
        }
        if self.params.control_effort_weight > 0.0 {
            let control_weight = self.params.control_effort_weight.sqrt();
            for k in 0..self.params.horizon_steps {
                residuals.push(control_weight * control_response.values[k][(0, 0)]);
            }
        }
        OVector::<f64, Dyn>::from_vec(residuals)
    }
}

impl LeastSquaresProblem<f64, Dyn, U2> for DiscreteStepPidLmProblem<'_> {
    type ParameterStorage = Owned<f64, U2>;
    type ResidualStorage = VecStorage<f64, Dyn, U1>;
    type JacobianStorage = Owned<f64, Dyn, U2>;

    fn set_params(&mut self, x: &Vector2<f64>) {
        self.variables = *x;
    }

    fn params(&self) -> Vector2<f64> {
        self.variables
    }

    fn residuals(&self) -> Option<OVector<f64, Dyn>> {
        Some(self.residual_vector())
    }

    fn jacobian(&self) -> Option<OMatrix<f64, Dyn, U2>> {
        let mut clone = *self;
        differentiate_numerically(&mut clone)
    }
}

#[derive(Clone, Debug)]
struct PreparedStepResponse {
    /// Time grid shifted so the dominant input step occurs near `t = 0`.
    time_relative: Vec<f64>,
    /// Measured output samples on that shifted grid.
    output: Vec<f64>,
    /// Estimated pre-step output baseline.
    initial_output: f64,
    /// Estimated input step amplitude.
    step_amplitude: f64,
    /// Coarse steady-state process gain estimate from baseline/tail averages.
    gain_guess: f64,
    /// Characteristic experiment time span used to scale positivity floors.
    time_scale: f64,
}

impl PreparedStepResponse {
    /// Normalizes raw sampled step data into the quantities needed by the
    /// landmark estimators and LM refinement.
    ///
    /// The preprocessing intentionally looks for one dominant input jump and
    /// then computes pre-step and post-step averages around that detected
    /// transition. That keeps the first fitting pass simple and robust for the
    /// intended "single experiment, single step" workflow.
    fn from_data(data: &StepResponseData<f64>) -> Result<Self, PidDesignError> {
        let n = data.time.len();
        let step_index =
            dominant_step_index(&data.input).ok_or(PidDesignError::InvalidStepAmplitude)?;
        let pre_end = (step_index + 1).max(1);
        let post_start = (step_index + 1).min(n - 1);
        let tail = tail_count(n - post_start);

        let initial_input = mean(&data.input[..pre_end]);
        let final_input = mean(&data.input[post_start..]);
        let initial_output = mean(&data.output[..pre_end]);
        let final_output = mean(&data.output[n - tail..]);

        let step_amplitude = final_input - initial_input;
        if !step_amplitude.is_finite() || step_amplitude.abs() <= 1.0e-12 {
            return Err(PidDesignError::InvalidStepAmplitude);
        }
        let output_change = final_output - initial_output;
        if !output_change.is_finite() || output_change.abs() <= 1.0e-12 {
            return Err(PidDesignError::InvalidResponseAmplitude);
        }

        let step_time = interpolate_input_step_time(&data.time, &data.input, step_index)?;

        let time_relative = data.time.iter().map(|&t| t - step_time).collect::<Vec<_>>();
        let time_scale = (data.time[n - 1] - data.time[0]).abs().max(1.0e-6);
        Ok(Self {
            time_relative,
            output: data.output.clone(),
            initial_output,
            step_amplitude,
            gain_guess: output_change / step_amplitude,
            time_scale,
        })
    }
}

/// Forms the first FOPDT estimate from standard normalized step landmarks.
///
/// The `28.3%` and `63.2%` crossings give a coarse delay/lag split that is
/// easy to compute and usually good enough to seed local nonlinear refinement.
fn initial_fopdt_estimate(data: &PreparedStepResponse) -> Result<FopdtModel<f64>, PidDesignError> {
    let final_output = *data.output.last().ok_or(PidDesignError::InvalidData {
        which: "output.last",
    })?;
    let total_change = final_output - data.initial_output;
    let gain = data.gain_guess;
    let sign = total_change.signum();
    let level_283 = data.initial_output + 0.283 * total_change;
    let level_632 = data.initial_output + 0.632 * total_change;

    let start_index = first_nonnegative_index(&data.time_relative);
    let t_283 = interpolate_crossing(
        &data.time_relative,
        &data.output,
        level_283,
        sign,
        start_index,
    )
    .ok_or(PidDesignError::InvalidDelayEstimate)?;
    let t_632 = interpolate_crossing(
        &data.time_relative,
        &data.output,
        level_632,
        sign,
        start_index,
    )
    .ok_or(PidDesignError::InvalidDelayEstimate)?;

    let tau = 1.5 * (t_632 - t_283);
    let delay = (t_632 - tau).max(0.0);
    if !tau.is_finite() || tau <= 0.0 || !delay.is_finite() {
        return Err(PidDesignError::InvalidProcessFit);
    }

    Ok(FopdtModel {
        gain,
        time_constant: tau,
        delay,
    })
}

/// Builds a physically plausible SOPDT seed from the coarser FOPDT estimate.
///
/// This does not try to solve the second-order fit in closed form. Instead it
/// searches a small deterministic grid of lag splits and delay scalings and
/// keeps the candidate with the smallest direct step-response residual.
fn initial_sopdt_estimate(data: &PreparedStepResponse, fopdt: FopdtModel<f64>) -> SopdtModel<f64> {
    let floor = positive_floor(data.time_scale);
    let total_scales = [0.5, 0.75, 1.0, 1.25, 1.5];
    let split_fractions = [0.55, 0.65, 0.75, 0.85, 0.95];
    let delay_scales = [0.5, 1.0, 1.5];

    let mut best = SopdtModel {
        gain: fopdt.gain,
        time_constant_1: (0.75 * fopdt.time_constant).max(floor),
        time_constant_2: (0.25 * fopdt.time_constant).max(floor),
        delay: fopdt.delay.max(0.0),
    };
    let mut best_objective = sopdt_objective(data, best);

    for &total_scale in &total_scales {
        let total = (fopdt.time_constant * total_scale).max(2.0 * floor);
        for &split in &split_fractions {
            let tau1 = (total * split).max(floor);
            let tau2 = (total * (1.0 - split)).max(floor);
            for &delay_scale in &delay_scales {
                let candidate = SopdtModel {
                    gain: fopdt.gain,
                    time_constant_1: tau1.max(tau2),
                    time_constant_2: tau1.min(tau2),
                    delay: (fopdt.delay * delay_scale).max(0.0),
                };
                let objective = sopdt_objective(data, candidate);
                if objective < best_objective {
                    best = candidate;
                    best_objective = objective;
                }
            }
        }
    }

    best
}

/// Runs LM refinement for the FOPDT parameters.
///
/// The time constant and delay are optimized in log coordinates so the
/// recovered model stays positive without adding explicit inequality
/// constraints to the least-squares problem.
fn refine_fopdt(
    data: &PreparedStepResponse,
    initial: FopdtModel<f64>,
    options: ProcessModelFitOptions,
) -> Result<FopdtModel<f64>, PidDesignError> {
    let floor = positive_floor(data.time_scale);
    let problem = FopdtLmProblem {
        data,
        params: Vector3::new(
            initial.gain,
            initial.time_constant.max(floor).ln(),
            initial.delay.max(floor).ln(),
        ),
        floor,
    };
    let (problem, report) = process_fit_solver(options)?.minimize(problem);
    let model = problem.model();
    if !report.termination.was_successful()
        || !model.gain.is_finite()
        || !model.time_constant.is_finite()
        || !model.delay.is_finite()
        || model.time_constant <= 0.0
        || model.delay < 0.0
    {
        return Err(PidDesignError::InvalidProcessFit);
    }
    Ok(model)
}

/// Runs LM refinement for the SOPDT parameters.
///
/// The parameterization uses:
///
/// - one base lag
/// - one positive lag separation
/// - one positive delay
///
/// so both time constants remain positive and ordered automatically.
fn refine_sopdt(
    data: &PreparedStepResponse,
    initial: SopdtModel<f64>,
    options: ProcessModelFitOptions,
) -> Result<SopdtModel<f64>, PidDesignError> {
    let floor = positive_floor(data.time_scale);
    let base = initial.time_constant_2.max(floor);
    let extra = (initial.time_constant_1 - initial.time_constant_2)
        .abs()
        .max(floor);
    let problem = SopdtLmProblem {
        data,
        params: Vector4::new(
            initial.gain,
            base.ln(),
            extra.ln(),
            initial.delay.max(floor).ln(),
        ),
        floor,
    };
    let (problem, report) = process_fit_solver(options)?.minimize(problem);
    let model = problem.model();
    if !report.termination.was_successful()
        || !model.gain.is_finite()
        || !model.time_constant_1.is_finite()
        || !model.time_constant_2.is_finite()
        || !model.delay.is_finite()
        || model.time_constant_1 <= 0.0
        || model.time_constant_2 <= 0.0
        || model.delay < 0.0
    {
        return Err(PidDesignError::InvalidProcessFit);
    }
    Ok(model)
}

fn process_fit_solver(
    options: ProcessModelFitOptions,
) -> Result<LevenbergMarquardt<f64>, PidDesignError> {
    let mut solver = LevenbergMarquardt::new();
    if let Some(tolerance) = options.tolerance {
        if !tolerance.is_finite() || tolerance <= 0.0 {
            return Err(PidDesignError::InvalidTuningParameter {
                which: "process_fit_tolerance",
            });
        }
        solver = solver.with_tol(tolerance);
    }
    if let Some(patience) = options.patience {
        if patience == 0 {
            return Err(PidDesignError::InvalidTuningParameter {
                which: "process_fit_patience",
            });
        }
        solver = solver.with_patience(patience);
    }
    Ok(solver)
}

/// Sum-of-squares objective used to compare FOPDT candidates.
fn fopdt_objective(data: &PreparedStepResponse, model: FopdtModel<f64>) -> f64 {
    data.time_relative
        .iter()
        .zip(data.output.iter())
        .map(|(&t, &y)| {
            let residual =
                model.step_response_value(t, data.step_amplitude, data.initial_output) - y;
            residual * residual
        })
        .sum()
}

/// Sum-of-squares objective used to compare SOPDT candidates.
fn sopdt_objective(data: &PreparedStepResponse, model: SopdtModel<f64>) -> f64 {
    data.time_relative
        .iter()
        .zip(data.output.iter())
        .map(|(&t, &y)| {
            let residual =
                model.step_response_value(t, data.step_amplitude, data.initial_output) - y;
            residual * residual
        })
        .sum()
}

#[derive(Clone)]
struct FopdtLmProblem<'a> {
    data: &'a PreparedStepResponse,
    /// Parameters stored as `[gain, ln(tau), ln(delay)]`.
    params: Vector3<f64>,
    floor: f64,
}

impl<'a> FopdtLmProblem<'a> {
    /// Maps the LM parameter vector back to a physical FOPDT model.
    fn model(&self) -> FopdtModel<f64> {
        FopdtModel {
            gain: self.params[0],
            time_constant: self.params[1].exp().max(self.floor),
            delay: self.params[2].exp().max(self.floor),
        }
    }

    /// Residual vector for the sampled delayed first-order step model.
    fn residual_vector(&self) -> OVector<f64, Dyn> {
        let model = self.model();
        let residuals = self
            .data
            .time_relative
            .iter()
            .zip(self.data.output.iter())
            .map(|(&t, &y)| {
                model.step_response_value(t, self.data.step_amplitude, self.data.initial_output) - y
            })
            .collect::<Vec<_>>();
        OVector::<f64, Dyn>::from_vec(residuals)
    }
}

impl<'a> LeastSquaresProblem<f64, Dyn, U3> for FopdtLmProblem<'a> {
    type ParameterStorage = Owned<f64, U3>;
    type ResidualStorage = VecStorage<f64, Dyn, U1>;
    type JacobianStorage = Owned<f64, Dyn, U3>;

    fn set_params(&mut self, x: &Vector3<f64>) {
        self.params = *x;
    }

    fn params(&self) -> Vector3<f64> {
        self.params
    }

    fn residuals(&self) -> Option<OVector<f64, Dyn>> {
        Some(self.residual_vector())
    }

    fn jacobian(&self) -> Option<OMatrix<f64, Dyn, U3>> {
        let raw_tau = self.params[1].exp();
        let raw_delay = self.params[2].exp();
        let d_tau_d_log_tau = if raw_tau > self.floor { raw_tau } else { 0.0 };
        let d_delay_d_log_delay = if raw_delay > self.floor {
            raw_delay
        } else {
            0.0
        };
        let model = self.model();
        let mut jacobian =
            OMatrix::<f64, Dyn, U3>::zeros_generic(Dyn(self.data.time_relative.len()), U3);
        for (row, &time_since_step) in self.data.time_relative.iter().enumerate() {
            let partials =
                model.step_response_jacobian_value(time_since_step, self.data.step_amplitude);
            jacobian[(row, 0)] = partials.gain;
            jacobian[(row, 1)] = partials.time_constant * d_tau_d_log_tau;
            jacobian[(row, 2)] = partials.delay * d_delay_d_log_delay;
        }
        Some(jacobian)
    }
}

#[derive(Clone)]
struct SopdtLmProblem<'a> {
    data: &'a PreparedStepResponse,
    /// Parameters stored as `[gain, ln(base_tau), ln(extra_tau), ln(delay)]`.
    params: Vector4<f64>,
    floor: f64,
}

impl<'a> SopdtLmProblem<'a> {
    /// Maps the LM parameter vector back to a physical SOPDT model.
    fn model(&self) -> SopdtModel<f64> {
        let base = self.params[1].exp().max(self.floor);
        let extra = self.params[2].exp().max(self.floor);
        SopdtModel {
            gain: self.params[0],
            time_constant_1: base + extra,
            time_constant_2: base,
            delay: self.params[3].exp().max(self.floor),
        }
    }

    /// Residual vector for the sampled delayed second-order step model.
    fn residual_vector(&self) -> OVector<f64, Dyn> {
        let model = self.model();
        let residuals = self
            .data
            .time_relative
            .iter()
            .zip(self.data.output.iter())
            .map(|(&t, &y)| {
                model.step_response_value(t, self.data.step_amplitude, self.data.initial_output) - y
            })
            .collect::<Vec<_>>();
        OVector::<f64, Dyn>::from_vec(residuals)
    }
}

impl<'a> LeastSquaresProblem<f64, Dyn, U4> for SopdtLmProblem<'a> {
    type ParameterStorage = Owned<f64, U4>;
    type ResidualStorage = VecStorage<f64, Dyn, U1>;
    type JacobianStorage = Owned<f64, Dyn, U4>;

    fn set_params(&mut self, x: &Vector4<f64>) {
        self.params = *x;
    }

    fn params(&self) -> Vector4<f64> {
        self.params
    }

    fn residuals(&self) -> Option<OVector<f64, Dyn>> {
        Some(self.residual_vector())
    }

    fn jacobian(&self) -> Option<OMatrix<f64, Dyn, U4>> {
        let raw_base = self.params[1].exp();
        let raw_extra = self.params[2].exp();
        let raw_delay = self.params[3].exp();
        let d_base_d_log_base = if raw_base > self.floor { raw_base } else { 0.0 };
        let d_extra_d_log_extra = if raw_extra > self.floor {
            raw_extra
        } else {
            0.0
        };
        let d_delay_d_log_delay = if raw_delay > self.floor {
            raw_delay
        } else {
            0.0
        };
        let model = self.model();
        let mut jacobian =
            OMatrix::<f64, Dyn, U4>::zeros_generic(Dyn(self.data.time_relative.len()), U4);
        for (row, &time_since_step) in self.data.time_relative.iter().enumerate() {
            let partials =
                model.step_response_jacobian_value(time_since_step, self.data.step_amplitude);
            jacobian[(row, 0)] = partials.gain;
            jacobian[(row, 1)] =
                (partials.time_constant_1 + partials.time_constant_2) * d_base_d_log_base;
            jacobian[(row, 2)] = partials.time_constant_1 * d_extra_d_log_extra;
            jacobian[(row, 3)] = partials.delay * d_delay_d_log_delay;
        }
        Some(jacobian)
    }
}

/// Finds the first crossing of `target` after `start_index` with the expected
/// response direction.
fn interpolate_crossing<R>(
    time: &[R],
    signal: &[R],
    target: R,
    direction: R,
    start_index: usize,
) -> Option<R>
where
    R: Float,
{
    if direction == R::zero() {
        return None;
    }
    for idx in start_index..signal.len().saturating_sub(1) {
        let lhs = signal[idx];
        let rhs = signal[idx + 1];
        let crossed = if direction > R::zero() {
            lhs <= target && rhs >= target
        } else {
            lhs >= target && rhs <= target
        };
        if crossed {
            let dy = rhs - lhs;
            if dy == R::zero() {
                return Some(time[idx]);
            }
            let alpha = (target - lhs) / dy;
            return Some(time[idx] + alpha * (time[idx + 1] - time[idx]));
        }
    }
    None
}

/// Returns the first index whose shifted time is nonnegative.
fn first_nonnegative_index(values: &[f64]) -> usize {
    values.iter().position(|&value| value >= 0.0).unwrap_or(0)
}

/// Finds the dominant single input jump in the sampled command record.
///
/// The current fitting path assumes one main step experiment, so the largest
/// adjacent jump is treated as the step used for identification.
fn dominant_step_index(signal: &[f64]) -> Option<usize> {
    let mut best_index = None;
    let mut best_jump = 0.0f64;
    for idx in 0..signal.len().saturating_sub(1) {
        let jump = (signal[idx + 1] - signal[idx]).abs();
        if jump > best_jump {
            best_jump = jump;
            best_index = Some(idx);
        }
    }
    if best_jump > 1.0e-12 {
        best_index
    } else {
        None
    }
}

/// Interpolates the detected input step to a sub-sample transition time.
fn interpolate_input_step_time(
    time: &[f64],
    input: &[f64],
    step_index: usize,
) -> Result<f64, PidDesignError> {
    let lhs_u = input[step_index];
    let rhs_u = input[step_index + 1];
    let lhs_t = time[step_index];
    let rhs_t = time[step_index + 1];
    let du = rhs_u - lhs_u;
    if du == 0.0 {
        return Err(PidDesignError::InvalidData {
            which: "input_step_time",
        });
    }
    let target = lhs_u + 0.5 * du;
    let alpha = (target - lhs_u) / du;
    Ok(lhs_t + alpha * (rhs_t - lhs_t))
}

/// Large residual vector returned when step-optimization tuning hits an invalid design.
fn step_optimization_penalty_vector(horizon_steps: usize) -> OVector<f64, Dyn> {
    OVector::<f64, Dyn>::from_vec(vec![1.0e6; horizon_steps])
}

/// Heuristic tail length used for steady-state averaging.
fn tail_count(n: usize) -> usize {
    (n / 10).max(3).min(n.max(1))
}

/// Arithmetic mean helper for baseline and tail estimates.
fn mean(values: &[f64]) -> f64 {
    values.iter().sum::<f64>() / values.len() as f64
}

/// Positive floor derived from the experiment time scale.
///
/// This prevents zero or negative lag/delay values in the transformed LM
/// parameters while staying small relative to the sampled experiment.
fn positive_floor(scale: f64) -> f64 {
    (1.0e-9 * scale).max(1.0e-9)
}

/// Rejects zero or nonfinite process gains before applying a tuning rule.
fn validate_process_gain<R>(gain: R) -> Result<(), PidDesignError>
where
    R: Float,
{
    if gain.is_finite() && gain != R::zero() {
        Ok(())
    } else {
        Err(PidDesignError::InvalidProcessFit)
    }
}

/// Validates a strictly positive tuning or process parameter.
fn validate_positive<R>(value: R, which: &'static str) -> Result<(), PidDesignError>
where
    R: Float,
{
    if value.is_finite() && value > R::zero() {
        Ok(())
    } else {
        Err(PidDesignError::InvalidTuningParameter { which })
    }
}

/// Validates a nonnegative tuning or process parameter.
fn validate_nonnegative<R>(value: R, which: &'static str) -> Result<(), PidDesignError>
where
    R: Float,
{
    if value.is_finite() && value >= R::zero() {
        Ok(())
    } else {
        Err(PidDesignError::InvalidTuningParameter { which })
    }
}

#[cfg(test)]
mod tests {
    use super::{
        AntiWindup, FopdtModel, FrequencyPidParams, PidControllerKind, PidDesignError,
        SampledIoData, SimcPidParams, SopdtModel, StepOptimizationPidParams, StepResponseData,
        design_pid_from_continuous_state_space_frequency, design_pid_from_continuous_tf_frequency,
        design_pid_from_discrete_tf_step_optimization, design_pid_from_fopdt,
        design_pid_from_okid_era, design_pid_from_sopdt, design_pid_from_step_response_fopdt,
        fit_fopdt_from_step_response, fit_sopdt_from_step_response,
    };
    use crate::control::identification::{EraParams, OkidParams};
    use crate::control::lti::{ContinuousTransferFunction, DiscreteTransferFunction};
    use alloc::vec::Vec;
    use faer::Mat;
    use nalgebra::ComplexField;

    fn assert_close(lhs: f64, rhs: f64, tol: f64) {
        let err = (lhs - rhs).abs();
        assert!(err <= tol, "lhs={lhs}, rhs={rhs}, err={err}, tol={tol}");
    }

    fn fopdt_step_data(model: FopdtModel<f64>, dt: f64, duration: f64) -> StepResponseData<f64> {
        let n = (duration / dt).round() as usize + 1;
        let step_time = 5.0 * dt;
        let mut time = Vec::with_capacity(n);
        let mut input = Vec::with_capacity(n);
        let mut output = Vec::with_capacity(n);
        for i in 0..n {
            let t = i as f64 * dt;
            time.push(t);
            let u = if t >= step_time { 1.0 } else { 0.0 };
            input.push(u);
            output.push(model.step_response_value((t - step_time).max(0.0), 1.0, 0.0));
        }
        StepResponseData::new(time, input, output).unwrap()
    }

    fn sopdt_step_data(model: SopdtModel<f64>, dt: f64, duration: f64) -> StepResponseData<f64> {
        let n = (duration / dt).round() as usize + 1;
        let step_time = 5.0 * dt;
        let mut time = Vec::with_capacity(n);
        let mut input = Vec::with_capacity(n);
        let mut output = Vec::with_capacity(n);
        for i in 0..n {
            let t = i as f64 * dt;
            time.push(t);
            let u = if t >= step_time { 1.0 } else { 0.0 };
            input.push(u);
            output.push(model.step_response_value((t - step_time).max(0.0), 1.0, 0.0));
        }
        StepResponseData::new(time, input, output).unwrap()
    }

    #[test]
    fn step_response_data_rejects_invalid_inputs() {
        let err =
            StepResponseData::new(vec![0.0, 0.0], vec![0.0, 1.0], vec![0.0, 1.0]).unwrap_err();
        assert!(matches!(err, PidDesignError::InvalidData { .. }));
    }

    #[test]
    fn fit_fopdt_recovers_synthetic_step_response() {
        let model = FopdtModel {
            gain: 2.0,
            time_constant: 3.0,
            delay: 0.7,
        };
        let data = fopdt_step_data(model, 0.05, 20.0);
        let fit = fit_fopdt_from_step_response(&data).unwrap();

        assert_close(fit.model.gain, model.gain, 1.0e-2);
        assert_close(fit.model.time_constant, model.time_constant, 5.0e-2);
        assert_close(fit.model.delay, model.delay, 5.0e-2);
        assert!(fit.objective <= 1.0e-6);
    }

    #[test]
    fn fit_sopdt_recovers_synthetic_step_response() {
        let model = SopdtModel {
            gain: 1.5,
            time_constant_1: 2.5,
            time_constant_2: 0.8,
            delay: 0.4,
        };
        let data = sopdt_step_data(model, 0.05, 20.0);
        let fit = fit_sopdt_from_step_response(&data).unwrap();

        assert_close(fit.model.gain, model.gain, 1.0e-2);
        assert_close(fit.model.time_constant_1, model.time_constant_1, 7.5e-2);
        assert_close(fit.model.time_constant_2, model.time_constant_2, 7.5e-2);
        assert_close(fit.model.delay, model.delay, 5.0e-2);
        assert!(fit.objective <= 1.0e-6);
    }

    #[test]
    fn simc_fopdt_pi_matches_expected_formula() {
        let model = FopdtModel {
            gain: 2.0,
            time_constant: 4.0,
            delay: 1.0,
        };
        let params =
            SimcPidParams::new(2.0, PidControllerKind::Pi, 10.0, AntiWindup::None).unwrap();
        let design = design_pid_from_fopdt(model, params).unwrap();
        assert_close(design.pid.kp(), 4.0 / (2.0 * (2.0 + 1.0)), 1.0e-12);
        assert_close(design.pid.ki(), design.pid.kp() / 4.0, 1.0e-12);
        assert_close(design.pid.kd(), 0.0, 1.0e-12);
    }

    #[test]
    fn simc_sopdt_pid_populates_derivative_filter() {
        let model = SopdtModel {
            gain: 1.0,
            time_constant_1: 3.0,
            time_constant_2: 1.0,
            delay: 0.5,
        };
        let params =
            SimcPidParams::new(1.5, PidControllerKind::Pid, 8.0, AntiWindup::None).unwrap();
        let design = design_pid_from_sopdt(model, params).unwrap();
        assert!(design.pid.kd() > 0.0);
        assert!(design.pid.derivative_filter().unwrap() > 0.0);
    }

    #[test]
    fn step_fit_design_builds_pid_from_fopdt_fit() {
        let model = FopdtModel {
            gain: 1.2,
            time_constant: 2.0,
            delay: 0.3,
        };
        let data = fopdt_step_data(model, 0.05, 12.0);
        let params =
            SimcPidParams::new(1.0, PidControllerKind::Pid, 10.0, AntiWindup::None).unwrap();
        let design = design_pid_from_step_response_fopdt(&data, params).unwrap();
        assert_close(design.fit.model.gain, model.gain, 1.0e-3);
        assert!(design.design.pid.kp().is_finite());
    }

    #[test]
    fn fit_rejects_zero_step_amplitude() {
        let data = StepResponseData::new(
            vec![0.0, 1.0, 2.0, 3.0],
            vec![1.0, 1.0, 1.0, 1.0],
            vec![0.0, 0.0, 0.0, 0.0],
        )
        .unwrap();
        let err = fit_fopdt_from_step_response(&data).unwrap_err();
        assert!(matches!(err, PidDesignError::InvalidStepAmplitude));
    }

    #[test]
    fn continuous_frequency_design_hits_target() {
        let plant = ContinuousTransferFunction::continuous(vec![1.0], vec![1.0, 1.0]).unwrap();
        let params = FrequencyPidParams::new(
            1.0,
            60.0,
            PidControllerKind::Pi,
            10.0,
            8.0,
            AntiWindup::None,
        )
        .unwrap();
        let design = design_pid_from_continuous_tf_frequency(&plant, params).unwrap();
        assert_close(design.loop_value.abs(), 1.0, 1.0e-8);
        assert_close(design.achieved_phase_margin_deg, 60.0, 1.0e-6);

        let plant_ss = plant.to_state_space().unwrap();
        let from_ss = design_pid_from_continuous_state_space_frequency(&plant_ss, params).unwrap();
        assert_close(from_ss.pid.kp(), design.pid.kp(), 1.0e-8);
        assert_close(from_ss.pid.ki(), design.pid.ki(), 1.0e-8);
    }

    #[test]
    fn discrete_step_optimization_returns_stabilizing_controller() {
        let plant = DiscreteTransferFunction::discrete(vec![0.2], vec![1.0, -0.8], 0.1).unwrap();
        let params = StepOptimizationPidParams::new(
            0.5,
            20,
            PidControllerKind::Pi,
            10.0,
            8.0,
            AntiWindup::None,
        )
        .unwrap();

        let design = design_pid_from_discrete_tf_step_optimization(&plant, params).unwrap();
        assert!(design.tracking_cost.is_finite());
        assert!(design.control_cost.is_finite());

        let controller_tf = design
            .pid
            .to_transfer_function_discrete(plant.sample_time())
            .unwrap();
        let closed_loop = controller_tf
            .mul(&plant)
            .unwrap()
            .unity_feedback()
            .unwrap()
            .to_state_space()
            .unwrap();
        assert!(closed_loop.is_asymptotically_stable().unwrap());
    }

    #[test]
    fn okid_era_pid_design_builds_controller_from_sampled_data() {
        let plant = DiscreteTransferFunction::discrete(vec![0.15], vec![1.0, -0.85], 0.1).unwrap();
        let plant_ss = plant.to_state_space().unwrap();
        let nsamples = 80usize;
        let input = (0..nsamples)
            .map(|k| if (k / 4) % 2 == 0 { 1.0 } else { -0.5 })
            .collect::<Vec<_>>();
        let inputs = Mat::from_fn(1, nsamples, |_, col| input[col]);
        let sim = plant_ss.simulate(&[0.0], inputs.as_ref()).unwrap();
        let output = (0..nsamples)
            .map(|col| sim.outputs[(0, col)])
            .collect::<Vec<_>>();

        let data = SampledIoData::new(0.1, input, output).unwrap();
        let okid_params = OkidParams::new(24, 8);
        let era_params = EraParams::new(0.1).with_order(1);
        let pid_params = StepOptimizationPidParams::new(
            0.6,
            25,
            PidControllerKind::Pi,
            10.0,
            8.0,
            AntiWindup::None,
        )
        .unwrap();

        let design = design_pid_from_okid_era(&data, &okid_params, era_params, pid_params).unwrap();
        assert_eq!(design.identified_plant.sample_time(), 0.1);
        assert_eq!(design.retained_order, 1);
        assert!(design.design.pid.kp().is_finite());
        assert!(design.design.pid.ki().is_finite());

        let original_dc = plant_ss.dc_gain().unwrap()[(0, 0)].re;
        let identified_dc = design.identified_plant.dc_gain().unwrap()[(0, 0)].re;
        assert!((identified_dc - original_dc).abs() <= 0.1);
    }
}
