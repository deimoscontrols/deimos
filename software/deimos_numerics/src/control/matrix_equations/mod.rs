//! Algebraic matrix-equation solvers used by higher-level control workflows.
//!
//! This module groups the low-level dense and sparse solver kernels that sit
//! below synthesis, estimation, and model-reduction routines:
//!
//! - [`lyapunov`] for continuous-time Lyapunov equations and Gramians
//! - [`stein`] for discrete-time Stein equations and Gramians
//! - [`riccati`] for continuous/discrete algebraic Riccati equations
//!
//! # Two Intuitions
//!
//! 1. **Equation-solver view.** These routines solve the matrix equations that
//!    appear repeatedly under higher-level control workflows.
//! 2. **Energy-and-optimality view.** The same equations define controllability
//!    and observability energy (Lyapunov/Stein) and optimal control or
//!    estimation tradeoffs (Riccati).
//!
//! # Glossary
//!
//! - **Lyapunov equation:** Continuous-time Gramian equation.
//! - **Stein equation:** Discrete-time Gramian equation.
//! - **CARE / DARE:** Continuous/discrete algebraic Riccati equations.
//! - **ADI:** Alternating-direction implicit iteration for low-rank solves.
//!
//! # Mathematical Formulation
//!
//! Core equations in this module are:
//!
//! - Lyapunov: `A X + X A^H + Q = 0`
//! - Stein: `X - A X A^H = Q`
//! - CARE / DARE: algebraic Riccati equations for optimal regulator and
//!   estimator design
//!
//! # Implementation Notes
//!
//! - Dense paths favor explicit, reference-style algorithms.
//! - Sparse paths focus on low-rank Gramian workflows rather than
//!   sparse Riccati solvers.
//! - These routines are intentionally reusable and avoid controller- or
//!   estimator-specific packaging.
//!
//! # Feature Matrix
//!
//! | Equation | Dense | Sparse / low-rank | Continuous | Discrete |
//! | --- | --- | --- | --- | --- |
//! | Lyapunov | yes | yes | yes | no |
//! | Stein | yes | yes | no | yes |
//! | Riccati | yes | no | yes | yes |

pub(super) fn vec_index(row: usize, col: usize, nrows: usize) -> usize {
    row + nrows * col
}

pub mod lyapunov;
pub mod riccati;
pub mod stein;

pub use lyapunov::{
    DenseLyapunovSolve, LowRankFactor, LowRankLyapunovSolve, LyapunovError, LyapunovParams,
    ShiftStrategy, controllability_gramian_dense, controllability_gramian_low_rank,
    observability_gramian_dense, observability_gramian_low_rank, solve_continuous_lyapunov_dense,
};
pub use riccati::{
    RiccatiError, RiccatiSolve, care_gain_from_solution, dare_gain_from_solution, solve_care_dense,
    solve_dare_dense,
};
pub use stein::{
    DenseSteinSolve, SteinError, controllability_gramian_discrete_dense,
    controllability_gramian_discrete_low_rank, observability_gramian_discrete_dense,
    observability_gramian_discrete_low_rank, solve_discrete_stein_dense,
};
