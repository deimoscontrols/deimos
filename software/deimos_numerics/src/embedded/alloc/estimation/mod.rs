//! Dynamic-size nonlinear estimators for `embedded::alloc`.
//!
//! # Glossary
//!
//! - **Posterior:** State estimate after incorporating the latest
//!   measurement.
//! - **Prediction:** State estimate before the latest measurement update.
//! - **Measurement update:** Correction stage driven by the innovation.

use crate::embedded::EmbeddedError;

mod core;
mod dense;
mod ekf;
mod ukf;

fn validate_input_dim<T, M>(model: &M, input: &[T]) -> Result<(), EmbeddedError>
where
    M: ekf::DiscreteNonlinearModel<T>,
{
    if input.len() == model.input_dim() {
        Ok(())
    } else {
        Err(EmbeddedError::LengthMismatch {
            which: "embedded.alloc.estimation.input",
            expected: model.input_dim(),
            actual: input.len(),
        })
    }
}

fn validate_output_dim<T, M>(model: &M, output: &[T]) -> Result<(), EmbeddedError>
where
    M: ekf::DiscreteNonlinearModel<T>,
{
    if output.len() == model.output_dim() {
        Ok(())
    } else {
        Err(EmbeddedError::LengthMismatch {
            which: "embedded.alloc.estimation.output",
            expected: model.output_dim(),
            actual: output.len(),
        })
    }
}

pub use ekf::{
    DiscreteExtendedKalmanModel, DiscreteNonlinearModel, ExtendedKalmanFilter,
    ExtendedKalmanPrediction, ExtendedKalmanUpdate,
};
pub use ukf::{UnscentedKalmanFilter, UnscentedKalmanPrediction, UnscentedKalmanUpdate};
