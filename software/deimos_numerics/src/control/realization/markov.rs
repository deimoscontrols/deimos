//! Discrete-time Markov-parameter sequences.
//!
//! # Two Intuitions
//!
//! 1. **Impulse-response view.** A Markov sequence is the system's
//!    impulse-response written one sample block at a time.
//! 2. **Identification-currency view.** It is the format that identification
//!    and realization algorithms exchange before they decide on any particular
//!    internal state coordinates.
//!
//! # Glossary
//!
//! - **Markov block `H_k`:** Output-by-input impulse-response block at lag `k`.
//! - **Direct term `D`:** Zeroth Markov block `H_0`.
//!
//! # Mathematical Formulation
//!
//! For a discrete-time state-space model,
//! `H_0 = D` and `H_k = C A^(k-1) B` for `k >= 1`.
//!
//! # Implementation Notes
//!
//! - The container enforces a single fixed block shape because later Hankel
//!   assembly depends on that rectangular block grid.
//! - Sparse and dense state-space helpers both materialize their Markov
//!   sequences into the same owned dense block representation.

use super::error::RealizationError;
use super::hankel::{BlockHankel, ShiftedBlockHankelPair};
use crate::control::lti::{DiscreteStateSpace, SparseDiscreteStateSpace};
use crate::sparse::compensated::CompensatedField;
use alloc::vec::Vec;
use faer::{Mat, MatRef};
use faer_traits::RealField;
use num_traits::Float;

/// Discrete-time Markov-parameter sequence.
///
/// For a discrete-time LTI system
///
/// `x[k+1] = A x[k] + B u[k]`
///
/// `y[k]   = C x[k] + D u[k]`
///
/// the blocks are
///
/// - `H_0 = D`
/// - `H_k = C A^(k-1) B` for `k >= 1`
///
/// Each block is stored as a dense `noutputs x ninputs` matrix. This is the
/// shared currency for data-driven realization methods such as ERA and OKID.
///
/// The convention here matches the standard discrete-time impulse-response
/// blocks used in Ho-Kalman-style realization and later ERA/OKID variants:
/// one dense block per sample, with the full output-by-input map retained at
/// each lag.
#[derive(Clone, Debug, PartialEq)]
pub struct MarkovSequence<T> {
    noutputs: usize,
    ninputs: usize,
    blocks: Vec<Mat<T>>,
}

impl<T> MarkovSequence<T> {
    /// Creates an empty Markov sequence with known block shape.
    ///
    /// This is mainly used when a caller requests zero retained Markov blocks
    /// from an otherwise well-formed system model.
    #[must_use]
    pub fn empty(noutputs: usize, ninputs: usize) -> Self {
        Self {
            noutputs,
            ninputs,
            blocks: Vec::new(),
        }
    }

    /// Builds a Markov sequence from owned dense blocks after validating that
    /// every block has the same `output x input` shape.
    ///
    /// The container is intentionally strict here because later block-Hankel
    /// assembly assumes a rectangular block grid with one fixed block shape.
    pub fn from_blocks(blocks: Vec<Mat<T>>) -> Result<Self, RealizationError> {
        let Some(first) = blocks.first() else {
            return Err(RealizationError::EmptySequence);
        };
        let noutputs = first.nrows();
        let ninputs = first.ncols();
        for (index, block) in blocks.iter().enumerate().skip(1) {
            if block.nrows() != noutputs || block.ncols() != ninputs {
                return Err(RealizationError::InconsistentBlockShape {
                    index,
                    expected_nrows: noutputs,
                    expected_ncols: ninputs,
                    actual_nrows: block.nrows(),
                    actual_ncols: block.ncols(),
                });
            }
        }
        Ok(Self {
            noutputs,
            ninputs,
            blocks,
        })
    }

    /// Number of stored Markov blocks.
    #[must_use]
    pub fn len(&self) -> usize {
        self.blocks.len()
    }

    /// Returns whether the sequence stores no blocks.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.blocks.is_empty()
    }

    /// Number of inputs represented by each block column dimension.
    #[must_use]
    pub fn ninputs(&self) -> usize {
        self.ninputs
    }

    /// Number of outputs represented by each block row dimension.
    #[must_use]
    pub fn noutputs(&self) -> usize {
        self.noutputs
    }

    /// Returns the `(noutputs, ninputs)` shape of each Markov block.
    #[must_use]
    pub fn block_shape(&self) -> (usize, usize) {
        (self.noutputs, self.ninputs)
    }

    /// Returns one Markov block by index.
    ///
    /// `block(0)` is the direct term `D`.
    #[must_use]
    pub fn block(&self, index: usize) -> MatRef<'_, T> {
        self.blocks[index].as_ref()
    }

    /// Returns one Markov block by index if it exists.
    #[must_use]
    pub fn get_block(&self, index: usize) -> Option<MatRef<'_, T>> {
        self.blocks.get(index).map(Mat::as_ref)
    }

    /// Returns all owned Markov blocks as a slice.
    #[must_use]
    pub fn blocks(&self) -> &[Mat<T>] {
        &self.blocks
    }

    /// Consumes the sequence and returns the owned dense blocks.
    #[must_use]
    pub fn into_blocks(self) -> Vec<Mat<T>> {
        self.blocks
    }

    /// Builds one dense block-Hankel matrix with configurable top-left block
    /// index.
    ///
    /// The top-left block is `H_start_index`, so the block at `(i, j)` is
    /// `H_{start_index + i + j}`.
    ///
    /// This is the canonical block layout used by Ho-Kalman-style realization
    /// and by ERA once the sequence is shifted to start at `H_1`.
    pub fn block_hankel(
        &self,
        start_index: usize,
        row_blocks: usize,
        col_blocks: usize,
    ) -> Result<BlockHankel<T>, RealizationError>
    where
        T: Copy,
    {
        BlockHankel::from_markov(self, start_index, row_blocks, col_blocks)
    }

    /// Builds the standard one-step-shifted ERA Hankel pair.
    ///
    /// This uses:
    ///
    /// - `H0` with top-left block `H_1`
    /// - `H1` with top-left block `H_2`
    ///
    /// This keeps the storage layer aligned with the textbook ERA convention
    /// so the eventual algorithm layer can work directly from the returned
    /// pair without reindexing.
    pub fn shifted_hankel_pair(
        &self,
        row_blocks: usize,
        col_blocks: usize,
    ) -> Result<ShiftedBlockHankelPair<T>, RealizationError>
    where
        T: Copy,
    {
        ShiftedBlockHankelPair::from_markov(self, row_blocks, col_blocks)
    }
}

impl<T> DiscreteStateSpace<T>
where
    T: CompensatedField,
    T::Real: Float + RealField,
{
    /// Returns the first `n_blocks` discrete-time Markov blocks.
    ///
    /// The returned sequence includes the direct term:
    ///
    /// - block `0`: `D`
    /// - block `k >= 1`: `C A^(k-1) B`
    ///
    /// The implementation intentionally delegates to the public impulse
    /// response path so the response layer and realization layer share one
    /// MIMO block convention.
    pub fn markov_parameters(&self, n_blocks: usize) -> MarkovSequence<T> {
        if n_blocks == 0 {
            return MarkovSequence::empty(self.noutputs(), self.ninputs());
        }
        MarkovSequence::from_blocks(self.impulse_response(n_blocks).values)
            .expect("discrete impulse-response blocks should always be shape-consistent")
    }
}

impl<T> SparseDiscreteStateSpace<T>
where
    T: CompensatedField,
    T::Real: Float + RealField,
{
    /// Returns the first `n_blocks` sparse discrete-time Markov blocks.
    ///
    /// The sparse state matrix is never densified; the sequence is generated
    /// through the existing sparse impulse-response path and stored as dense
    /// `noutputs x ninputs` blocks.
    ///
    /// That keeps the sparse model path aligned with the dense identification
    /// APIs, since ERA and OKID usually work with moderate dense Hankel
    /// matrices even when the source state operator is sparse.
    pub fn markov_parameters(&self, n_blocks: usize) -> MarkovSequence<T> {
        if n_blocks == 0 {
            return MarkovSequence::empty(self.noutputs(), self.ninputs());
        }
        MarkovSequence::from_blocks(self.impulse_response(n_blocks).values)
            .expect("sparse discrete impulse-response blocks should always be shape-consistent")
    }
}

#[cfg(test)]
mod tests {
    use super::MarkovSequence;
    use crate::control::lti::state_space::{DiscreteStateSpace, SparseDiscreteStateSpace};
    use faer::Mat;
    use faer::MatRef;
    use faer::sparse::{SparseColMat, Triplet};

    fn assert_mat_eq(lhs: MatRef<'_, f64>, rhs: MatRef<'_, f64>) {
        assert_eq!(lhs.nrows(), rhs.nrows());
        assert_eq!(lhs.ncols(), rhs.ncols());
        for row in 0..lhs.nrows() {
            for col in 0..lhs.ncols() {
                assert_eq!(lhs[(row, col)], rhs[(row, col)]);
            }
        }
    }

    fn scalar_system() -> DiscreteStateSpace<f64> {
        DiscreteStateSpace::new(
            Mat::from_fn(1, 1, |_, _| 2.0),
            Mat::from_fn(1, 1, |_, _| 3.0),
            Mat::from_fn(1, 1, |_, _| 5.0),
            Mat::from_fn(1, 1, |_, _| 7.0),
            1.0,
        )
        .unwrap()
    }

    #[test]
    fn markov_sequence_validates_block_shapes() {
        let err = MarkovSequence::from_blocks(vec![
            Mat::from_fn(2, 1, |row, _| row as f64),
            Mat::from_fn(1, 1, |_, _| 1.0),
        ])
        .unwrap_err();
        assert!(matches!(
            err,
            crate::control::realization::RealizationError::InconsistentBlockShape {
                index: 1,
                expected_nrows: 2,
                expected_ncols: 1,
                actual_nrows: 1,
                actual_ncols: 1,
            }
        ));
    }

    #[test]
    fn dense_markov_parameters_match_scalar_closed_form() {
        let seq = scalar_system().markov_parameters(4);
        let expected = [7.0, 15.0, 30.0, 60.0];
        assert_eq!(seq.len(), expected.len());
        for (index, &value) in expected.iter().enumerate() {
            assert_eq!(seq.block(index)[(0, 0)], value);
        }
    }

    #[test]
    fn dense_markov_parameters_match_small_mimo_reference() {
        let sys = DiscreteStateSpace::new(
            Mat::from_fn(2, 2, |row, col| match (row, col) {
                (0, 0) => 2.0,
                (1, 1) => 3.0,
                _ => 0.0,
            }),
            Mat::from_fn(2, 2, |row, col| if row == col { 1.0 } else { 0.0 }),
            Mat::from_fn(2, 2, |row, col| match (row, col) {
                (0, 0) => 4.0,
                (0, 1) => 5.0,
                (1, 0) => 6.0,
                (1, 1) => 7.0,
                _ => unreachable!(),
            }),
            Mat::from_fn(2, 2, |row, col| match (row, col) {
                (0, 0) => 8.0,
                (0, 1) => 9.0,
                (1, 0) => 10.0,
                (1, 1) => 11.0,
                _ => unreachable!(),
            }),
            0.5,
        )
        .unwrap();

        let seq = sys.markov_parameters(3);
        assert_mat_eq(seq.block(0), sys.d());
        assert_mat_eq(seq.block(1), sys.c());
        let expected_h2 = Mat::from_fn(2, 2, |row, col| match (row, col) {
            (0, 0) => 8.0,
            (0, 1) => 15.0,
            (1, 0) => 12.0,
            (1, 1) => 21.0,
            _ => unreachable!(),
        });
        assert_mat_eq(seq.block(2), expected_h2.as_ref());
    }

    #[test]
    fn sparse_markov_parameters_match_dense_reference() {
        let dense = scalar_system();
        let sparse_a =
            SparseColMat::<usize, f64>::try_new_from_triplets(1, 1, &[Triplet::new(0, 0, 2.0)])
                .unwrap();
        let sparse = SparseDiscreteStateSpace::new(
            sparse_a,
            Mat::from_fn(1, 1, |_, _| 3.0),
            Mat::from_fn(1, 1, |_, _| 5.0),
            Mat::from_fn(1, 1, |_, _| 7.0),
            1.0,
        )
        .unwrap();

        let dense_seq = dense.markov_parameters(5);
        let sparse_seq = sparse.markov_parameters(5);
        assert_eq!(dense_seq, sparse_seq);
    }
}
