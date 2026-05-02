//! Sparse linear algebra wrappers and preconditioning utilities.
//!
//! # Two Intuitions
//!
//! 1. **Solver view.** This subsystem provides the direct and iterative sparse
//!    linear algebra needed by the control and decomposition layers.
//! 2. **Building-block view.** It also exposes the supporting pieces that make
//!    those solvers composable: compensated reductions, sparse matvec traits,
//!    block preconditioners, and Schur-complement operators.
//!
//! # Glossary
//!
//! - **CSC / CSR:** Sparse compressed-column / compressed-row storage.
//! - **Preconditioner:** Approximate inverse used inside an iterative solver.
//! - **Iterative refinement:** Residual-correction loop on top of a direct
//!   factorization.
//! - **Schur complement:** Reduced operator formed by eliminating one block of
//!   a block system.
//!
//! # Mathematical Formulation
//!
//! The major solver forms are:
//!
//! - direct factorizations `P A Q = L U` or sparse Cholesky variants
//! - iterative Krylov solve for `A x = b`
//! - block preconditioner applications that approximate `A^-1`
//!
//! # Implementation Notes
//!
//! - The sparse surface is designed so direct factorizations can also act as
//!   preconditioners.
//! - Compensated reductions are kept small and reusable so high-level modules
//!   can opt into improved residual checks.
//! - Most wrappers are staged around `faer`'s symbolic/numeric split.
//!
//! # Feature Matrix
//!
//! | Feature | Direct | Iterative | Preconditioning | Complex support |
//! | --- | --- | --- | --- | --- |
//! | LU | yes | refinement only | yes | yes |
//! | Cholesky | yes | no | yes | yes |
//! | BiCGSTAB | no | yes | yes | yes |
//! | Equilibration | n/a | assists | assists | yes |
//! | Block/Schur operators | n/a | assists | yes | yes |

/// BiCGSTAB iterative solver.
pub mod bicgstab;
/// Sparse Cholesky factorizations.
pub mod cholesky;
pub(crate) mod col;
/// Compensated complex and real accumulation helpers.
pub mod compensated;
/// Two-sided sparse matrix equilibration.
pub mod equilibration;
/// Sparse LU factorization and iterative refinement helpers.
pub mod lu;
/// Sparse matrix-vector multiplication traits and wrappers.
pub mod matvec;
/// Preconditioner traits and concrete sparse preconditioners.
pub mod precond;
/// Schur-complement operators for block sparse systems.
pub mod schur;

pub use bicgstab::{BiCGSTAB, BiCGSTABError, BiCGSTABSolveError};
pub use cholesky::{SparseLdlt, SparseLdltError, SparseLlt, SparseLltError};
pub use compensated::CompensatedField;
pub use equilibration::{Equilibration, EquilibrationError, EquilibrationParams};
pub use lu::{LuRefinementParams, RefinedLuSolve, SparseLu, SparseLuError};
pub use matvec::SparseMatVec;
pub use precond::{
    BiPrecond, BlockDiagonalPrecond2, BlockPrecondError, BlockSplit2, BlockUpperTriangularPrecond2,
    DiagonalPrecond, DiagonalPrecondError, IdentityPrecond, Precond, SchurPrecond2,
};
pub use schur::{SchurComplement2, SchurComplementError};
