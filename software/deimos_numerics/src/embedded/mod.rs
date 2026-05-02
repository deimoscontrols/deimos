//! Embedded-friendly runtime kernels.
//!
//! This module is a separate deployment-oriented lane alongside the broader
//! `control` stack. It is intentionally narrower:
//!
//! - `embedded::fixed` is fully fixed-size and allocation-free
//! - `embedded::alloc` is `no_std + alloc` for dynamic nonlinear estimators
//!
//! The goal is to provide runtime forms that map directly to embedded targets
//! without pulling in the analysis, design, plotting, or dynamic conversion
//! machinery from the `std`-only control module.
//!
pub mod error;
pub mod fixed;
pub(crate) mod math;

#[cfg(feature = "alloc")]
pub mod alloc;

pub use error::EmbeddedError;
