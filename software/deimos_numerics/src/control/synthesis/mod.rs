//! Controller synthesis and practical controller runtime layers.
//!
//! This module groups:
//!
//! - [`lqr`] and [`lqg`] for optimal controller design
//! - [`pole_placement`] for classical dense pole assignment
//! - [`pid`] and [`pid_design`] for PID runtime control and tuning workflows
//!
//! # Two Intuitions
//!
//! 1. **Design-output view.** This is the layer that returns objects users can
//!    actually deploy: gains, controller realizations, and runtime controller
//!    state machines.
//! 2. **Method-family view.** It also groups several design traditions:
//!    optimal control (`LQR`, `LQG`), classical pole shaping, and practical
//!    PID tuning and runtime control.
//!
//! # Glossary
//!
//! - **State feedback:** Control law `u = -K x`.
//! - **LQG:** Regulator plus observer packaged into one dynamic controller.
//! - **PIDF:** PID with a filtered derivative term.
//! - **Pole placement:** Direct assignment of desired closed-loop eigenvalues.
//!
//! # Mathematical Formulation
//!
//! The main synthesis outputs target one of two closed-loop matrices:
//!
//! - controller side: `A - B K`
//! - observer side: `A - L C`
//!
//! or package both together in a dynamic compensator driven by the measured
//! outputs.
//!
//! # Implementation Notes
//!
//! - `LQR` and `LQG` are thin workflow layers on top of lower-level Riccati and
//!   estimator routines.
//! - Pole placement is dense-first and still conservative for MIMO targets.
//! - PID support spans both runtime control and model/data-driven tuning.
//!
//! # Feature Matrix
//!
//! | Feature | Continuous | Discrete | SISO | MIMO |
//! | --- | --- | --- | --- | --- |
//! | `LQR` / `DLQR` | yes | yes | yes | yes |
//! | `LQG` / `DLQG` | yes | yes | yes | yes |
//! | PID runtime | sampled only | yes | yes | no |
//! | PID design | yes | yes | yes | no |
//! | Pole placement | yes | yes | yes | partial |

pub mod lqg;
pub mod lqr;
pub mod pid;
pub mod pid_design;
pub mod pole_placement;

pub use lqg::{LqgError, LqgSolve, dlqg_dense, lqg_dense};
pub use lqr::{LqrError, LqrSolve, dlqr_dense, lqr_dense};
pub use pid::{AntiWindup, Pid, PidError, PidOutput, PidState};
pub use pid_design::{
    FopdtModel, FrequencyPidDesign, FrequencyPidParams, OkidEraPidDesign, PidControllerKind,
    PidDesignError, ProcessFitResult, ProcessModelFitOptions, ProcessPidDesign, SampledIoData,
    SimcPidParams, SopdtModel, StepFitPidDesign, StepOptimizationPidDesign,
    StepOptimizationPidParams, StepResponseData, design_pid_from_continuous_state_space_frequency,
    design_pid_from_continuous_tf_frequency, design_pid_from_discrete_state_space_frequency,
    design_pid_from_discrete_state_space_step_optimization, design_pid_from_discrete_tf_frequency,
    design_pid_from_discrete_tf_step_optimization, design_pid_from_fopdt, design_pid_from_okid_era,
    design_pid_from_sopdt, design_pid_from_step_response_fopdt,
    design_pid_from_step_response_sopdt, fit_fopdt_from_step_response,
    fit_fopdt_from_step_response_with_options, fit_sopdt_from_step_response,
    fit_sopdt_from_step_response_with_options,
};
pub use pole_placement::{
    PolePlacementError, PolePlacementSolve, dplace_observer_poles_dense, dplace_poles_dense,
    place_observer_poles_dense, place_poles_dense,
};
