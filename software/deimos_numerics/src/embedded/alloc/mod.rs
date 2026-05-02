//! Dynamic-size `no_std + alloc` embedded runtimes.
//!
use faer::{Col, Mat};

pub mod estimation;

/// Heap-backed dense matrix used by the `embedded::alloc` estimators.
pub type Matrix<T> = Mat<T>;

/// Heap-backed dense column vector used by the `embedded::alloc` estimators.
pub type Vector<T> = Col<T>;
