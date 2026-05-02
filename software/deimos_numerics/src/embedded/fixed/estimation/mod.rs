//! Fixed-size embedded estimation kernels.
//!
//! # Glossary
//!
//! - **Recursive filter:** Estimator that updates both state and covariance at
//!   each sample.
//! - **Steady-state filter:** Estimator that reuses a fixed correction gain at
//!   runtime.

mod kalman;

pub use kalman::{
    CovarianceUpdate, DiscreteKalmanFilter, KalmanPrediction, KalmanUpdate, SteadyStateKalmanFilter,
};
