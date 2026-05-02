//! Fixed-size runtime LTI kernels.
//!
//! This module intentionally contains execution-oriented kernels rather than
//! design-facing alternate representations. In particular:
//!
//! - fixed FIR and fractional-delay filters are provided directly
//! - fixed state-space and PID runtimes are provided directly
//! - fixed delta-SOS is provided as the embedded IIR execution form
//!
//! There is intentionally no fixed ordinary SOS storage type here yet. The
//! expected workflow is to design or store IIR filters in the alloc-side
//! control layer, then convert to delta-SOS before deploying into the fixed
//! embedded runtime.
//!
//! # Glossary
//!
//! - **Execution form:** Runtime-oriented representation chosen for numerical
//!   robustness or deployment convenience.
//! - **Delta-SOS:** Delta-operator second-order-section runtime form.
//! - **Fractional delay:** FIR filter approximating a non-integer sample delay.

mod delta_sos;
mod fir;
pub mod fractional_delay;
mod pid;
mod state_space;

pub use delta_sos::{DeltaSection, DeltaSos, DeltaSosState};
pub use fir::{Fir, FirState};
pub use fractional_delay::{lagrange_fractional_delay, lagrange_fractional_delay_taps};
pub use pid::{AntiWindup, Pid, PidOutput, PidState};
pub use state_space::{DiscreteStateSpace, Matrix, Vector};
