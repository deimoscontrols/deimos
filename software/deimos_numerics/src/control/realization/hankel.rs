//! Block-Hankel assembly utilities for realization algorithms.
//!
//! # Two Intuitions
//!
//! 1. **Structured-matrix view.** The Hankel matrix is a rearrangement of the
//!    Markov sequence that makes time shifts explicit in its anti-diagonals.
//! 2. **Subspace view.** ERA and related methods use this structure because a
//!    low-rank factorization of the Hankel pair exposes the state subspace.
//!
//! # Glossary
//!
//! - **Anti-diagonal rule:** Block `(i, j)` stores `H_{start + i + j}`.
//! - **Shifted pair:** Two Hankel matrices whose top-left blocks differ by one
//!   Markov index.
//!
//! # Mathematical Formulation
//!
//! If the top-left block is `H_s`, then the Hankel matrix stores
//! `H_{s+i+j}` at block `(i, j)`. The shifted pair `(H_0, H_1)` is the basic
//! data object used by ERA.
//!
//! # Implementation Notes
//!
//! - The matrix is assembled densely because that is the right practical input
//!   to the current dense SVD-based realization path.
//! - The typed wrapper keeps the block geometry alongside the raw matrix so
//!   callers do not need to infer it later.

use super::error::RealizationError;
use super::markov::MarkovSequence;
use faer::{Mat, MatRef};

/// Dense block-Hankel matrix assembled from a discrete-time Markov sequence.
///
/// If the top-left block is `H_start_index`, then block `(i, j)` stores
/// `H_{start_index + i + j}`. The matrix is materialized densely because that
/// is the right trade for ERA/OKID support and keeps later SVD
/// calls straightforward.
///
/// For ERA specifically, the common choice is `start_index = 1`, so the first
/// block row begins with `H_1` rather than the direct term `H_0 = D`.
#[derive(Clone, Debug, PartialEq)]
pub struct BlockHankel<T> {
    start_index: usize,
    row_blocks: usize,
    col_blocks: usize,
    noutputs: usize,
    ninputs: usize,
    matrix: Mat<T>,
}

impl<T> BlockHankel<T> {
    /// Assembles a dense block-Hankel matrix from a validated Markov sequence.
    ///
    /// The builder preserves the explicit block layout in the returned type so
    /// higher-level realization code does not need to infer `(row_blocks,
    /// col_blocks, noutputs, ninputs)` back from the raw dense matrix shape.
    pub fn from_markov(
        sequence: &MarkovSequence<T>,
        start_index: usize,
        row_blocks: usize,
        col_blocks: usize,
    ) -> Result<Self, RealizationError>
    where
        T: Copy,
    {
        validate_hankel_request(sequence.len(), start_index, row_blocks, col_blocks)?;
        let (noutputs, ninputs) = sequence.block_shape();
        let (nrows, ncols) = hankel_matrix_shape(noutputs, ninputs, row_blocks, col_blocks);

        let matrix = Mat::from_fn(nrows, ncols, |row, col| {
            let block_row = row / noutputs;
            let row_in_block = row % noutputs;
            let block_col = col / ninputs;
            let col_in_block = col % ninputs;
            // Dense assembly is simple because each scalar entry belongs to
            // exactly one Markov block selected by the Hankel anti-diagonal
            // rule `start + i + j`.
            sequence.block(start_index + block_row + block_col)[(row_in_block, col_in_block)]
        });

        Ok(Self {
            start_index,
            row_blocks,
            col_blocks,
            noutputs,
            ninputs,
            matrix,
        })
    }

    /// Index of the top-left Markov block `H_start_index`.
    #[must_use]
    pub fn start_index(&self) -> usize {
        self.start_index
    }

    /// Number of output block rows.
    #[must_use]
    pub fn row_blocks(&self) -> usize {
        self.row_blocks
    }

    /// Number of input block columns.
    #[must_use]
    pub fn col_blocks(&self) -> usize {
        self.col_blocks
    }

    /// Number of outputs per Markov block row.
    #[must_use]
    pub fn noutputs(&self) -> usize {
        self.noutputs
    }

    /// Number of inputs per Markov block column.
    #[must_use]
    pub fn ninputs(&self) -> usize {
        self.ninputs
    }

    /// Dense assembled matrix view.
    #[must_use]
    pub fn matrix(&self) -> MatRef<'_, T> {
        self.matrix.as_ref()
    }
}

/// Standard one-step-shifted block-Hankel pair used by ERA.
///
/// `h0` starts at `H_1`, while `h1` starts at `H_2`.
///
/// This is the standard shifted pair used in both Ho-Kalman-style realization
/// formulas and the Juang-Pappa ERA construction.
#[derive(Clone, Debug, PartialEq)]
pub struct ShiftedBlockHankelPair<T> {
    h0: BlockHankel<T>,
    h1: BlockHankel<T>,
}

impl<T> ShiftedBlockHankelPair<T> {
    /// Builds the standard ERA block-Hankel pair from one Markov sequence.
    pub fn from_markov(
        sequence: &MarkovSequence<T>,
        row_blocks: usize,
        col_blocks: usize,
    ) -> Result<Self, RealizationError>
    where
        T: Copy,
    {
        validate_hankel_request(sequence.len(), 1, row_blocks, col_blocks)?;
        validate_hankel_request(sequence.len(), 2, row_blocks, col_blocks)?;
        Ok(Self {
            h0: BlockHankel::from_markov(sequence, 1, row_blocks, col_blocks)?,
            h1: BlockHankel::from_markov(sequence, 2, row_blocks, col_blocks)?,
        })
    }

    /// Unshifted ERA Hankel matrix whose top-left block is `H_1`.
    #[must_use]
    pub fn h0(&self) -> &BlockHankel<T> {
        &self.h0
    }

    /// One-step-shifted ERA Hankel matrix whose top-left block is `H_2`.
    #[must_use]
    pub fn h1(&self) -> &BlockHankel<T> {
        &self.h1
    }
}

/// Returns the dense matrix shape of a block-Hankel matrix with the requested
/// block layout.
///
/// This is a small bookkeeping helper, but centralizing it keeps later ERA
/// code from duplicating block-to-dense shape conversions.
#[must_use]
pub fn hankel_matrix_shape(
    noutputs: usize,
    ninputs: usize,
    row_blocks: usize,
    col_blocks: usize,
) -> (usize, usize) {
    (noutputs * row_blocks, ninputs * col_blocks)
}

/// Returns the minimum Markov-sequence length needed for a block-Hankel matrix
/// with the given top-left block index and block layout.
///
/// For a top-left block `H_start_index`, the bottom-right block is
/// `H_{start_index + row_blocks + col_blocks - 2}`.
///
/// The returned value is a sequence length, not the largest required index, so
/// it can be compared directly against `MarkovSequence::len()`.
#[must_use]
pub fn required_markov_len(start_index: usize, row_blocks: usize, col_blocks: usize) -> usize {
    start_index + row_blocks + col_blocks - 1
}

/// Returns the largest square ERA block dimension `q` such that both `H0` and
/// `H1` can be formed from a sequence of the given length.
///
/// Since `H0` and `H1` require indices through `H_{2q}`, the sequence must
/// contain at least `2q + 1` blocks including `H_0`.
///
/// This is the simplest admissibility rule for the common square-window ERA
/// case.
#[must_use]
pub fn max_square_era_block_dim(markov_len: usize) -> usize {
    markov_len.saturating_sub(1) / 2
}

/// Validates that a Markov sequence is long enough to support the requested
/// block-Hankel layout.
///
/// This keeps the indexing rule in one place so `BlockHankel` and
/// `ShiftedBlockHankelPair` cannot drift apart on their admissibility checks.
fn validate_hankel_request(
    sequence_len: usize,
    start_index: usize,
    row_blocks: usize,
    col_blocks: usize,
) -> Result<(), RealizationError> {
    if row_blocks == 0 {
        return Err(RealizationError::ZeroBlockCount {
            which: "row_blocks",
        });
    }
    if col_blocks == 0 {
        return Err(RealizationError::ZeroBlockCount {
            which: "col_blocks",
        });
    }
    let required = required_markov_len(start_index, row_blocks, col_blocks);
    if sequence_len < required {
        return Err(RealizationError::SequenceTooShort {
            available: sequence_len,
            required,
            start_index,
            row_blocks,
            col_blocks,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        BlockHankel, ShiftedBlockHankelPair, max_square_era_block_dim, required_markov_len,
    };
    use crate::control::realization::{MarkovSequence, RealizationError};
    use faer::Mat;

    fn scalar_markov(values: &[f64]) -> MarkovSequence<f64> {
        MarkovSequence::from_blocks(
            values
                .iter()
                .map(|&value| Mat::from_fn(1, 1, |_, _| value))
                .collect(),
        )
        .unwrap()
    }

    #[test]
    fn block_hankel_places_scalar_blocks_correctly() {
        let seq = scalar_markov(&[10.0, 11.0, 12.0, 13.0, 14.0]);
        let hankel = BlockHankel::from_markov(&seq, 1, 2, 3).unwrap();
        assert_eq!(hankel.start_index(), 1);
        assert_eq!(hankel.matrix()[(0, 0)], 11.0);
        assert_eq!(hankel.matrix()[(0, 1)], 12.0);
        assert_eq!(hankel.matrix()[(0, 2)], 13.0);
        assert_eq!(hankel.matrix()[(1, 0)], 12.0);
        assert_eq!(hankel.matrix()[(1, 1)], 13.0);
        assert_eq!(hankel.matrix()[(1, 2)], 14.0);
    }

    #[test]
    fn shifted_pair_is_one_step_shifted() {
        let seq = scalar_markov(&[10.0, 11.0, 12.0, 13.0, 14.0]);
        let pair = ShiftedBlockHankelPair::from_markov(&seq, 1, 2).unwrap();
        assert_eq!(pair.h0().start_index(), 1);
        assert_eq!(pair.h1().start_index(), 2);
        assert_eq!(pair.h0().matrix()[(0, 0)], 11.0);
        assert_eq!(pair.h0().matrix()[(0, 1)], 12.0);
        assert_eq!(pair.h1().matrix()[(0, 0)], 12.0);
        assert_eq!(pair.h1().matrix()[(0, 1)], 13.0);
    }

    #[test]
    fn shifted_pair_requires_enough_markov_blocks() {
        let seq = scalar_markov(&[1.0, 2.0, 3.0]);
        let err = ShiftedBlockHankelPair::from_markov(&seq, 2, 2).unwrap_err();
        assert_eq!(
            err,
            RealizationError::SequenceTooShort {
                available: 3,
                required: 4,
                start_index: 1,
                row_blocks: 2,
                col_blocks: 2,
            }
        );
    }

    #[test]
    fn era_block_helpers_match_length_constraint() {
        assert_eq!(required_markov_len(1, 2, 3), 5);
        assert_eq!(max_square_era_block_dim(0), 0);
        assert_eq!(max_square_era_block_dim(1), 0);
        assert_eq!(max_square_era_block_dim(5), 2);
        assert_eq!(max_square_era_block_dim(7), 3);
    }
}
