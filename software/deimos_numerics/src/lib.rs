#![doc = include_str!("../README.md")]
#![no_std]
#![allow(
    clippy::large_enum_variant,
    clippy::manual_memcpy,
    clippy::needless_range_loop,
    clippy::result_large_err,
    clippy::too_many_arguments,
    clippy::type_complexity
)]

#[cfg(feature = "alloc")]
#[macro_use]
extern crate alloc;

#[cfg(test)]
extern crate std;

#[cfg(feature = "alloc")]
pub mod control;
#[cfg(feature = "alloc")]
pub mod decomp;
/// Embedded-friendly runtime control and estimation kernels.
pub mod embedded;
#[cfg(feature = "alloc")]
/// Internal scalar arithmetic helpers shared across the crate.
pub(crate) mod scalar;
#[cfg(feature = "alloc")]
/// Sparse direct solvers, iterative solvers, and preconditioners.
pub mod sparse;
/// Accurate floating-point summation utilities.
pub mod twosum;
