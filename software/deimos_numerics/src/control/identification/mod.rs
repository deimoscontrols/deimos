//! System-identification algorithms built on the realization utilities.
//!
//! The identification layer sits above [`crate::control::realization`] and
//! turns either:
//!
//! - structured response data, such as Markov parameters, or
//! - sampled input/output data
//!
//! into discrete-time state-space models.
//!
//! # Two Intuitions
//!
//! 1. **Data-preparation view.** Identification here is the process of
//!    converting measured data into the impulse-response objects from which a
//!    realization can be built.
//! 2. **Subspace view.** The same workflow can be seen as building a low-rank
//!    subspace from Markov or Hankel data and then reading a state-space model
//!    out of that subspace.
//!
//! # Glossary
//!
//! - **Markov parameters:** Discrete impulse-response blocks `H_k`.
//! - **Hankel matrix:** Block matrix with time-shifted Markov blocks.
//! - **ERA:** Eigensystem realization algorithm.
//! - **OKID:** Observer/Kalman filter identification.
//!
//! # Mathematical Formulation
//!
//! OKID estimates a Markov sequence from sampled I/O data. ERA then takes a
//! shifted block-Hankel pair derived from that sequence and computes a reduced
//! state-space realization through an SVD and shifted subspace formulas.
//!
//! # Implementation Notes
//!
//! - The identification surface is discrete-time only.
//! - ERA is implemented both from Markov data and from an explicit shifted
//!   Hankel pair.
//! - OKID assumes the initial-condition transient is absent or has
//!   been trimmed away.
//!
//! # Feature Matrix
//!
//! | Algorithm | Input | Output | Time domain |
//! | --- | --- | --- | --- |
//! | `ERA` | Markov data or shifted Hankel pair | discrete state space | discrete |
//! | `OKID` | sampled input/output data | Markov sequence | discrete |

mod era;
mod okid;

pub use era::{
    EraError, EraInternals, EraInternalsLevel, EraParams, EraResult, era_from_markov,
    era_from_shifted_hankel,
};
pub use okid::{OkidError, OkidParams, OkidResult, okid};
