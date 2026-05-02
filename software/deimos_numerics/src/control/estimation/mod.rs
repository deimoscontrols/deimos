//! Estimator design and runtime filtering.
//!
//! This subsystem groups the crate's public linear estimator features:
//!
//! - [`linear`] covers `LQE` / `DLQE`, discrete linear Kalman filtering, and
//!   fixed-gain observer runtimes
//!
//! Nonlinear `EKF` / `UKF` runtimes now live under
//! [`crate::embedded::alloc::estimation`], which keeps the public split between
//! allocating nonlinear runtime execution and the broader control-side design
//! stack explicit.
//!
//! # Two Intuitions
//!
//! 1. **Information-flow view.** Estimation turns noisy outputs and known
//!    inputs into a best current guess of the hidden system state.
//! 2. **Dual-control view.** Linear estimator design mirrors controller
//!    synthesis on the transposed system: `LQE` is the observer-side dual of
//!    `LQR`.
//!
//! # Glossary
//!
//! - **Observer gain `L`:** Injection gain in `A - L C`.
//! - **Innovation:** Measurement residual `y - y^-`.
//! - **Joseph update:** Numerically robust covariance update formula.
//! - **Steady-state filter:** Fixed-gain observer obtained after covariance
//!   convergence.
//!
//! # Mathematical Formulation
//!
//! Linear estimators use:
//!
//! - prediction: `x^- = A x + B u`
//! - update: `x^+ = x^- + L (y - C x^- - D u)`
//!
//!
//! # Implementation Notes
//!
//! - Linear design and runtime live together so the returned gains can be used
//!   directly by convenience observers.
//! - Nonlinear runtime filters are intentionally kept in the embedded `alloc`
//!   lane rather than the public control namespace.
//!
//! # Feature Matrix
//!
//! | Feature | Continuous | Discrete | Linear |
//! | --- | --- | --- | --- |
//! | `LQE` / `DLQE` design | yes | yes | yes |
//! | Time-varying Kalman runtime | no | yes | yes |
//! | Fixed-gain observer runtime | yes | yes | yes |

pub mod linear;
pub use linear::{
    ContinuousObserver, ContinuousObserverDerivative, CovarianceUpdate, DiscreteKalmanFilter,
    EstimatorError, KalmanPrediction, KalmanUpdate, LqeSolve, SteadyStateKalmanFilter,
    SteadyStateKalmanPrediction, SteadyStateKalmanUpdate, dlqe_dense, lqe_dense,
    steady_state_filter_gain_dense,
};
