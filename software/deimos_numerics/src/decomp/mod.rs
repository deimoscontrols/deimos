//! Decomposition utilities built on top of `faer`'s dense and matrix-free
//! eigendecomposition / SVD backends.
//!
//! The decomposition front-ends in this module share a common result contract:
//!
//! - returned components are ordered by descending magnitude
//! - partial convergence is reported through [`DecompInfo`] rather than treated
//!   as an error
//! - compensated arithmetic is only used in the wrapper logic that this crate
//!   owns, such as compensated operator adapters and diagnostic checks
//!
//! The public API intentionally distinguishes between dense and sparse /
//! matrix-free use:
//!
//! - dense entry points may request the full decomposition
//! - sparse / matrix-free entry points always request a fixed number of leading
//!   components
//!
//! That split follows the actual backend capabilities. `faer` provides dense
//! full decompositions, but its sparse / matrix-free eigendecomposition and SVD
//! paths are restarted dominant-component solvers rather than full spectral
//! factorizations.
//!
//! # Two Intuitions
//!
//! 1. **Backend-normalization view.** This module hides the small but important
//!    differences between several decomposition backends and returns a single
//!    result contract to the rest of the crate.
//! 2. **Diagnostic view.** It is also the place where the crate recomputes
//!    residuals, orders components deterministically, and reports partial
//!    convergence instead of treating every backend imperfection as a fatal
//!    error.
//!
//! # Glossary
//!
//! - **Partial decomposition:** Only the dominant `k` components are computed.
//! - **Ritz value/vector:** Approximate eigenpair returned by a Krylov method.
//! - **Generalized eigenpair:** Pair `(alpha, beta)` representing
//!   `lambda = alpha / beta`.
//! - **BiLinOp / LinOp:** Matrix-free operator traits from `faer`.
//!
//! # Mathematical Formulation
//!
//! The module exposes:
//!
//! - eigendecomposition `A v = lambda v`
//! - generalized eigendecomposition `A v = lambda B v`
//! - singular-value decomposition `A = U Sigma V^H`
//!
//! with shared ordering and residual diagnostics over the returned dominant
//! window.
//!
//! # Implementation Notes
//!
//! - Dense wrappers may compute a full backend factorization and then truncate.
//! - Sparse wrappers are always dominant-component solvers.
//! - Compensated arithmetic is used in wrapper-owned diagnostics, not as a
//!   replacement for the backend kernels themselves.
//!
//! # Feature Matrix
//!
//! | Feature | Dense | Sparse / matrix-free | Self-adjoint | General |
//! | --- | --- | --- | --- | --- |
//! | Eigenvalues/eigenvectors | yes | yes | yes | yes |
//! | Generalized eigen | yes | no | n/a | yes |
//! | SVD | yes | yes | n/a | yes |

pub mod eigen;
pub mod operator;
pub mod svd;

pub use eigen::{
    dense_eigen, dense_eigenvalues, dense_generalized_eigen, dense_self_adjoint_eigen,
    sparse_eigen, sparse_eigen_scratch_req, sparse_eigen_with_scratch, sparse_self_adjoint_eigen,
    sparse_self_adjoint_eigen_scratch_req, sparse_self_adjoint_eigen_with_scratch,
};
pub use operator::{CompensatedApply, CompensatedBiApply, CompensatedBiLinOp, CompensatedLinOp};
pub use svd::{dense_svd, sparse_svd, sparse_svd_scratch_req, sparse_svd_with_scratch};

use crate::sparse::col::{col_slice, col_slice_mut};
use crate::sparse::compensated::{CompensatedField, dotc, norm2};
use alloc::vec::Vec;
use core::cmp::Ordering;
use core::fmt;
use faer::matrix_free::eigen::PartialEigenParams;
use faer::{Col, ColRef, Mat, MatRef, Unbind};
use faer_traits::ComplexField;
use faer_traits::ext::ComplexFieldExt;
use num_traits::Float;

/// Quality and convergence diagnostics returned alongside a decomposition.
///
/// The decomposition routines do not treat partial convergence as a hard error.
/// Instead, they return whatever components the backend produced together with
/// these diagnostics so callers can decide whether the result is good enough
/// for their workflow.
///
/// `max_residual_norm` and `max_orthogonality_error` are dimensionless wrapper
/// diagnostics over the returned component set.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DecompInfo<R> {
    /// Number of components requested by the caller after backend-specific
    /// target validation or clamping.
    pub n_requested: usize,
    /// Number of components reported as converged by the backend.
    pub n_converged: usize,
    /// Maximum residual norm over the returned components.
    ///
    /// For SVD this is the worse of `||A v - sigma u||` and
    /// `||A^H u - sigma v||`. For eigen it is `||A v - lambda v||`.
    pub max_residual_norm: R,
    /// Maximum orthogonality or norm drift error over the returned vectors.
    ///
    /// This combines norm drift and pairwise overlap checks so callers can
    /// quickly tell whether the returned basis vectors still behave like an
    /// orthonormal family.
    pub max_orthogonality_error: R,
}

impl<R: Float> DecompInfo<R> {
    /// Returns whether all requested components converged.
    #[must_use]
    pub fn fully_converged(&self) -> bool {
        self.n_converged >= self.n_requested
    }
}

/// Singular-value decomposition result.
///
/// The columns of `u` and `v` are ordered to match `s`, and `s` is sorted by
/// descending magnitude.
///
/// `u` has shape `(m, k)`, `s` has shape `(k, 1)`, and `v` has shape `(n, k)`.
#[derive(Clone, Debug, PartialEq)]
pub struct PartialSvd<T: ComplexField> {
    /// Left singular vectors, one per column.
    pub u: Mat<T>,
    /// Singular values ordered by descending magnitude.
    pub s: Col<T>,
    /// Right singular vectors, one per column.
    pub v: Mat<T>,
    /// Convergence and quality diagnostics.
    pub info: DecompInfo<T::Real>,
}

impl<T: ComplexField> PartialSvd<T> {
    /// Materializes the singular values as a diagonal matrix with shape
    /// `(k, k)`.
    #[must_use]
    pub fn sigma_as_diagonal(&self) -> Mat<T> {
        let n = self.s.nrows();
        let mut sigma = Mat::zeros(n, n);
        for i in 0..n {
            sigma[(i, i)] = self.s[i].clone();
        }
        sigma
    }
}

/// Eigendecomposition result.
///
/// The columns of `vectors` are ordered to match `values`, and `values` are
/// sorted by descending magnitude.
///
/// `values` has shape `(k, 1)` and `vectors` has shape `(n, k)`.
#[derive(Clone, Debug, PartialEq)]
pub struct PartialEigen<T: ComplexField> {
    /// Eigenvalues ordered by descending magnitude.
    pub values: Col<T>,
    /// Corresponding eigenvectors, one per column.
    pub vectors: Mat<T>,
    /// Convergence and quality diagnostics.
    pub info: DecompInfo<T::Real>,
}

impl<T: ComplexField> PartialEigen<T> {
    /// Materializes the eigenvalues as a diagonal matrix with shape `(k, k)`.
    #[must_use]
    pub fn values_as_diagonal(&self) -> Mat<T> {
        let n = self.values.nrows();
        let mut lambda = Mat::zeros(n, n);
        for i in 0..n {
            lambda[(i, i)] = self.values[i].clone();
        }
        lambda
    }
}

/// Generalized eigendecomposition result.
///
/// The columns of `vectors` are ordered to match the generalized eigenpairs
/// `(alpha, beta)`. The implied generalized eigenvalues are `alpha / beta`
/// wherever `beta` is nonzero.
///
/// `alpha` and `beta` each have shape `(k, 1)`, and `vectors` has shape
/// `(n, k)`.
#[derive(Clone, Debug, PartialEq)]
pub struct PartialGeneralizedEigen<T: ComplexField> {
    /// Generalized eigen numerator factors.
    pub alpha: Col<T>,
    /// Generalized eigen denominator factors.
    pub beta: Col<T>,
    /// Corresponding right generalized eigenvectors, one per column.
    pub vectors: Mat<T>,
    /// Convergence and quality diagnostics.
    pub info: DecompInfo<T::Real>,
}

/// Builder-style parameters for dense decomposition entry points.
///
/// Dense decomposition is allowed to request either the full factorization or
/// only a leading dominant window. When a partial dense backend is not a good
/// fit for the requested size, the dense wrappers may fall back to a full dense
/// factorization and then truncate the result.
///
/// `start_vector`, when supplied, has shape `(n, 1)`. `tol` is dimensionless.
#[derive(Clone, Debug, PartialEq)]
pub struct DenseDecompParams<T: ComplexField> {
    /// `None` requests the full dense decomposition, while `Some(k)` requests
    /// the leading `k` dominant components through the partial backend.
    pub n_components: Option<usize>,
    /// Convergence tolerance for partial backends.
    pub tol: T::Real,
    /// Optional minimum Krylov subspace dimension override.
    pub min_dim: Option<usize>,
    /// Optional maximum Krylov subspace dimension override.
    pub max_dim: Option<usize>,
    /// Maximum number of partial-solver restarts.
    pub max_restarts: usize,
    /// Optional user-provided deterministic start vector for partial backends.
    pub start_vector: Option<Col<T>>,
}

impl<T> Default for DenseDecompParams<T>
where
    T: crate::sparse::CompensatedField,
    T::Real: Float,
{
    fn default() -> Self {
        Self {
            n_components: None,
            tol: T::Real::epsilon().sqrt(),
            min_dim: None,
            max_dim: None,
            max_restarts: 1000,
            start_vector: None,
        }
    }
}

impl<T> DenseDecompParams<T>
where
    T: crate::sparse::CompensatedField,
    T::Real: Float,
{
    /// Creates parameters with documented defaults.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Requests either the full dense decomposition or the leading `k`
    /// components through the partial backend.
    #[must_use]
    pub fn with_n_components(mut self, n_components: Option<usize>) -> Self {
        self.n_components = n_components;
        self
    }

    /// Overrides the partial-backend convergence tolerance.
    #[must_use]
    pub fn with_tol(mut self, tol: T::Real) -> Self {
        self.tol = tol;
        self
    }

    /// Overrides the minimum Krylov subspace dimension.
    #[must_use]
    pub fn with_min_dim(mut self, min_dim: usize) -> Self {
        self.min_dim = Some(min_dim);
        self
    }

    /// Overrides the maximum Krylov subspace dimension.
    #[must_use]
    pub fn with_max_dim(mut self, max_dim: usize) -> Self {
        self.max_dim = Some(max_dim);
        self
    }

    /// Overrides the maximum number of restarts for partial backends.
    #[must_use]
    pub fn with_max_restarts(mut self, max_restarts: usize) -> Self {
        self.max_restarts = max_restarts;
        self
    }

    /// Supplies a user-chosen partial-backend start vector with shape `(n, 1)`.
    #[must_use]
    pub fn with_start_vector(mut self, start_vector: Col<T>) -> Self {
        self.start_vector = Some(start_vector);
        self
    }
}

/// Builder-style parameters for sparse / matrix-free partial decompositions.
///
/// Sparse and matrix-free backends are intentionally partial-only. The target
/// count is therefore required up front rather than expressed as `Option`.
///
/// `start_vector`, when supplied, has shape `(n, 1)`. `tol` is dimensionless.
#[derive(Clone, Debug, PartialEq)]
pub struct SparseDecompParams<T: ComplexField> {
    /// Number of dominant components to request.
    pub n_components: usize,
    /// Convergence tolerance for the partial backend.
    pub tol: T::Real,
    /// Optional minimum Krylov subspace dimension override.
    pub min_dim: Option<usize>,
    /// Optional maximum Krylov subspace dimension override.
    pub max_dim: Option<usize>,
    /// Maximum number of partial-solver restarts.
    pub max_restarts: usize,
    /// Optional user-provided deterministic start vector.
    pub start_vector: Option<Col<T>>,
}

impl<T> SparseDecompParams<T>
where
    T: crate::sparse::CompensatedField,
    T::Real: Float,
{
    /// Creates sparse decomposition parameters with the required target count.
    #[must_use]
    pub fn new(n_components: usize) -> Self {
        Self {
            n_components,
            tol: T::Real::epsilon().sqrt(),
            min_dim: None,
            max_dim: None,
            max_restarts: 1000,
            start_vector: None,
        }
    }

    /// Overrides the convergence tolerance.
    #[must_use]
    pub fn with_tol(mut self, tol: T::Real) -> Self {
        self.tol = tol;
        self
    }

    /// Overrides the minimum Krylov subspace dimension.
    #[must_use]
    pub fn with_min_dim(mut self, min_dim: usize) -> Self {
        self.min_dim = Some(min_dim);
        self
    }

    /// Overrides the maximum Krylov subspace dimension.
    #[must_use]
    pub fn with_max_dim(mut self, max_dim: usize) -> Self {
        self.max_dim = Some(max_dim);
        self
    }

    /// Overrides the maximum number of restarts.
    #[must_use]
    pub fn with_max_restarts(mut self, max_restarts: usize) -> Self {
        self.max_restarts = max_restarts;
        self
    }

    /// Supplies a user-chosen start vector with shape `(n, 1)`.
    #[must_use]
    pub fn with_start_vector(mut self, start_vector: Col<T>) -> Self {
        self.start_vector = Some(start_vector);
        self
    }
}

/// Errors produced by decomposition front-ends.
#[derive(Debug)]
pub enum DecompError {
    /// A supplied input had the wrong dimension.
    DimensionMismatch {
        /// Identifies the incompatible input object.
        which: &'static str,
        /// Required dimension.
        expected: usize,
        /// Actual supplied dimension.
        actual: usize,
    },
    /// The requested sparse target count is not valid for the operator.
    InvalidTarget {
        /// Requested number of target eigenpairs or singular triplets.
        requested: usize,
        /// Largest admissible target count for the operator.
        max: usize,
    },
    /// The caller supplied an all-zero start vector.
    ZeroStartVector,
    /// The dense SVD backend failed.
    DenseSvd(faer::linalg::solvers::SvdError),
    /// The dense eigendecomposition backend failed.
    DenseEvd(faer::linalg::solvers::EvdError),
    /// The dense generalized eigendecomposition backend failed.
    DenseGevd(faer::linalg::solvers::GevdError),
}

impl fmt::Display for DecompError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

impl core::error::Error for DecompError {}

impl From<faer::linalg::solvers::SvdError> for DecompError {
    fn from(value: faer::linalg::solvers::SvdError) -> Self {
        Self::DenseSvd(value)
    }
}

impl From<faer::linalg::solvers::EvdError> for DecompError {
    fn from(value: faer::linalg::solvers::EvdError) -> Self {
        Self::DenseEvd(value)
    }
}

impl From<faer::linalg::solvers::GevdError> for DecompError {
    fn from(value: faer::linalg::solvers::GevdError) -> Self {
        Self::DenseGevd(value)
    }
}

/// Builds the shared restarted-Krylov parameter block used by `faer`'s partial
/// eigendecomposition and SVD entry points.
pub(crate) fn partial_eigen_params<T: CompensatedField>(
    min_dim: Option<usize>,
    max_dim: Option<usize>,
    max_restarts: usize,
) -> PartialEigenParams
where
    T::Real: Float,
{
    PartialEigenParams {
        min_dim: min_dim.unwrap_or(0),
        max_dim: max_dim.unwrap_or(0),
        max_restarts,
        ..Default::default()
    }
}

/// Normalizes a user-provided start vector or synthesizes a deterministic
/// default one.
///
/// The normalization step is performed in this crate rather than assumed from
/// the caller so all sparse and dense partial front-ends start from a known
/// unit-norm vector.
pub(crate) fn normalized_start_vector<T>(
    start_vector: Option<&Col<T>>,
    len: usize,
) -> Result<Col<T>, DecompError>
where
    T: CompensatedField,
    T::Real: Float,
{
    let mut start = match start_vector {
        Some(start) => {
            if start.nrows() != len {
                return Err(DecompError::DimensionMismatch {
                    which: "start_vector",
                    expected: len,
                    actual: start.nrows(),
                });
            }
            Col::from_fn(len, |i| start[i.unbound()])
        }
        None => default_start_vector::<T>(len),
    };

    let norm = norm2(col_slice(&start));
    if norm == T::Real::zero() {
        return Err(DecompError::ZeroStartVector);
    }

    let norm_inv = norm.recip();
    for value in col_slice_mut(&mut start) {
        *value = value.mul_real(norm_inv);
    }
    Ok(start)
}

fn default_start_vector<T>(len: usize) -> Col<T>
where
    T: CompensatedField,
    T::Real: Float,
{
    // Use a deterministic alternating-sign pattern instead of hidden
    // randomness. That keeps results reproducible while avoiding the
    // pathological symmetry of an all-ones vector.
    let one = T::Real::one();
    let minus_one = -one;
    let half = one / (one + one);
    Col::from_fn(len, |i| {
        let idx = i.unbound();
        let real = if idx % 2 == 0 { one } else { minus_one };
        let imag = if idx % 3 == 0 { half } else { T::Real::zero() };
        T::from_real_imag(real, imag)
    })
}

/// Returns a permutation that orders scalar components by descending
/// magnitude.
pub(crate) fn sorted_order_descending_by_abs<T>(values: ColRef<'_, T>) -> Vec<usize>
where
    T: CompensatedField,
    T::Real: Float,
{
    let mut order: Vec<_> = (0..values.nrows()).collect();
    order.sort_by(|&lhs, &rhs| {
        let lhs = values[lhs].abs();
        let rhs = values[rhs].abs();
        rhs.partial_cmp(&lhs).unwrap_or(Ordering::Equal)
    });
    order
}

/// Applies a column-vector permutation.
pub(crate) fn permute_col<T: Clone>(values: ColRef<'_, T>, order: &[usize]) -> Col<T> {
    Col::from_fn(order.len(), |i| values[order[i.unbound()]].clone())
}

/// Applies the same permutation to the columns of a matrix.
pub(crate) fn permute_mat_cols<T: Clone>(matrix: MatRef<'_, T>, order: &[usize]) -> Mat<T> {
    Mat::from_fn(matrix.nrows(), order.len(), |i, j| {
        matrix[(i.unbound(), order[j.unbound()])].clone()
    })
}

/// Returns the worst norm-drift or pairwise-overlap error in `vectors`.
///
/// The decomposition wrappers use this as a lightweight sanity check on the
/// basis returned by the backend after sorting and truncation.
pub(crate) fn orthogonality_error<T>(vectors: MatRef<'_, T>) -> T::Real
where
    T: CompensatedField,
    T::Real: Float,
{
    let k = vectors.ncols();
    let mut max_error = T::Real::zero();

    for j in 0..k {
        let col_j = vectors.col(j).try_as_col_major().unwrap().as_slice();
        let norm_error = (norm2(col_j) - T::Real::one()).abs();
        if norm_error > max_error {
            max_error = norm_error;
        }
        for i in 0..j {
            let col_i = vectors.col(i).try_as_col_major().unwrap().as_slice();
            let overlap = dotc(col_i, col_j).abs();
            if overlap > max_error {
                max_error = overlap;
            }
        }
    }

    max_error
}
