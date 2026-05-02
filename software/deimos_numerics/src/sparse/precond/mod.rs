//! Preconditioner adapters and simple concrete preconditioners for sparse solvers.
//!
//! This module re-exports faer's preconditioner traits and keeps project-local
//! concrete preconditioners in separate files. The current solver bridge lives
//! here as well so iterative methods can apply a preconditioner repeatedly
//! without allocating on every iteration.
//!
//! # Glossary
//!
//! - **Preconditioner:** Approximate inverse applied inside an iterative
//!   solver.
//! - **Scratch:** Temporary storage reserved once and reused across repeated
//!   applications.
//! - **`apply_in_place`:** Operation that overwrites the right-hand side with
//!   the preconditioned result.

mod block;
mod diagonal;
mod schur;

use super::col::copy_col;
use faer::Col;
use faer::Par;
use faer::dyn_stack::{MemBuffer, MemStack};
use faer_traits::ComplexField;

pub use block::{
    BlockDiagonalPrecond2, BlockPrecondError, BlockSplit2, BlockUpperTriangularPrecond2,
};
pub use diagonal::{DiagonalPrecond, DiagonalPrecondError};
pub use faer::matrix_free::{BiPrecond, IdentityPrecond, Precond};
pub use schur::SchurPrecond2;

#[inline]
pub(crate) fn precond_buffer<T, P>(precond: &P) -> MemBuffer
where
    T: ComplexField,
    P: Precond<T>,
{
    // Preconditioners advertise their scratch needs through faer's interface.
    // We allocate that storage once per solver and then reuse it across all
    // preconditioner applications inside the iteration.
    MemBuffer::new(precond.apply_in_place_scratch(1, Par::Seq))
}

#[inline]
pub(crate) fn apply_precond_to_col<T, P>(
    precond: &P,
    out: &mut Col<T>,
    rhs: &Col<T>,
    buffer: &mut MemBuffer,
) where
    T: ComplexField + Copy,
    P: Precond<T>,
{
    // The solver owns the buffer so repeated preconditioner application inside
    // the iteration does not allocate.
    copy_col(out, rhs);
    let stack = MemStack::new(buffer);
    precond.apply_in_place(out.as_mat_mut(), Par::Seq, stack);
}
