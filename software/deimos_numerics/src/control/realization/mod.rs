//! Reusable realization-building utilities shared by identification methods.
//!
//! This layer sits between:
//!
//! - known or estimated discrete-time Markov parameters
//! - higher-level realization algorithms such as ERA and OKID
//!
//! The implementation keeps the core deliberately small:
//!
//! - [`MarkovSequence`] stores the block sequence `H_0, H_1, ...`
//! - [`BlockHankel`] assembles dense block-Hankel matrices from that sequence
//! - [`ShiftedBlockHankelPair`] builds the standard ERA pair `(H0, H1)`
//!
//! The module is discrete-time focused. Continuous-time impulse responses are
//! functions of time rather than finite recurrence blocks, so a continuous
//! Markov abstraction would need an explicit sampling policy that belongs in a
//! later layer.
//!
//! In this context, a "realization" means a specific state-space model
//! `(A, B, C, D)` whose external input-output behavior matches some other
//! system description such as:
//!
//! - Markov parameters
//! - an impulse response
//! - a transfer function
//! - measured input/output data
//!
//! Realizations are not unique. If `(A, B, C, D)` is one realization and `T`
//! is any invertible state-coordinate change, then
//!
//! - `A' = T A T^-1`
//! - `B' = T B`
//! - `C' = C T^-1`
//! - `D' = D`
//!
//! is a different realization of the same external system. Identification and
//! realization algorithms therefore recover a state-space representation only
//! up to such internal coordinate changes. The role of the utilities in this
//! module is to build the structured data objects, such as Markov sequences
//! and shifted block-Hankel matrices, from which those equivalent
//! state-space models can later be constructed.
//!
//! Literature:
//!
//! - Juang and Pappa, "An Eigensystem Realization Algorithm for Modal
//!   Parameter Identification and Model Reduction," Journal of Guidance,
//!   Control, and Dynamics, 1985.
//! - Juang, Phan, Horta, and Longman, "Identification of Observer/Kalman
//!   Filter Markov Parameters: Theory and Experiments," Journal of Guidance,
//!   Control, and Dynamics, 1993.
//! - Van Overschee and De Moor, *Subspace Identification for Linear Systems*,
//!   Kluwer, 1996.
//! - Brunton and Kutz, *Data-Driven Science and Engineering*, 2nd ed.,
//!   Cambridge University Press, 2022, especially the realization and
//!   identification material in Section 9.3.
//!
//! # Two Intuitions
//!
//! 1. **Data-structure view.** This module packages the discrete response data
//!    that realization algorithms manipulate before they ever produce a state
//!    matrix.
//! 2. **Shared-currency view.** It is also the common layer between
//!    simulation-derived Markov parameters, OKID output, and ERA input.
//!
//! # Glossary
//!
//! - **Markov parameter:** Discrete impulse-response block.
//! - **Block-Hankel matrix:** Dense matrix formed by arranging Markov blocks on
//!   anti-diagonals.
//! - **Shifted Hankel pair:** Two Hankel matrices offset by one Markov index.
//!
//! # Mathematical Formulation
//!
//! For a discrete-time state-space system, the Markov sequence is
//! `H_0 = D`, `H_k = C A^(k-1) B`, and Hankel matrices are built by arranging
//! those blocks into a structured dense matrix that exposes shift invariance.
//!
//! # Implementation Notes
//!
//! - The module is intentionally representation-focused; it does not attempt to
//!   realize a state-space model by itself.
//! - Dense block assembly is used because that is the right practical trade for
//!   the current ERA/OKID workflow.

mod error;
mod hankel;
mod markov;

pub use error::RealizationError;
pub use hankel::{
    BlockHankel, ShiftedBlockHankelPair, hankel_matrix_shape, max_square_era_block_dim,
    required_markov_len,
};
pub use markov::MarkovSequence;
