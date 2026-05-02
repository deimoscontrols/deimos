//! Model-reduction building blocks and balanced front ends.
//!
//! This module groups the reduction-side layers:
//!
//! - [`hsvd`] for the reusable balancing core
//! - [`balanced`] for dense balanced realization and balanced truncation
//!
//! # Two Intuitions
//!
//! 1. **Compression view.** Reduction replaces a high-order model by a smaller
//!    one that keeps the input-output behavior users care about.
//! 2. **Energy view.** Balanced methods rank states by joint
//!    controllability/observability energy and discard weak directions first.
//!
//! # Glossary
//!
//! - **HSVD:** Hankel singular value decomposition.
//! - **Balanced truncation:** Projection-based model reduction built from HSVD.
//! - **Projection matrices:** Left/right maps used to assemble the reduced
//!   model.
//!
//! # Mathematical Formulation
//!
//! Balanced reduction works by finding coordinates where controllability and
//! observability Gramians are simultaneously diagonal. Balanced realization
//! keeps the full numerical rank, while balanced truncation discards the
//! smallest Hankel singular directions.
//!
//! # Implementation Notes
//!
//! - HSVD is exposed as a reusable middle layer rather than being buried
//!   inside balanced truncation.
//! - Dense and low-rank workflows share the same truncation and diagnostic
//!   policy.
//! - Standard balanced truncation assumes asymptotic stability of the input
//!   model.
//!
//! # Feature Matrix
//!
//! | Feature | Dense continuous | Dense discrete | Sparse / low-rank continuous | Sparse / low-rank discrete |
//! | --- | --- | --- | --- | --- |
//! | HSVD core | yes | yes | yes | yes |
//! | Balanced realization | yes | yes | no | no |
//! | Balanced truncation | yes | yes | yes | yes |

pub mod balanced;
pub mod hsvd;

pub use balanced::{
    BalancedError, BalancedInternals, BalancedParams, BalancedRealizationResult,
    BalancedTruncationResult, InternalsLevel, balanced_realization_continuous_dense,
    balanced_realization_discrete_dense, balanced_truncation_continuous_dense,
    balanced_truncation_continuous_low_rank, balanced_truncation_discrete_dense,
    balanced_truncation_discrete_low_rank,
};
pub use hsvd::{
    HsvdError, HsvdInternals, HsvdInternalsLevel, HsvdParams, HsvdResult, hsvd_from_dense_gramians,
    hsvd_from_factors,
};
