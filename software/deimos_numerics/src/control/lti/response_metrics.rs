//! Sampled time-domain response metrics.
//!
//! The implementation is intentionally grid-based. Metrics are extracted
//! from sampled step responses rather than exact symbolic solves, so the
//! caller's sampling grid determines the final accuracy.
//!
//! # Two Intuitions
//!
//! 1. **Control-summary view.** This module compresses a whole step-response
//!    trace down to the small set of numbers engineers usually compare first.
//! 2. **Threshold-crossing view.** The same metrics can be understood as a set
//!    of geometric events on a sampled curve: when did it first cross 10%,
//!    peak, and stay inside the settling band?
//!
//! # Glossary
//!
//! - **Rise time:** Time between two configured fractions of the final change.
//! - **Settling time:** Last time after which the response stays in a tolerance
//!   band.
//! - **Overshoot:** Peak excursion above the final value, normalized by the
//!   total step excursion.
//!
//! # Mathematical Formulation
//!
//! Given sampled step-response values `y[k]`, the module estimates:
//!
//! - rise time from lower/upper fraction crossings
//! - settling time from the final sustained entrance into a relative band
//! - overshoot and undershoot from direction-aware extrema
//! - steady-state error from the final sampled value
//!
//! # Implementation Notes
//!
//! - Metrics are sampled-grid estimates, not symbolic properties of the exact
//!   underlying system.
//! - The implementation is intentionally SISO-focused because these metrics are
//!   ambiguous without a channel-selection policy in MIMO problems.

use super::{
    ContinuousSos, ContinuousStateSpace, ContinuousTransferFunction, ContinuousZpk, DiscreteSos,
    DiscreteStateSpace, DiscreteTransferFunction, DiscreteZpk, LtiError,
};
use crate::scalar::mul_add;
use crate::sparse::compensated::CompensatedField;
use alloc::vec::Vec;
use faer_traits::RealField;
use faer_traits::math_utils::from_f64;
use num_traits::Float;

/// Parameters controlling sampled step-response metric extraction.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StepResponseMetricParams<R> {
    /// Lower fraction of the total step excursion used for rise time.
    pub rise_lower_frac: R,
    /// Upper fraction of the total step excursion used for rise time.
    pub rise_upper_frac: R,
    /// Relative settling band around the final value.
    pub settling_band_frac: R,
}

impl<R> Default for StepResponseMetricParams<R>
where
    R: Float + RealField,
{
    fn default() -> Self {
        // Match the usual "stepinfo"-style defaults: 10-90% rise time and a
        // 2% settling band.
        Self {
            rise_lower_frac: from_f64::<R>(0.1),
            rise_upper_frac: from_f64::<R>(0.9),
            settling_band_frac: from_f64::<R>(0.02),
        }
    }
}

/// Summary metrics extracted from a sampled unit-step response trace.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StepResponseMetrics<R> {
    /// Initial response value.
    pub initial_value: R,
    /// Final sampled response value.
    pub steady_state_value: R,
    /// Direction-aware peak value.
    pub peak_value: R,
    /// Time at which the direction-aware peak occurs.
    pub peak_time: R,
    /// Overshoot relative to the total sampled excursion, in percent.
    pub overshoot_percent: R,
    /// Opposite-direction excursion relative to the total sampled excursion,
    /// in percent.
    pub undershoot_percent: R,
    /// Rise time between the configured lower and upper fractions.
    pub rise_time: Option<R>,
    /// Settling time inside the configured relative band.
    pub settling_time: Option<R>,
    /// Unit-step steady-state error `1 - y_inf`.
    pub steady_state_error: R,
}

impl<R> ContinuousTransferFunction<R>
where
    R: CompensatedField<Real = R> + Float + RealField,
{
    /// Extracts sampled step-response metrics on the supplied time grid.
    pub fn step_metrics(
        &self,
        sample_times: &[R],
        params: &StepResponseMetricParams<R>,
    ) -> Result<StepResponseMetrics<R>, LtiError> {
        self.to_state_space()?.step_metrics(sample_times, params)
    }
}

impl<R> DiscreteTransferFunction<R>
where
    R: CompensatedField<Real = R> + Float + RealField,
{
    /// Extracts sampled step-response metrics from the first `n_steps`
    /// discrete-time samples.
    pub fn step_metrics(
        &self,
        n_steps: usize,
        params: &StepResponseMetricParams<R>,
    ) -> Result<StepResponseMetrics<R>, LtiError> {
        self.to_state_space()?.step_metrics(n_steps, params)
    }
}

impl<R> ContinuousZpk<R>
where
    R: CompensatedField<Real = R> + Float + RealField,
{
    /// Extracts sampled step-response metrics on the supplied time grid.
    pub fn step_metrics(
        &self,
        sample_times: &[R],
        params: &StepResponseMetricParams<R>,
    ) -> Result<StepResponseMetrics<R>, LtiError> {
        self.to_state_space()?.step_metrics(sample_times, params)
    }
}

impl<R> DiscreteZpk<R>
where
    R: CompensatedField<Real = R> + Float + RealField,
{
    /// Extracts sampled step-response metrics from the first `n_steps`
    /// discrete-time samples.
    pub fn step_metrics(
        &self,
        n_steps: usize,
        params: &StepResponseMetricParams<R>,
    ) -> Result<StepResponseMetrics<R>, LtiError> {
        self.to_state_space()?.step_metrics(n_steps, params)
    }
}

impl<R> ContinuousSos<R>
where
    R: CompensatedField<Real = R> + Float + RealField,
{
    /// Extracts sampled step-response metrics on the supplied time grid.
    pub fn step_metrics(
        &self,
        sample_times: &[R],
        params: &StepResponseMetricParams<R>,
    ) -> Result<StepResponseMetrics<R>, LtiError> {
        self.to_state_space()?.step_metrics(sample_times, params)
    }
}

impl<R> DiscreteSos<R>
where
    R: CompensatedField<Real = R> + Float + RealField,
{
    /// Extracts sampled step-response metrics from the first `n_steps`
    /// discrete-time samples.
    pub fn step_metrics(
        &self,
        n_steps: usize,
        params: &StepResponseMetricParams<R>,
    ) -> Result<StepResponseMetrics<R>, LtiError> {
        self.to_state_space()?.step_metrics(n_steps, params)
    }
}

impl<R> ContinuousStateSpace<R>
where
    R: CompensatedField<Real = R> + Float + RealField,
{
    /// Extracts sampled step-response metrics on the supplied time grid.
    pub fn step_metrics(
        &self,
        sample_times: &[R],
        params: &StepResponseMetricParams<R>,
    ) -> Result<StepResponseMetrics<R>, LtiError> {
        ensure_siso_state_space(self)?;
        let response = self.step_response(sample_times)?;
        let values = response
            .values
            .iter()
            .map(|block| block[(0, 0)])
            .collect::<Vec<_>>();
        step_metrics_from_samples(sample_times, &values, params, "step_metrics")
    }
}

impl<R> DiscreteStateSpace<R>
where
    R: CompensatedField<Real = R> + Float + RealField,
{
    /// Extracts sampled step-response metrics from the first `n_steps`
    /// discrete-time samples.
    pub fn step_metrics(
        &self,
        n_steps: usize,
        params: &StepResponseMetricParams<R>,
    ) -> Result<StepResponseMetrics<R>, LtiError> {
        ensure_siso_state_space(self)?;
        let response = self.step_response(n_steps);
        let times = (0..n_steps)
            .map(|idx| from_f64::<R>(idx as f64) * self.sample_time())
            .collect::<Vec<_>>();
        let values = response
            .values
            .iter()
            .map(|block| block[(0, 0)])
            .collect::<Vec<_>>();
        step_metrics_from_samples(&times, &values, params, "step_metrics")
    }
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

fn step_metrics_from_samples<R>(
    sample_times: &[R],
    values: &[R],
    params: &StepResponseMetricParams<R>,
    which: &'static str,
) -> Result<StepResponseMetrics<R>, LtiError>
where
    R: Float + RealField,
{
    validate_metric_inputs(sample_times, values, params, which)?;

    let initial = values[0];
    let steady = *values.last().unwrap();
    let delta = steady - initial;
    let excursion = delta.abs();
    let tol = from_f64::<R>(128.0) * R::epsilon() * excursion.max(R::one());

    // Interpret "peak" relative to the sign of the net excursion so the same
    // logic works for upward and downward unit-step responses.
    let (peak_value, peak_time) = direction_peak(sample_times, values, delta);
    let overshoot_percent = if excursion <= tol {
        R::zero()
    } else if delta >= R::zero() {
        ((peak_value - steady).max(R::zero()) / excursion) * from_f64::<R>(100.0)
    } else {
        ((steady - peak_value).max(R::zero()) / excursion) * from_f64::<R>(100.0)
    };

    let undershoot_percent = if excursion <= tol {
        R::zero()
    } else {
        let opposite = opposite_extremum(values, delta);
        if delta >= R::zero() {
            ((initial - opposite).max(R::zero()) / excursion) * from_f64::<R>(100.0)
        } else {
            ((opposite - initial).max(R::zero()) / excursion) * from_f64::<R>(100.0)
        }
    };

    let rise_time = if excursion <= tol {
        None
    } else {
        let low_target = initial + params.rise_lower_frac * delta;
        let high_target = initial + params.rise_upper_frac * delta;
        let low_cross = first_crossing_time(sample_times, values, low_target, delta >= R::zero());
        let high_cross = first_crossing_time(sample_times, values, high_target, delta >= R::zero());
        match (low_cross, high_cross) {
            (Some(low), Some(high)) => Some(high - low),
            _ => None,
        }
    };

    let settling_time = if excursion <= tol {
        Some(sample_times[0])
    } else {
        let band = params.settling_band_frac.abs() * excursion;
        settling_time(sample_times, values, steady, band)
    };

    Ok(StepResponseMetrics {
        initial_value: initial,
        steady_state_value: steady,
        peak_value,
        peak_time,
        overshoot_percent,
        undershoot_percent,
        rise_time,
        settling_time,
        steady_state_error: R::one() - steady,
    })
}

fn validate_metric_inputs<R>(
    sample_times: &[R],
    values: &[R],
    params: &StepResponseMetricParams<R>,
    which: &'static str,
) -> Result<(), LtiError>
where
    R: Float + RealField,
{
    if sample_times.is_empty() || sample_times.len() != values.len() {
        return Err(LtiError::InvalidSampleGrid { which });
    }
    if sample_times
        .windows(2)
        .any(|window| !window[0].is_finite() || !window[1].is_finite() || window[1] < window[0])
    {
        return Err(LtiError::InvalidSampleGrid { which });
    }
    if sample_times[0] < R::zero()
        || sample_times.iter().any(|&time| !time.is_finite())
        || values.iter().any(|&value| !value.is_finite())
    {
        return Err(LtiError::InvalidSamplePoint { which });
    }
    if !params.rise_lower_frac.is_finite()
        || !params.rise_upper_frac.is_finite()
        || !params.settling_band_frac.is_finite()
        || params.rise_lower_frac < R::zero()
        || params.rise_upper_frac < params.rise_lower_frac
        || params.rise_upper_frac > R::one()
        || params.settling_band_frac < R::zero()
    {
        return Err(LtiError::InvalidSampleGrid { which });
    }
    Ok(())
}

fn direction_peak<R>(sample_times: &[R], values: &[R], delta: R) -> (R, R)
where
    R: Float + RealField,
{
    let mut best_idx = 0usize;
    for idx in 1..values.len() {
        let better = if delta >= R::zero() {
            values[idx] > values[best_idx]
        } else {
            values[idx] < values[best_idx]
        };
        if better {
            best_idx = idx;
        }
    }
    (values[best_idx], sample_times[best_idx])
}

fn opposite_extremum<R>(values: &[R], delta: R) -> R
where
    R: Float + RealField,
{
    let mut best = values[0];
    for &value in &values[1..] {
        let better = if delta >= R::zero() {
            value < best
        } else {
            value > best
        };
        if better {
            best = value;
        }
    }
    best
}

fn first_crossing_time<R>(sample_times: &[R], values: &[R], target: R, rising: bool) -> Option<R>
where
    R: Float + RealField,
{
    for idx in 0..values.len().saturating_sub(1) {
        let y0 = values[idx];
        let y1 = values[idx + 1];
        if approx_reached(y0, target) {
            return Some(sample_times[idx]);
        }
        let crossed = if rising {
            y0 < target && y1 >= target
        } else {
            y0 > target && y1 <= target
        };
        if crossed {
            // Keep the implementation cheap: linear interpolation on the sampled
            // segment is enough for these grid-based metrics.
            return Some(interpolate_time(
                sample_times[idx],
                sample_times[idx + 1],
                y0,
                y1,
                target,
            ));
        }
    }
    values
        .last()
        .copied()
        .filter(|&value| approx_reached(value, target))
        .map(|_| *sample_times.last().unwrap())
}

fn settling_time<R>(sample_times: &[R], values: &[R], steady: R, band: R) -> Option<R>
where
    R: Float + RealField,
{
    // Settling time is the first sample after the last band violation. This is
    // deliberately sample-grid based rather than an exact continuous solve.
    let last_outside = values
        .iter()
        .enumerate()
        .rev()
        .find(|&(_, &value)| (value - steady).abs() > band)
        .map(|(idx, _)| idx);

    match last_outside {
        None => Some(sample_times[0]),
        Some(idx) if idx + 1 < sample_times.len() => Some(sample_times[idx + 1]),
        Some(_) => None,
    }
}

fn interpolate_time<R>(t0: R, t1: R, y0: R, y1: R, target: R) -> R
where
    R: Float + RealField,
{
    if approx_reached(y0, y1) {
        return t0;
    }
    let alpha = (target - y0) / (y1 - y0);
    mul_add(alpha, t1 - t0, t0)
}

fn approx_reached<R>(lhs: R, rhs: R) -> bool
where
    R: Float + RealField,
{
    // Scale the comparison so near-zero and large-magnitude traces both behave
    // reasonably under the same metric-extraction code.
    let scale = lhs.abs().max(rhs.abs()).max(R::one());
    (lhs - rhs).abs() <= from_f64::<R>(128.0) * R::epsilon() * scale
}

#[cfg(test)]
mod tests {
    use super::{StepResponseMetricParams, StepResponseMetrics};
    use crate::control::lti::{ContinuousTransferFunction, DiscreteTransferFunction};
    use alloc::vec::Vec;

    fn assert_close(lhs: f64, rhs: f64, tol: f64) {
        let err = (lhs - rhs).abs();
        assert!(err <= tol, "lhs={lhs}, rhs={rhs}, err={err}, tol={tol}");
    }

    fn assert_metrics_close(
        lhs: StepResponseMetrics<f64>,
        rhs: StepResponseMetrics<f64>,
        tol: f64,
    ) {
        assert_close(lhs.initial_value, rhs.initial_value, tol);
        assert_close(lhs.steady_state_value, rhs.steady_state_value, tol);
        assert_close(lhs.peak_value, rhs.peak_value, tol);
        assert_close(lhs.peak_time, rhs.peak_time, tol);
        assert_close(lhs.overshoot_percent, rhs.overshoot_percent, tol);
        assert_close(lhs.undershoot_percent, rhs.undershoot_percent, tol);
        assert_close(lhs.steady_state_error, rhs.steady_state_error, tol);
        match (lhs.rise_time, rhs.rise_time) {
            (Some(lhs), Some(rhs)) => assert_close(lhs, rhs, tol),
            (None, None) => {}
            _ => panic!("rise_time mismatch"),
        }
        match (lhs.settling_time, rhs.settling_time) {
            (Some(lhs), Some(rhs)) => assert_close(lhs, rhs, tol),
            (None, None) => {}
            _ => panic!("settling_time mismatch"),
        }
    }

    #[test]
    fn continuous_first_order_metrics_match_closed_form() {
        let tf = ContinuousTransferFunction::continuous(vec![1.0], vec![1.0, 1.0]).unwrap();
        let params = StepResponseMetricParams::default();
        let sample_times = (0..2001)
            .map(|idx| 10.0 * idx as f64 / 2000.0)
            .collect::<Vec<_>>();
        let metrics = tf.step_metrics(&sample_times, &params).unwrap();

        assert_close(metrics.initial_value, 0.0, 1.0e-12);
        assert_close(metrics.steady_state_value, 1.0 - (-10.0f64).exp(), 1.0e-12);
        assert_close(metrics.overshoot_percent, 0.0, 1.0e-12);
        assert_close(metrics.undershoot_percent, 0.0, 1.0e-12);
        assert_close(
            metrics.rise_time.unwrap(),
            (10.0f64).ln() - (10.0f64 / 9.0).ln(),
            5.0e-3,
        );
        assert!(metrics.settling_time.unwrap() >= 3.8 && metrics.settling_time.unwrap() <= 4.1);
    }

    #[test]
    fn discrete_first_order_metrics_are_sampled_consistently() {
        let tf = DiscreteTransferFunction::discrete(vec![0.2], vec![1.0, -0.8], 0.5).unwrap();
        let params = StepResponseMetricParams::default();
        let metrics = tf.step_metrics(40, &params).unwrap();

        assert_close(metrics.initial_value, 0.0, 1.0e-12);
        assert!(metrics.steady_state_value > 0.99);
        assert_close(metrics.overshoot_percent, 0.0, 1.0e-12);
        assert!(metrics.rise_time.unwrap() > 4.0);
        assert!(metrics.settling_time.unwrap() >= metrics.rise_time.unwrap());
    }

    #[test]
    fn metrics_match_across_siso_representations() {
        let tf = ContinuousTransferFunction::continuous(vec![2.0], vec![1.0, 3.0, 2.0]).unwrap();
        let zpk = tf.to_zpk().unwrap();
        let sos = tf.to_sos().unwrap();
        let ss = tf.to_state_space().unwrap();
        let params = StepResponseMetricParams::default();
        let sample_times = (0..1601)
            .map(|idx| 8.0 * idx as f64 / 1600.0)
            .collect::<Vec<_>>();

        let tf_metrics = tf.step_metrics(&sample_times, &params).unwrap();
        let zpk_metrics = zpk.step_metrics(&sample_times, &params).unwrap();
        let sos_metrics = sos.step_metrics(&sample_times, &params).unwrap();
        let ss_metrics = ss.step_metrics(&sample_times, &params).unwrap();

        assert_metrics_close(tf_metrics, zpk_metrics, 1.0e-8);
        assert_metrics_close(tf_metrics, sos_metrics, 1.0e-8);
        assert_metrics_close(tf_metrics, ss_metrics, 1.0e-8);
    }
}
