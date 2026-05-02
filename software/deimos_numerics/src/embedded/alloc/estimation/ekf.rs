//! Simplified dynamic-size discrete-time extended Kalman filtering.
//!
//! # Glossary
//!
//! - **Jacobian:** First derivative of the state or output map with respect to
//!   the state.
//! - **Innovation:** Measurement residual between the actual and predicted
//!   outputs.
//! - **Process-noise covariance:** Covariance assigned to state evolution
//!   uncertainty.
//! - **Measurement-noise covariance:** Covariance assigned to sensor
//!   uncertainty.

use super::core::{normalized_innovation_norm, predict_covariance, updated_covariance};
use super::dense::{
    llt_solve, mat_mul, mat_mul_vec, vec_add, vec_as_slice, vec_as_slice_mut, vec_norm, vec_sub,
    vector_as_column_matrix, vector_from_slice, zero_matrix, zero_vector,
};
use super::{validate_input_dim, validate_output_dim};
use crate::control::estimation::CovarianceUpdate;
use crate::embedded::EmbeddedError;
use crate::embedded::alloc::{Matrix, Vector};
use crate::sparse::compensated::CompensatedField;
use faer_traits::RealField;
use num_traits::Float;

/// Dynamic nonlinear discrete-time model.
pub trait DiscreteNonlinearModel<T> {
    /// State dimension.
    fn state_dim(&self) -> usize;
    /// Input dimension.
    fn input_dim(&self) -> usize;
    /// Output dimension.
    fn output_dim(&self) -> usize;
    /// Evaluates the transition `x[k+1] = f(x[k], u[k])`.
    ///
    /// Args:
    ///   state: Current state slice with length `state_dim()`.
    ///   input: Current input slice with length `input_dim()`.
    ///   next_state: Output buffer with length `state_dim()` written with the
    ///     next state.
    fn transition(&self, state: &[T], input: &[T], next_state: &mut [T]);
    /// Evaluates the output `y[k] = h(x[k], u[k])`.
    ///
    /// Args:
    ///   state: Current state slice with length `state_dim()`.
    ///   input: Current input slice with length `input_dim()`.
    ///   output: Output buffer with length `output_dim()` written with the
    ///     model output in measurement units.
    fn output(&self, state: &[T], input: &[T], output: &mut [T]);
}

/// EKF-specific nonlinear model with explicit Jacobians.
pub trait DiscreteExtendedKalmanModel<T>: DiscreteNonlinearModel<T> {
    /// Evaluates the transition Jacobian `∂f/∂x`.
    ///
    /// Args:
    ///   state: Current state slice with length `state_dim()`.
    ///   input: Current input slice with length `input_dim()`.
    ///   jacobian: Output matrix with shape `(state_dim(), state_dim())`.
    fn transition_jacobian(&self, state: &[T], input: &[T], jacobian: &mut Matrix<T>);
    /// Evaluates the output Jacobian `∂h/∂x`.
    ///
    /// Args:
    ///   state: Current state slice with length `state_dim()`.
    ///   input: Current input slice with length `input_dim()`.
    ///   jacobian: Output matrix with shape `(output_dim(), state_dim())`.
    fn output_jacobian(&self, state: &[T], input: &[T], jacobian: &mut Matrix<T>);
}

/// One EKF prediction stage.
///
/// Each field is a prediction-stage quantity with state shape `(nx, 1)`,
/// covariance shape `(nx, nx)`, or output shape `(ny, 1)`.
#[derive(Clone, Debug, PartialEq)]
pub struct ExtendedKalmanPrediction<T> {
    /// Predicted state estimate.
    pub state: Vector<T>,
    /// Predicted covariance.
    pub covariance: Matrix<T>,
    /// Predicted output.
    pub output: Vector<T>,
}

/// One EKF update stage.
///
/// Each field is an update-stage quantity with innovation shape `(ny, 1)`,
/// gain shape `(nx, ny)`, state shape `(nx, 1)`, covariance shape `(nx, nx)`,
/// or output shape `(ny, 1)`.
#[derive(Clone, Debug, PartialEq)]
pub struct ExtendedKalmanUpdate<T> {
    /// Innovation `y - h(x^-, u)`.
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
    /// Posterior output estimate.
    pub output: Vector<T>,
}

#[derive(Clone, Debug, PartialEq)]
struct EkfScratch<T> {
    f: Matrix<T>,
    h: Matrix<T>,
    state: Vector<T>,
    output: Vector<T>,
}

impl<T> EkfScratch<T>
where
    T: Float,
{
    fn new(state_dim: usize, output_dim: usize) -> Self {
        Self {
            f: zero_matrix(state_dim, state_dim),
            h: zero_matrix(output_dim, state_dim),
            state: zero_vector(state_dim),
            output: zero_vector(output_dim),
        }
    }

    fn ensure_dims(&mut self, state_dim: usize, output_dim: usize) {
        if self.f.nrows() != state_dim || self.f.ncols() != state_dim {
            self.f = zero_matrix(state_dim, state_dim);
        }
        if self.h.nrows() != output_dim || self.h.ncols() != state_dim {
            self.h = zero_matrix(output_dim, state_dim);
        }
        if self.state.nrows() != state_dim {
            self.state = zero_vector(state_dim);
        }
        if self.output.nrows() != output_dim {
            self.output = zero_vector(output_dim);
        }
    }
}

/// Dynamic-size extended Kalman filter runtime.
#[derive(Clone, Debug, PartialEq)]
pub struct ExtendedKalmanFilter<T, M> {
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
    scratch: EkfScratch<T>,
}

impl<T, M> ExtendedKalmanFilter<T, M>
where
    T: CompensatedField + RealField,
    T::Real: Float,
    M: DiscreteExtendedKalmanModel<T>,
{
    /// Creates a validated dynamic-size EKF runtime.
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
    ///   A validated EKF runtime with reusable internal scratch for `step()`.
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
                which: "embedded.alloc.ekf.x_hat",
                expected: nx,
                actual: x_hat.nrows(),
            });
        }
        if w.nrows() != nx || w.ncols() != nx {
            return Err(EmbeddedError::DimensionMismatch {
                which: "embedded.alloc.ekf.w",
                expected_rows: nx,
                expected_cols: nx,
                actual_rows: w.nrows(),
                actual_cols: w.ncols(),
            });
        }
        if v.nrows() != ny || v.ncols() != ny {
            return Err(EmbeddedError::DimensionMismatch {
                which: "embedded.alloc.ekf.v",
                expected_rows: ny,
                expected_cols: ny,
                actual_rows: v.nrows(),
                actual_cols: v.ncols(),
            });
        }
        if p.nrows() != nx || p.ncols() != nx {
            return Err(EmbeddedError::DimensionMismatch {
                which: "embedded.alloc.ekf.p",
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
            scratch: EkfScratch::new(nx, ny),
        })
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

    /// Computes the non-mutating prediction stage.
    ///
    /// Args:
    ///   input: Input slice with length `input_dim()`.
    ///
    /// Returns:
    ///   Prediction-stage state `(nx, 1)`, covariance `(nx, nx)`, and output
    ///   `(ny, 1)`.
    pub fn predict(&self, input: &[T]) -> Result<ExtendedKalmanPrediction<T>, EmbeddedError> {
        validate_input_dim(&self.model, input)?;

        let nx = self.model.state_dim();
        let mut f = zero_matrix(nx, nx);
        self.model
            .transition_jacobian(vec_as_slice(&self.x_hat), input, &mut f);

        let mut state = zero_vector(nx);
        self.model.transition(
            vec_as_slice(&self.x_hat),
            input,
            vec_as_slice_mut(&mut state),
        );

        let covariance = predict_covariance(f.as_ref(), self.p.as_ref(), self.w.as_ref());
        let mut output = zero_vector(self.model.output_dim());
        self.model
            .output(vec_as_slice(&state), input, vec_as_slice_mut(&mut output));

        Ok(ExtendedKalmanPrediction {
            state,
            covariance,
            output,
        })
    }

    /// Computes the non-mutating measurement update.
    ///
    /// Args:
    ///   prediction: Prediction-stage result computed from the same sample.
    ///   input: Input slice with length `input_dim()` used by the output map.
    ///   measurement: Measurement slice with length `output_dim()` in the same
    ///     units as the model output.
    ///
    /// Returns:
    ///   Update-stage innovation `(ny, 1)`, gain `(nx, ny)`, posterior state
    ///   `(nx, 1)`, posterior covariance `(nx, nx)`, and posterior output
    ///   `(ny, 1)`.
    pub fn update(
        &self,
        prediction: &ExtendedKalmanPrediction<T>,
        input: &[T],
        measurement: &[T],
    ) -> Result<ExtendedKalmanUpdate<T>, EmbeddedError> {
        validate_input_dim(&self.model, input)?;
        validate_output_dim(&self.model, measurement)?;

        let ny = self.model.output_dim();
        let mut h = zero_matrix(ny, self.model.state_dim());
        self.model
            .output_jacobian(vec_as_slice(&prediction.state), input, &mut h);

        let innovation = vec_sub(&vector_from_slice(measurement), &prediction.output)?;
        let innovation_covariance =
            predict_covariance(h.as_ref(), prediction.covariance.as_ref(), self.v.as_ref());
        let h_t = h.transpose().to_owned();
        let cross_covariance = mat_mul(&prediction.covariance, &h_t)?;
        let cross_t = cross_covariance.transpose().to_owned();
        let gain = llt_solve(
            &innovation_covariance,
            &cross_t,
            "embedded.alloc.ekf.innovation_covariance",
        )?
        .transpose()
        .to_owned();
        let state = vec_add(&prediction.state, &mat_mul_vec(&gain, &innovation)?)?;
        let covariance = updated_covariance(
            CovarianceUpdate::Joseph,
            prediction.covariance.as_ref(),
            gain.as_ref(),
            h.as_ref(),
            self.v.as_ref(),
            innovation_covariance.as_ref(),
        );

        let mut output = zero_vector(ny);
        self.model
            .output(vec_as_slice(&state), input, vec_as_slice_mut(&mut output));

        Ok(ExtendedKalmanUpdate {
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
            output,
        })
    }

    /// Runs one full EKF step and stores the posterior estimate.
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
    ) -> Result<ExtendedKalmanUpdate<T>, EmbeddedError> {
        validate_input_dim(&self.model, input)?;
        validate_output_dim(&self.model, measurement)?;

        let nx = self.model.state_dim();
        let ny = self.model.output_dim();
        self.scratch.ensure_dims(nx, ny);

        self.model
            .transition_jacobian(vec_as_slice(&self.x_hat), input, &mut self.scratch.f);
        self.model.transition(
            vec_as_slice(&self.x_hat),
            input,
            vec_as_slice_mut(&mut self.scratch.state),
        );

        let prediction_covariance =
            predict_covariance(self.scratch.f.as_ref(), self.p.as_ref(), self.w.as_ref());

        self.model.output(
            vec_as_slice(&self.scratch.state),
            input,
            vec_as_slice_mut(&mut self.scratch.output),
        );
        let innovation = vec_sub(&vector_from_slice(measurement), &self.scratch.output)?;

        self.model.output_jacobian(
            vec_as_slice(&self.scratch.state),
            input,
            &mut self.scratch.h,
        );
        let innovation_covariance = predict_covariance(
            self.scratch.h.as_ref(),
            prediction_covariance.as_ref(),
            self.v.as_ref(),
        );
        let h_t = self.scratch.h.transpose().to_owned();
        let cross_covariance = mat_mul(&prediction_covariance, &h_t)?;
        let cross_t = cross_covariance.transpose().to_owned();
        let gain = llt_solve(
            &innovation_covariance,
            &cross_t,
            "embedded.alloc.ekf.innovation_covariance",
        )?
        .transpose()
        .to_owned();
        let state = vec_add(&self.scratch.state, &mat_mul_vec(&gain, &innovation)?)?;
        let covariance = updated_covariance(
            CovarianceUpdate::Joseph,
            prediction_covariance.as_ref(),
            gain.as_ref(),
            self.scratch.h.as_ref(),
            self.v.as_ref(),
            innovation_covariance.as_ref(),
        );

        self.model.output(
            vec_as_slice(&state),
            input,
            vec_as_slice_mut(&mut self.scratch.output),
        );
        let output = self.scratch.output.clone();

        self.x_hat = state.clone();
        self.p = covariance.clone();

        Ok(ExtendedKalmanUpdate {
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
            output,
        })
    }
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

    impl DiscreteExtendedKalmanModel<f64> for QuadraticSensor {
        fn transition_jacobian(&self, _state: &[f64], _input: &[f64], jacobian: &mut Matrix<f64>) {
            jacobian[(0, 0)] = 1.0;
        }

        fn output_jacobian(&self, state: &[f64], _input: &[f64], jacobian: &mut Matrix<f64>) {
            jacobian[(0, 0)] = 2.0 * state[0];
        }
    }

    #[test]
    fn alloc_ekf_step_runs() {
        let mut filter = ExtendedKalmanFilter::new(
            QuadraticSensor,
            scale_matrix(&identity_matrix(1), 1.0e-3),
            scale_matrix(&identity_matrix(1), 1.0e-2),
            vector_from_slice(&[0.5]),
            identity_matrix(1),
        )
        .unwrap();

        let update = filter.step(&[0.1], &[0.49]).unwrap();
        assert!(update.output[0].is_finite());
        assert!(filter.state_estimate()[0].is_finite());
    }
}
