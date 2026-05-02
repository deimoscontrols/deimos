//! Simplified SPD-only dynamic-size discrete-time unscented Kalman filtering.
//!
//! # Glossary
//!
//! - **Sigma points:** Deterministic samples used by the unscented transform.
//! - **Alpha / beta / kappa:** Spread and prior-shape parameters for the sigma
//!   point set.
//! - **SPD:** Symmetric positive definite.
//! - **Innovation covariance:** Covariance of the predicted measurement error.

use super::core::{
    normalized_innovation_norm, updated_covariance_ukf, weighted_covariance,
    weighted_cross_covariance, weighted_mean,
};
use super::dense::{
    cholesky_lower, col_as_slice, col_as_slice_mut, column_matrix_to_vector, llt_solve,
    mat_mul_vec, scale_matrix, vec_add, vec_as_slice, vec_as_slice_mut, vec_norm, vec_sub,
    vector_as_column_matrix, vector_from_slice, vectors_as_columns, zero_matrix, zero_vector,
};
use super::ekf::DiscreteNonlinearModel;
use super::{validate_input_dim, validate_output_dim};
use crate::control::estimation::CovarianceUpdate;
use crate::embedded::EmbeddedError;
use crate::embedded::alloc::{Matrix, Vector};
use crate::sparse::compensated::CompensatedField;
use alloc::vec::Vec;
use faer::Col;
use faer_traits::RealField;
use num_traits::Float;

/// One UKF prediction stage.
///
/// Each field is a prediction-stage quantity with state shape `(nx, 1)`,
/// covariance shape `(nx, nx)`, or sigma-point collection length
/// `2 * nx + 1`.
#[derive(Clone, Debug, PartialEq)]
pub struct UnscentedKalmanPrediction<T> {
    /// Predicted state estimate.
    pub state: Vector<T>,
    /// Predicted covariance.
    pub covariance: Matrix<T>,
    /// Propagated state sigma points.
    pub sigma_points: Vec<Vector<T>>,
}

/// One UKF update stage.
///
/// Each field is an update-stage quantity with innovation shape `(ny, 1)`,
/// gain shape `(nx, ny)`, state shape `(nx, 1)`, covariance shape `(nx, nx)`,
/// or predicted-output shape `(ny, 1)`.
#[derive(Clone, Debug, PartialEq)]
pub struct UnscentedKalmanUpdate<T> {
    /// Innovation `y - y^-`.
    pub innovation: Vector<T>,
    /// Euclidean innovation norm.
    pub innovation_norm: T,
    /// Innovation covariance.
    pub innovation_covariance: Matrix<T>,
    /// Normalized innovation norm.
    pub normalized_innovation_norm: T,
    /// Kalman gain.
    pub gain: Matrix<T>,
    /// Posterior state estimate.
    pub state: Vector<T>,
    /// Posterior covariance.
    pub covariance: Matrix<T>,
    /// Predicted measurement.
    pub predicted_output: Vector<T>,
}

#[derive(Clone, Debug, PartialEq)]
struct UkfScratch<T> {
    sigma_points: Matrix<T>,
    propagated_state_points: Matrix<T>,
    measurement_points: Matrix<T>,
}

impl<T> UkfScratch<T>
where
    T: Float,
{
    fn new(state_dim: usize, output_dim: usize) -> Self {
        let npoints = 2 * state_dim + 1;
        Self {
            sigma_points: zero_matrix(state_dim, npoints),
            propagated_state_points: zero_matrix(state_dim, npoints),
            measurement_points: zero_matrix(output_dim, npoints),
        }
    }

    fn ensure_dims(&mut self, state_dim: usize, output_dim: usize) {
        let npoints = 2 * state_dim + 1;
        if self.sigma_points.nrows() != state_dim || self.sigma_points.ncols() != npoints {
            self.sigma_points = zero_matrix(state_dim, npoints);
        }
        if self.propagated_state_points.nrows() != state_dim
            || self.propagated_state_points.ncols() != npoints
        {
            self.propagated_state_points = zero_matrix(state_dim, npoints);
        }
        if self.measurement_points.nrows() != output_dim
            || self.measurement_points.ncols() != npoints
        {
            self.measurement_points = zero_matrix(output_dim, npoints);
        }
    }
}

/// Simplified SPD-only unscented Kalman filter runtime.
#[derive(Clone, Debug, PartialEq)]
pub struct UnscentedKalmanFilter<T, M> {
    /// Nonlinear model.
    pub model: M,
    /// Process-noise covariance.
    pub w: Matrix<T>,
    /// Measurement-noise covariance.
    pub v: Matrix<T>,
    /// Current posterior state estimate.
    pub x_hat: Vector<T>,
    /// Current posterior covariance.
    pub p: Matrix<T>,
    /// Spread parameter.
    pub alpha: T,
    /// Prior-knowledge parameter.
    pub beta: T,
    /// Secondary spread parameter.
    pub kappa: T,
    scratch: UkfScratch<T>,
}

impl<T, M> UnscentedKalmanFilter<T, M>
where
    T: CompensatedField + RealField,
    T::Real: Float,
    M: DiscreteNonlinearModel<T>,
{
    /// Creates a validated SPD-only UKF runtime.
    ///
    /// Args:
    ///   model: Nonlinear model with `nx = state_dim()` states and `ny =
    ///     output_dim()` outputs.
    ///   w: Process-noise covariance with shape `(nx, nx)`.
    ///   v: Measurement-noise covariance with shape `(ny, ny)`.
    ///   x_hat: Initial posterior state estimate with shape `(nx, 1)`.
    ///   p: Initial posterior covariance with shape `(nx, nx)`.
    ///
    /// Returns:
    ///   A validated UKF runtime with reusable sigma-point scratch for
    ///   `step()`.
    pub fn new(
        model: M,
        w: Matrix<T>,
        v: Matrix<T>,
        x_hat: Vector<T>,
        p: Matrix<T>,
    ) -> Result<Self, EmbeddedError> {
        let nx = model.state_dim();
        let ny = model.output_dim();
        if x_hat.nrows() != nx {
            return Err(EmbeddedError::LengthMismatch {
                which: "embedded.alloc.ukf.x_hat",
                expected: nx,
                actual: x_hat.nrows(),
            });
        }
        if w.nrows() != nx || w.ncols() != nx {
            return Err(EmbeddedError::DimensionMismatch {
                which: "embedded.alloc.ukf.w",
                expected_rows: nx,
                expected_cols: nx,
                actual_rows: w.nrows(),
                actual_cols: w.ncols(),
            });
        }
        if v.nrows() != ny || v.ncols() != ny {
            return Err(EmbeddedError::DimensionMismatch {
                which: "embedded.alloc.ukf.v",
                expected_rows: ny,
                expected_cols: ny,
                actual_rows: v.nrows(),
                actual_cols: v.ncols(),
            });
        }
        if p.nrows() != nx || p.ncols() != nx {
            return Err(EmbeddedError::DimensionMismatch {
                which: "embedded.alloc.ukf.p",
                expected_rows: nx,
                expected_cols: nx,
                actual_rows: p.nrows(),
                actual_cols: p.ncols(),
            });
        }

        Ok(Self {
            model,
            w,
            v,
            x_hat,
            p,
            alpha: T::epsilon().sqrt(),
            beta: T::one() + T::one(),
            kappa: T::zero(),
            scratch: UkfScratch::new(nx, ny),
        })
    }

    /// Overrides the sigma-point spread parameters.
    ///
    /// Args:
    ///   alpha: Primary sigma-point spread parameter. It is dimensionless and
    ///     must be finite and positive.
    ///   beta: Prior-knowledge parameter. It is dimensionless and must be
    ///     finite and nonnegative.
    ///   kappa: Secondary spread parameter. It is dimensionless and must be
    ///     finite.
    ///
    /// Returns:
    ///   The same filter instance configured with the supplied sigma-point
    ///   parameters.
    pub fn with_sigma_parameters(
        mut self,
        alpha: T,
        beta: T,
        kappa: T,
    ) -> Result<Self, EmbeddedError> {
        if !alpha.is_finite() || alpha <= T::zero() {
            return Err(EmbeddedError::InvalidParameter {
                which: "embedded.alloc.ukf.alpha",
            });
        }
        if !beta.is_finite() || beta < T::zero() {
            return Err(EmbeddedError::InvalidParameter {
                which: "embedded.alloc.ukf.beta",
            });
        }
        if !kappa.is_finite() {
            return Err(EmbeddedError::InvalidParameter {
                which: "embedded.alloc.ukf.kappa",
            });
        }
        self.alpha = alpha;
        self.beta = beta;
        self.kappa = kappa;
        Ok(self)
    }

    /// Returns the current posterior state estimate with shape `(nx, 1)`.
    #[must_use]
    pub fn state_estimate(&self) -> &Vector<T> {
        &self.x_hat
    }

    /// Returns the current posterior covariance with shape `(nx, nx)`.
    #[must_use]
    pub fn covariance(&self) -> &Matrix<T> {
        &self.p
    }

    /// Computes the non-mutating sigma-point prediction stage.
    ///
    /// Args:
    ///   input: Input slice with length `input_dim()`.
    ///
    /// Returns:
    ///   Prediction-stage state `(nx, 1)`, covariance `(nx, nx)`, and the
    ///   propagated sigma-point set with `2 * nx + 1` vectors of shape
    ///   `(nx, 1)`.
    pub fn predict(&self, input: &[T]) -> Result<UnscentedKalmanPrediction<T>, EmbeddedError> {
        validate_input_dim(&self.model, input)?;
        let weights = sigma_weights(self.model.state_dim(), self.alpha, self.beta, self.kappa)?;
        let sigma_points = sigma_points(&self.x_hat, &self.p, self.alpha, self.kappa)?;

        let mut propagated = Vec::with_capacity(sigma_points.len());
        for sigma_point in &sigma_points {
            let mut next_state = zero_vector(self.model.state_dim());
            self.model.transition(
                vec_as_slice(sigma_point),
                input,
                vec_as_slice_mut(&mut next_state),
            );
            propagated.push(next_state);
        }

        let propagated_matrix =
            vectors_as_columns(&propagated, "embedded.alloc.ukf.propagated_sigma_points")?;
        let state = column_matrix_to_vector(
            &weighted_mean(propagated_matrix.as_ref(), vec_as_slice(&weights.mean)),
            "embedded.alloc.ukf.predicted_state",
        )?;
        let covariance = weighted_covariance(
            propagated_matrix.as_ref(),
            vector_as_column_matrix(&state).as_ref(),
            vec_as_slice(&weights.covariance),
        ) + &self.w;

        Ok(UnscentedKalmanPrediction {
            state,
            covariance,
            sigma_points: propagated,
        })
    }

    /// Computes the non-mutating UKF measurement update.
    ///
    /// Args:
    ///   prediction: Prediction-stage result computed from the same sample.
    ///   input: Input slice with length `input_dim()` used by the output map.
    ///   measurement: Measurement slice with length `output_dim()` in the same
    ///     units as the model output.
    ///
    /// Returns:
    ///   Update-stage innovation `(ny, 1)`, gain `(nx, ny)`, posterior state
    ///   `(nx, 1)`, posterior covariance `(nx, nx)`, and predicted output
    ///   `(ny, 1)`.
    pub fn update(
        &self,
        prediction: &UnscentedKalmanPrediction<T>,
        input: &[T],
        measurement: &[T],
    ) -> Result<UnscentedKalmanUpdate<T>, EmbeddedError> {
        validate_input_dim(&self.model, input)?;
        validate_output_dim(&self.model, measurement)?;

        let weights = sigma_weights(self.model.state_dim(), self.alpha, self.beta, self.kappa)?;
        let mut measurement_points = Vec::with_capacity(prediction.sigma_points.len());
        for sigma_point in &prediction.sigma_points {
            let mut output = zero_vector(self.model.output_dim());
            self.model.output(
                vec_as_slice(sigma_point),
                input,
                vec_as_slice_mut(&mut output),
            );
            measurement_points.push(output);
        }

        let state_points_matrix = vectors_as_columns(
            &prediction.sigma_points,
            "embedded.alloc.ukf.state_sigma_points",
        )?;
        let measurement_points_matrix = vectors_as_columns(
            &measurement_points,
            "embedded.alloc.ukf.measurement_sigma_points",
        )?;
        let predicted_output = column_matrix_to_vector(
            &weighted_mean(
                measurement_points_matrix.as_ref(),
                vec_as_slice(&weights.mean),
            ),
            "embedded.alloc.ukf.predicted_output",
        )?;
        let innovation = vec_sub(&vector_from_slice(measurement), &predicted_output)?;
        let innovation_covariance = weighted_covariance(
            measurement_points_matrix.as_ref(),
            vector_as_column_matrix(&predicted_output).as_ref(),
            vec_as_slice(&weights.covariance),
        ) + &self.v;
        let cross_covariance = weighted_cross_covariance(
            state_points_matrix.as_ref(),
            vector_as_column_matrix(&prediction.state).as_ref(),
            measurement_points_matrix.as_ref(),
            vector_as_column_matrix(&predicted_output).as_ref(),
            vec_as_slice(&weights.covariance),
        );
        let cross_t = cross_covariance.transpose().to_owned();
        let gain = llt_solve(
            &innovation_covariance,
            &cross_t,
            "embedded.alloc.ukf.innovation_covariance",
        )?
        .transpose()
        .to_owned();
        let state = vec_add(&prediction.state, &mat_mul_vec(&gain, &innovation)?)?;
        let covariance = updated_covariance_ukf(
            CovarianceUpdate::Simple,
            prediction.covariance.as_ref(),
            gain.as_ref(),
            cross_covariance.as_ref(),
            self.v.as_ref(),
            innovation_covariance.as_ref(),
        )?;

        Ok(UnscentedKalmanUpdate {
            innovation_norm: vec_norm(&innovation),
            normalized_innovation_norm: normalized_innovation_norm(
                vector_as_column_matrix(&innovation).as_ref(),
                innovation_covariance.as_ref(),
            )?,
            innovation,
            innovation_covariance,
            gain,
            state,
            covariance,
            predicted_output,
        })
    }

    /// Runs one full UKF step and stores the posterior estimate.
    ///
    /// Args:
    ///   input: Input slice with length `input_dim()`.
    ///   measurement: Measurement slice with length `output_dim()` in output
    ///     units.
    ///
    /// Returns:
    ///   The full update-stage result for the sample.
    pub fn step(
        &mut self,
        input: &[T],
        measurement: &[T],
    ) -> Result<UnscentedKalmanUpdate<T>, EmbeddedError> {
        validate_input_dim(&self.model, input)?;
        validate_output_dim(&self.model, measurement)?;

        let state_dim = self.model.state_dim();
        let output_dim = self.model.output_dim();
        let npoints = 2 * state_dim + 1;
        self.scratch.ensure_dims(state_dim, output_dim);

        let weights = sigma_weights(state_dim, self.alpha, self.beta, self.kappa)?;
        fill_sigma_points_matrix(
            &mut self.scratch.sigma_points,
            &self.x_hat,
            &self.p,
            self.alpha,
            self.kappa,
        )?;

        for col in 0..npoints {
            self.model.transition(
                col_as_slice(self.scratch.sigma_points.col(col)),
                input,
                col_as_slice_mut(self.scratch.propagated_state_points.col_mut(col)),
            );
        }

        let predicted_state = column_matrix_to_vector(
            &weighted_mean(
                self.scratch.propagated_state_points.as_ref(),
                vec_as_slice(&weights.mean),
            ),
            "embedded.alloc.ukf.predicted_state",
        )?;
        let predicted_state_matrix = vector_as_column_matrix(&predicted_state);
        let predicted_covariance = weighted_covariance(
            self.scratch.propagated_state_points.as_ref(),
            predicted_state_matrix.as_ref(),
            vec_as_slice(&weights.covariance),
        ) + &self.w;

        for col in 0..npoints {
            self.model.output(
                col_as_slice(self.scratch.propagated_state_points.col(col)),
                input,
                col_as_slice_mut(self.scratch.measurement_points.col_mut(col)),
            );
        }

        let predicted_output = column_matrix_to_vector(
            &weighted_mean(
                self.scratch.measurement_points.as_ref(),
                vec_as_slice(&weights.mean),
            ),
            "embedded.alloc.ukf.predicted_output",
        )?;
        let predicted_output_matrix = vector_as_column_matrix(&predicted_output);
        let innovation = vec_sub(&vector_from_slice(measurement), &predicted_output)?;
        let innovation_covariance = weighted_covariance(
            self.scratch.measurement_points.as_ref(),
            predicted_output_matrix.as_ref(),
            vec_as_slice(&weights.covariance),
        ) + &self.v;
        let cross_covariance = weighted_cross_covariance(
            self.scratch.propagated_state_points.as_ref(),
            predicted_state_matrix.as_ref(),
            self.scratch.measurement_points.as_ref(),
            predicted_output_matrix.as_ref(),
            vec_as_slice(&weights.covariance),
        );
        let cross_t = cross_covariance.transpose().to_owned();
        let gain = llt_solve(
            &innovation_covariance,
            &cross_t,
            "embedded.alloc.ukf.innovation_covariance",
        )?
        .transpose()
        .to_owned();
        let state = vec_add(&predicted_state, &mat_mul_vec(&gain, &innovation)?)?;
        let covariance = updated_covariance_ukf(
            CovarianceUpdate::Simple,
            predicted_covariance.as_ref(),
            gain.as_ref(),
            cross_covariance.as_ref(),
            self.v.as_ref(),
            innovation_covariance.as_ref(),
        )?;

        self.x_hat = state.clone();
        self.p = covariance.clone();

        Ok(UnscentedKalmanUpdate {
            innovation_norm: vec_norm(&innovation),
            normalized_innovation_norm: normalized_innovation_norm(
                vector_as_column_matrix(&innovation).as_ref(),
                innovation_covariance.as_ref(),
            )?,
            innovation,
            innovation_covariance,
            gain,
            state,
            covariance,
            predicted_output,
        })
    }
}

/// Weight vectors used by the standard scaled unscented transform.
struct SigmaWeights<T> {
    mean: Vector<T>,
    covariance: Vector<T>,
}

/// Computes the standard scaled unscented weights.
fn sigma_weights<T>(n: usize, alpha: T, beta: T, kappa: T) -> Result<SigmaWeights<T>, EmbeddedError>
where
    T: Float,
{
    let n_t = T::from(n).ok_or(EmbeddedError::InvalidParameter {
        which: "embedded.alloc.ukf.state_dim",
    })?;
    let lambda = alpha * alpha * (n_t + kappa) - n_t;
    let scale = n_t + lambda;
    if !scale.is_finite() || scale <= T::zero() {
        return Err(EmbeddedError::InvalidParameter {
            which: "embedded.alloc.ukf.scale",
        });
    }
    let count = 2 * n + 1;
    let mut mean = zero_vector(count);
    let mut covariance = zero_vector(count);
    let repeated = T::one() / ((T::one() + T::one()) * scale);
    for idx in 0..count {
        mean[idx] = repeated;
        covariance[idx] = repeated;
    }
    mean[0] = lambda / scale;
    covariance[0] = mean[0] + (T::one() - alpha * alpha + beta);
    Ok(SigmaWeights { mean, covariance })
}

/// Builds the sigma points around the current state estimate.
fn sigma_points<T>(
    state: &Vector<T>,
    covariance: &Matrix<T>,
    alpha: T,
    kappa: T,
) -> Result<Vec<Vector<T>>, EmbeddedError>
where
    T: Float,
{
    let n = state.nrows();
    let mut points_matrix = zero_matrix(n, 2 * n + 1);
    fill_sigma_points_matrix(&mut points_matrix, state, covariance, alpha, kappa)?;

    let mut points = Vec::with_capacity(2 * n + 1);
    for col in 0..points_matrix.ncols() {
        let point = points_matrix.col(col);
        points.push(Col::from_fn(n, |row| point[row]));
    }
    Ok(points)
}

fn fill_sigma_points_matrix<T>(
    points: &mut Matrix<T>,
    state: &Vector<T>,
    covariance: &Matrix<T>,
    alpha: T,
    kappa: T,
) -> Result<(), EmbeddedError>
where
    T: Float,
{
    let n = state.nrows();
    let n_t = T::from(n).ok_or(EmbeddedError::InvalidParameter {
        which: "embedded.alloc.ukf.state_dim",
    })?;
    let lambda = alpha * alpha * (n_t + kappa) - n_t;
    let scale = n_t + lambda;
    let factor = cholesky_lower(
        &scale_matrix(covariance, scale),
        "embedded.alloc.ukf.cholesky",
    )?;

    if points.nrows() != n || points.ncols() != 2 * n + 1 {
        *points = zero_matrix(n, 2 * n + 1);
    }

    for row in 0..n {
        points[(row, 0)] = state[row];
    }
    for col in 0..n {
        for row in 0..n {
            let delta = factor[(row, col)];
            points[(row, col + 1)] = state[row] + delta;
            points[(row, col + n + 1)] = state[row] - delta;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::dense::{identity_matrix, scale_matrix, vector_from_slice};
    use super::*;

    #[derive(Clone, Debug, PartialEq)]
    struct QuadraticSensor;

    impl DiscreteNonlinearModel<f64> for QuadraticSensor {
        fn state_dim(&self) -> usize {
            1
        }

        fn input_dim(&self) -> usize {
            1
        }

        fn output_dim(&self) -> usize {
            1
        }

        fn transition(&self, state: &[f64], input: &[f64], next_state: &mut [f64]) {
            next_state[0] = state[0] + input[0];
        }

        fn output(&self, state: &[f64], _input: &[f64], output: &mut [f64]) {
            output[0] = state[0] * state[0];
        }
    }

    #[test]
    fn alloc_ukf_step_runs() {
        let mut filter = UnscentedKalmanFilter::new(
            QuadraticSensor,
            scale_matrix(&identity_matrix(1), 1.0e-3),
            scale_matrix(&identity_matrix(1), 1.0e-2),
            vector_from_slice(&[0.5]),
            identity_matrix(1),
        )
        .unwrap();

        let update = filter.step(&[0.1], &[0.49]).unwrap();
        assert!(update.predicted_output[0].is_finite());
        assert!(filter.state_estimate()[0].is_finite());
    }
}
