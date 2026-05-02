//! Fixed-size discrete-time linear Kalman filtering.
//!
//! # Glossary
//!
//! - **Innovation:** Measurement residual between the actual and predicted
//!   outputs.
//! - **Process-noise covariance:** Covariance assigned to state evolution
//!   uncertainty.
//! - **Measurement-noise covariance:** Covariance assigned to sensor
//!   uncertainty.
//! - **Steady-state gain:** Fixed Kalman correction gain reused at every
//!   sample once the Riccati recursion has converged.

use crate::embedded::error::EmbeddedError;
use crate::embedded::fixed::linalg::{
    Matrix, Vector, identity_matrix, mat_add, mat_mul, mat_sub, mat_vec_mul, solve_linear_system,
    transpose, vec_add, vec_norm, vec_sub,
};
use crate::embedded::fixed::lti::DiscreteStateSpace;
use num_traits::Float;

/// Covariance update policy used by [`DiscreteKalmanFilter`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum CovarianceUpdate {
    /// `P^+ = P^- - K S K^T`
    Simple,
    /// `P^+ = (I - K C) P^- (I - K C)^T + K V K^T`
    #[default]
    Joseph,
}

/// One non-mutating prediction stage.
///
/// Each field is a prediction-stage quantity derived from `x[k|k]`,
/// `P[k|k]`, and the supplied input at sample `k`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct KalmanPrediction<T, const NX: usize, const NY: usize> {
    /// Predicted state estimate.
    pub state: Vector<T, NX>,
    /// Predicted covariance.
    pub covariance: Matrix<T, NX, NX>,
    /// Predicted output.
    pub output: Vector<T, NY>,
}

/// One non-mutating measurement update stage.
///
/// Each field is an update-stage quantity derived from a prediction, the
/// supplied measurement, and the configured noise covariances.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct KalmanUpdate<T, const NX: usize, const NY: usize> {
    /// Innovation `y - (C x^- + D u)`.
    pub innovation: Vector<T, NY>,
    /// Euclidean innovation norm.
    pub innovation_norm: T,
    /// Innovation covariance.
    pub innovation_covariance: Matrix<T, NY, NY>,
    /// Normalized innovation norm.
    pub normalized_innovation_norm: T,
    /// Kalman gain.
    pub gain: Matrix<T, NX, NY>,
    /// Measurement-side predicted output.
    pub predicted_output: Vector<T, NY>,
    /// Posterior state estimate.
    pub state: Vector<T, NX>,
    /// Posterior covariance.
    pub covariance: Matrix<T, NX, NX>,
    /// Posterior output.
    pub output: Vector<T, NY>,
}

/// Fixed-size recursive discrete-time Kalman filter.
///
/// The filter assumes process noise is already expressed in state coordinates,
/// so the covariance prediction is `P^- = A P A^T + W`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DiscreteKalmanFilter<T, const NX: usize, const NU: usize, const NY: usize> {
    /// Discrete-time plant model.
    pub system: DiscreteStateSpace<T, NX, NU, NY>,
    /// Process-noise covariance.
    pub w: Matrix<T, NX, NX>,
    /// Measurement-noise covariance.
    pub v: Matrix<T, NY, NY>,
    /// Covariance update policy.
    pub covariance_update: CovarianceUpdate,
    /// Current posterior state estimate.
    pub x_hat: Vector<T, NX>,
    /// Current posterior covariance.
    pub p: Matrix<T, NX, NX>,
}

/// Fixed-gain steady-state discrete-time observer.
///
/// This is the deployment form used after a steady-state Kalman gain has
/// already been designed offline.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SteadyStateKalmanFilter<T, const NX: usize, const NU: usize, const NY: usize> {
    /// Discrete-time plant model.
    pub system: DiscreteStateSpace<T, NX, NU, NY>,
    /// Fixed observer gain.
    pub gain: Matrix<T, NX, NY>,
    /// Current state estimate.
    pub x_hat: Vector<T, NX>,
    /// Optional steady-state covariance.
    pub steady_state_covariance: Option<Matrix<T, NX, NX>>,
}

impl<T, const NX: usize, const NU: usize, const NY: usize> DiscreteKalmanFilter<T, NX, NU, NY>
where
    T: Float,
{
    /// Creates a fixed-size discrete Kalman filter.
    ///
    /// Args:
    ///   system: Discrete-time plant realization with `NX` states, `NU`
    ///     inputs, `NY` outputs, and a stored sample interval.
    ///   w: Process-noise covariance with shape `(NX, NX)`.
    ///   v: Measurement-noise covariance with shape `(NY, NY)`.
    ///   x_hat: Initial posterior state estimate with shape `(NX,)`.
    ///   p: Initial posterior covariance with shape `(NX, NX)`.
    ///
    /// Returns:
    ///   A recursive fixed-size Kalman filter initialized at the supplied
    ///   posterior estimate.
    pub fn new(
        system: DiscreteStateSpace<T, NX, NU, NY>,
        w: Matrix<T, NX, NX>,
        v: Matrix<T, NY, NY>,
        x_hat: Vector<T, NX>,
        p: Matrix<T, NX, NX>,
    ) -> Result<Self, EmbeddedError> {
        Self::new_with_covariance_update(system, w, v, x_hat, p, CovarianceUpdate::default())
    }

    /// Creates a fixed-size discrete Kalman filter with an explicit covariance
    /// update policy.
    ///
    /// Args:
    ///   system: Discrete-time plant realization with `NX` states, `NU`
    ///     inputs, `NY` outputs, and a stored sample interval.
    ///   w: Process-noise covariance with shape `(NX, NX)`.
    ///   v: Measurement-noise covariance with shape `(NY, NY)`.
    ///   x_hat: Initial posterior state estimate with shape `(NX,)`.
    ///   p: Initial posterior covariance with shape `(NX, NX)`.
    ///   covariance_update: Covariance update law used after each
    ///     measurement correction.
    ///
    /// Returns:
    ///   A recursive fixed-size Kalman filter.
    pub fn new_with_covariance_update(
        system: DiscreteStateSpace<T, NX, NU, NY>,
        w: Matrix<T, NX, NX>,
        v: Matrix<T, NY, NY>,
        x_hat: Vector<T, NX>,
        p: Matrix<T, NX, NX>,
        covariance_update: CovarianceUpdate,
    ) -> Result<Self, EmbeddedError> {
        Ok(Self {
            system,
            w,
            v,
            covariance_update,
            x_hat,
            p,
        })
    }

    /// Returns the current posterior state estimate with shape `(NX,)`.
    #[must_use]
    pub fn state_estimate(&self) -> &Vector<T, NX> {
        &self.x_hat
    }

    /// Returns the current posterior covariance with shape `(NX, NX)`.
    #[must_use]
    pub fn covariance(&self) -> &Matrix<T, NX, NX> {
        &self.p
    }

    /// Computes the non-mutating prediction stage.
    ///
    /// Args:
    ///   input: Input vector with shape `(NU,)` applied over one sample
    ///     interval.
    ///
    /// Returns:
    ///   Prediction-stage state, covariance, and output terms, with shapes
    ///   `(NX,)`, `(NX, NX)`, and `(NY,)`.
    pub fn predict(
        &self,
        input: Vector<T, NU>,
    ) -> Result<KalmanPrediction<T, NX, NY>, EmbeddedError> {
        let state = self.system.next_state(&self.x_hat, &input);
        let covariance = mat_add(
            &mat_mul(
                &mat_mul(self.system.a(), &self.p),
                &transpose(self.system.a()),
            ),
            &self.w,
        );
        let output = self.system.output(&state, &input);
        Ok(KalmanPrediction {
            state,
            covariance,
            output,
        })
    }

    /// Computes the non-mutating measurement update.
    ///
    /// Args:
    ///   prediction: Prediction-stage state, covariance, and output computed
    ///     from the same sample.
    ///   input: Input vector with shape `(NU,)` used for the measurement-side
    ///     feedthrough term `D u`.
    ///   measurement: Measurement vector with shape `(NY,)` in the same units
    ///     as the plant output.
    ///
    /// Returns:
    ///   Update-stage innovation, gain, posterior state, posterior covariance,
    ///   and posterior output, with shapes `(NY,)`, `(NX, NY)`, `(NX,)`,
    ///   `(NX, NX)`, and `(NY,)`.
    pub fn update(
        &self,
        prediction: &KalmanPrediction<T, NX, NY>,
        input: Vector<T, NU>,
        measurement: Vector<T, NY>,
    ) -> Result<KalmanUpdate<T, NX, NY>, EmbeddedError> {
        let predicted_output = self.system.output(&prediction.state, &input);
        let innovation = vec_sub(&measurement, &predicted_output);
        let innovation_covariance = mat_add(
            &mat_mul(
                &mat_mul(self.system.c(), &prediction.covariance),
                &transpose(self.system.c()),
            ),
            &self.v,
        );
        let cross_covariance = mat_mul(&prediction.covariance, &transpose(self.system.c()));
        let gain = transpose(&solve_linear_system(
            &innovation_covariance,
            &transpose(&cross_covariance),
            "kalman.innovation_covariance",
        )?);
        let whitened_innovation = solve_linear_system(
            &innovation_covariance,
            &column_matrix(&innovation),
            "kalman.innovation_covariance",
        )?;
        let state = vec_add(&prediction.state, &mat_vec_mul(&gain, &innovation));
        let covariance = updated_covariance(
            self.covariance_update,
            &prediction.covariance,
            &gain,
            self.system.c(),
            &self.v,
            &innovation_covariance,
        );
        let output = self.system.output(&state, &input);
        let innovation_norm = vec_norm(&innovation);
        let normalized_innovation_norm =
            normalized_innovation_norm(&innovation, &whitened_innovation).sqrt();

        Ok(KalmanUpdate {
            innovation,
            innovation_norm,
            innovation_covariance,
            normalized_innovation_norm,
            gain,
            predicted_output,
            state,
            covariance,
            output,
        })
    }

    /// Runs one full recursive predict/update cycle and stores the posterior
    /// estimate.
    ///
    /// Args:
    ///   input: Input vector with shape `(NU,)`.
    ///   measurement: Measurement vector with shape `(NY,)` in output units.
    ///
    /// Returns:
    ///   The full update-stage result for the sample.
    pub fn step(
        &mut self,
        input: Vector<T, NU>,
        measurement: Vector<T, NY>,
    ) -> Result<KalmanUpdate<T, NX, NY>, EmbeddedError> {
        let prediction = self.predict(input)?;
        let update = self.update(&prediction, input, measurement)?;
        self.x_hat = update.state;
        self.p = update.covariance;
        Ok(update)
    }
}

impl<T, const NX: usize, const NU: usize, const NY: usize> SteadyStateKalmanFilter<T, NX, NU, NY>
where
    T: Float,
{
    /// Creates a fixed-gain steady-state observer from an explicit filter-form
    /// correction gain.
    ///
    /// Args:
    ///   system: Discrete-time plant realization with `NX` states, `NU`
    ///     inputs, `NY` outputs, and a stored sample interval.
    ///   gain: Steady-state correction gain `K` with shape `(NX, NY)` used in
    ///     `x^+ = x^- + K (y - y^-)`.
    ///   x_hat: Initial state estimate with shape `(NX,)`.
    ///   steady_state_covariance: Optional steady-state covariance with shape
    ///     `(NX, NX)`.
    ///
    /// Returns:
    ///   A fixed-gain observer that reuses the supplied gain at each sample.
    pub fn from_filter_gain(
        system: DiscreteStateSpace<T, NX, NU, NY>,
        gain: Matrix<T, NX, NY>,
        x_hat: Vector<T, NX>,
        steady_state_covariance: Option<Matrix<T, NX, NX>>,
    ) -> Self {
        Self {
            system,
            gain,
            x_hat,
            steady_state_covariance,
        }
    }

    /// Returns the current state estimate with shape `(NX,)`.
    #[must_use]
    pub fn state_estimate(&self) -> &Vector<T, NX> {
        &self.x_hat
    }

    /// Computes the predictor stage `A x + B u`.
    ///
    /// Args:
    ///   input: Input vector with shape `(NU,)`.
    ///
    /// Returns:
    ///   The predicted state vector with shape `(NX,)`.
    pub fn predict(&self, input: Vector<T, NU>) -> Vector<T, NX> {
        self.system.next_state(&self.x_hat, &input)
    }

    /// Applies one fixed-gain update from an externally supplied prediction.
    ///
    /// Args:
    ///   prediction_state: Predicted state vector with shape `(NX,)`.
    ///   input: Input vector with shape `(NU,)` used for the feedthrough term.
    ///   measurement: Measurement vector with shape `(NY,)` in output units.
    ///
    /// Returns:
    ///   A tuple containing the innovation `(NY,)`, updated state `(NX,)`, and
    ///   updated output `(NY,)`.
    pub fn update(
        &self,
        prediction_state: Vector<T, NX>,
        input: Vector<T, NU>,
        measurement: Vector<T, NY>,
    ) -> (Vector<T, NY>, Vector<T, NX>, Vector<T, NY>) {
        let predicted_output = self.system.output(&prediction_state, &input);
        let innovation = vec_sub(&measurement, &predicted_output);
        let state = vec_add(&prediction_state, &mat_vec_mul(&self.gain, &innovation));
        let output = self.system.output(&state, &input);
        (innovation, state, output)
    }

    /// Runs one full fixed-gain observer step and stores the new estimate.
    ///
    /// Args:
    ///   input: Input vector with shape `(NU,)`.
    ///   measurement: Measurement vector with shape `(NY,)` in output units.
    ///
    /// Returns:
    ///   The updated output estimate with shape `(NY,)`.
    pub fn step(&mut self, input: Vector<T, NU>, measurement: Vector<T, NY>) -> Vector<T, NY> {
        let prediction = self.predict(input);
        let (_innovation, state, output) = self.update(prediction, input, measurement);
        self.x_hat = state;
        output
    }
}

/// Packs one fixed-size vector into a single-column right-hand side block.
fn column_matrix<T, const N: usize>(vector: &Vector<T, N>) -> Matrix<T, N, 1>
where
    T: Float,
{
    let mut out = [[T::zero(); 1]; N];
    for idx in 0..N {
        out[idx][0] = vector[idx];
    }
    out
}

/// Applies the configured covariance update law.
fn updated_covariance<T, const NX: usize, const NY: usize>(
    covariance_update: CovarianceUpdate,
    prediction_covariance: &Matrix<T, NX, NX>,
    gain: &Matrix<T, NX, NY>,
    c: &Matrix<T, NY, NX>,
    v: &Matrix<T, NY, NY>,
    innovation_covariance: &Matrix<T, NY, NY>,
) -> Matrix<T, NX, NX>
where
    T: Float,
{
    match covariance_update {
        CovarianceUpdate::Simple => mat_sub(
            prediction_covariance,
            &mat_mul(&mat_mul(gain, innovation_covariance), &transpose(gain)),
        ),
        CovarianceUpdate::Joseph => {
            let identity = identity_matrix::<T, NX>();
            let residual = mat_sub(&identity, &mat_mul(gain, c));
            mat_add(
                &mat_mul(
                    &mat_mul(&residual, prediction_covariance),
                    &transpose(&residual),
                ),
                &mat_mul(&mat_mul(gain, v), &transpose(gain)),
            )
        }
    }
}

/// Computes the normalized innovation energy `r^T z` after solving `S z = r`.
fn normalized_innovation_norm<T, const NY: usize>(
    innovation: &Vector<T, NY>,
    whitened_innovation: &Matrix<T, NY, 1>,
) -> T
where
    T: Float,
{
    let mut acc = T::zero();
    for idx in 0..NY {
        acc = acc + innovation[idx] * whitened_innovation[idx][0];
    }
    acc.max(T::zero())
}

#[cfg(feature = "alloc")]
impl<T, const NX: usize, const NU: usize, const NY: usize> DiscreteKalmanFilter<T, NX, NU, NY>
where
    T: Float + faer_traits::RealField,
{
    /// Builds a fixed-size embedded Kalman filter from the dynamic control-side
    /// state-space model and covariance data.
    ///
    /// Args:
    ///   system: Dynamic discrete-time plant model with `NX` states, `NU`
    ///     inputs, and `NY` outputs.
    ///   w: Process-noise covariance with shape `(NX, NX)`.
    ///   v: Measurement-noise covariance with shape `(NY, NY)`.
    ///   x_hat: Initial posterior state estimate with shape `(NX,)`.
    ///   p: Initial posterior covariance with shape `(NX, NX)`.
    ///
    /// Returns:
    ///   A fixed-size recursive Kalman filter using copied model data.
    pub fn from_control_state_space(
        system: &crate::control::lti::DiscreteStateSpace<T>,
        w: Matrix<T, NX, NX>,
        v: Matrix<T, NY, NY>,
        x_hat: Vector<T, NX>,
        p: Matrix<T, NX, NX>,
    ) -> Result<Self, EmbeddedError> {
        Self::new(DiscreteStateSpace::try_from(system)?, w, v, x_hat, p)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_kalman_step_runs() {
        let system = DiscreteStateSpace::new(
            [[1.0, 0.1], [0.0, 1.0]],
            [[0.0], [0.1]],
            [[1.0, 0.0]],
            [[0.0]],
            0.1,
        )
        .unwrap();
        let mut filter = DiscreteKalmanFilter::new(
            system,
            [[1.0e-3, 0.0], [0.0, 1.0e-3]],
            [[1.0e-2]],
            [0.0, 0.0],
            identity_matrix::<f64, 2>(),
        )
        .unwrap();

        let update = filter.step([0.0], [1.0]).unwrap();
        assert!(update.output[0].is_finite());
        assert!(filter.state_estimate()[0].is_finite());
    }

    #[test]
    fn steady_state_from_filter_gain_runs() {
        let system = DiscreteStateSpace::new(
            [[1.0, 0.1], [0.0, 1.0]],
            [[0.0], [0.1]],
            [[1.0, 0.0]],
            [[0.0]],
            0.1,
        )
        .unwrap();
        let mut filter = SteadyStateKalmanFilter::from_filter_gain(
            system,
            [[0.75], [0.25]],
            [0.0, 0.0],
            Some(identity_matrix::<f64, 2>()),
        );

        let output = filter.step([0.0], [1.0]);
        assert!(output[0].is_finite());
        assert!(filter.state_estimate()[0].is_finite());
    }
}
