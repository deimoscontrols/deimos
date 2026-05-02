//! Control-systems algorithms built on the crate's dense and sparse numerics.
//!
//! The module is organized in layers:
//!
//! - `lti` provides model representations, analysis, and filtering
//! - `matrix_equations` provides reusable Lyapunov, Stein, and Riccati solvers
//! - `reduction` provides HSVD and balanced truncation
//! - `estimation` provides linear estimator design and runtime filtering
//! - `synthesis` provides controller design and practical runtime controllers
//!
//! The public surface is dense-first for higher-level model manipulation, with
//! selected sparse state-space workflows layered on top of the lower-level
//! sparse solver layers. Dense conversion, dense full-spectrum analysis, and
//! dense direct model manipulation remain the most complete paths.
//!
//! # Two Intuitions
//!
//! 1. **Workflow view.** This module is the path a controls user walks in
//!    practice: represent a plant, analyze it, identify or reduce it if
//!    needed, design estimators and controllers, then simulate and evaluate
//!    the closed loop.
//! 2. **Dependency view.** This module is also a stack of reusable numerical
//!    layers: matrix-equation solvers sit below reduction and synthesis;
//!    realization and identification sit beside state-space modeling; and the
//!    LTI layer provides the common representation surface above those kernels.
//!
//! # Glossary
//!
//! - **LTI:** Linear time-invariant.
//! - **Gramian:** Matrix measuring controllability or observability energy.
//! - **Riccati equation:** Matrix equation underlying LQR and Kalman design.
//! - **ERA / OKID:** Data-driven realization and identification algorithms.
//! - **HSVD / BT:** Hankel singular value decomposition and balanced
//!   truncation.
//! # Mathematical Formulation
//!
//! The dominant model form throughout the subsystem is state space:
//!
//! - continuous: `x' = A x + B u`, `y = C x + D u`
//! - discrete: `x[k+1] = A x[k] + B u[k]`, `y[k] = C x[k] + D u[k]`
//!
//! Higher-level algorithms are organized around that form:
//!
//! - estimation solves observer or Kalman problems for `A - L C`
//! - synthesis solves controller problems for `A - B K`
//! - reduction builds projection operators `V, W`
//! - realization and identification recover `(A, B, C, D)` from response data
//!
//! # Implementation Notes
//!
//! - Higher-level workflows are dense-first, even though sparse solver support
//!   exists underneath.
//! - Continuous and discrete time are distinct in the type system.
//! - The crate prefers explicit conversion and composition helpers over hidden
//!   coercions between representations.
//! - Public APIs generally return diagnostics together with the primary
//!   numerical result instead of hiding convergence quality.
//!
//! # Feature Matrix
//!
//! | Subsystem | Main purpose | Dense | Sparse | Continuous | Discrete |
//! | --- | --- | --- | --- | --- | --- |
//! | `lti` | representation, analysis, filtering | yes | partial | yes | yes |
//! | `matrix_equations` | Lyapunov / Stein / Riccati | yes | partial | yes | yes |
//! | `reduction` | HSVD and balanced truncation | yes | partial | yes | yes |
//! | `realization` | Markov and Hankel data structures | yes | n/a | no | yes |
//! | `identification` | ERA and OKID | yes | no | no | yes |
//! | `estimation` | LQE/Kalman/observer design | yes | no | partial | yes |
//! | `synthesis` | LQR/LQG/PID/pole placement | yes | no | yes | yes |

pub(crate) mod dense_ops;
pub mod estimation;
pub mod identification;
pub mod lti;
pub mod matrix_equations;
pub mod realization;
pub mod reduction;
pub mod synthesis;

pub use estimation::{
    ContinuousObserver, ContinuousObserverDerivative, CovarianceUpdate, DiscreteKalmanFilter,
    EstimatorError, KalmanPrediction, KalmanUpdate, LqeSolve, SteadyStateKalmanFilter,
    SteadyStateKalmanPrediction, SteadyStateKalmanUpdate, dlqe_dense, lqe_dense,
    steady_state_filter_gain_dense,
};
pub use identification::{
    EraError, EraInternals, EraInternalsLevel, EraParams, EraResult, OkidError, OkidParams,
    OkidResult, era_from_markov, era_from_shifted_hankel, okid,
};
pub use lti::state_space::{
    ContinuousStateSpace, ContinuousTime, ContinuousizationMethod, DiscreteStateSpace,
    DiscreteTime, DiscretizationMethod, ObserverControllerComposition, SparseContinuousStateSpace,
    SparseDiscreteStateSpace, SparseStateSpace, StateSpace, StateSpaceError,
};
pub use matrix_equations::{
    DenseLyapunovSolve, DenseSteinSolve, LowRankFactor, LowRankLyapunovSolve, LyapunovError,
    LyapunovParams, RiccatiError, RiccatiSolve, ShiftStrategy, SteinError, care_gain_from_solution,
    controllability_gramian_dense, controllability_gramian_discrete_dense,
    controllability_gramian_discrete_low_rank, controllability_gramian_low_rank,
    dare_gain_from_solution, observability_gramian_dense, observability_gramian_discrete_dense,
    observability_gramian_discrete_low_rank, observability_gramian_low_rank, solve_care_dense,
    solve_continuous_lyapunov_dense, solve_dare_dense, solve_discrete_stein_dense,
};
pub use realization::{
    BlockHankel, MarkovSequence, RealizationError, ShiftedBlockHankelPair, hankel_matrix_shape,
    max_square_era_block_dim, required_markov_len,
};
pub use reduction::{
    BalancedError, BalancedInternals, BalancedParams, BalancedTruncationResult, HsvdError,
    HsvdInternals, HsvdInternalsLevel, HsvdParams, HsvdResult, InternalsLevel,
    balanced_truncation_continuous_dense, balanced_truncation_continuous_low_rank,
    balanced_truncation_discrete_dense, balanced_truncation_discrete_low_rank,
    hsvd_from_dense_gramians, hsvd_from_factors,
};
pub use synthesis::{
    AntiWindup, FopdtModel, FrequencyPidDesign, FrequencyPidParams, LqgError, LqgSolve, LqrError,
    LqrSolve, OkidEraPidDesign, Pid, PidControllerKind, PidDesignError, PidError, PidOutput,
    PidState, PolePlacementError, PolePlacementSolve, ProcessFitResult, ProcessModelFitOptions,
    ProcessPidDesign, SampledIoData, SimcPidParams, SopdtModel, StepFitPidDesign,
    StepOptimizationPidDesign, StepOptimizationPidParams, StepResponseData,
    design_pid_from_continuous_state_space_frequency, design_pid_from_continuous_tf_frequency,
    design_pid_from_discrete_state_space_frequency,
    design_pid_from_discrete_state_space_step_optimization, design_pid_from_discrete_tf_frequency,
    design_pid_from_discrete_tf_step_optimization, design_pid_from_fopdt, design_pid_from_okid_era,
    design_pid_from_sopdt, design_pid_from_step_response_fopdt,
    design_pid_from_step_response_sopdt, dlqg_dense, dlqr_dense, dplace_observer_poles_dense,
    dplace_poles_dense, fit_fopdt_from_step_response, fit_fopdt_from_step_response_with_options,
    fit_sopdt_from_step_response, fit_sopdt_from_step_response_with_options, lqg_dense, lqr_dense,
    place_observer_poles_dense, place_poles_dense,
};
