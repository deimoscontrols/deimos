use core::fmt;

/// Errors produced by Markov-sequence and block-Hankel utilities.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RealizationError {
    /// A Markov sequence built from owned blocks needs at least one block so
    /// its input/output shape can be inferred.
    EmptySequence,
    /// One Markov block had a different `output x input` shape from the rest
    /// of the sequence.
    InconsistentBlockShape {
        /// Index of the first inconsistent block.
        index: usize,
        /// Required row count.
        expected_nrows: usize,
        /// Required column count.
        expected_ncols: usize,
        /// Actual row count in the inconsistent block.
        actual_nrows: usize,
        /// Actual column count in the inconsistent block.
        actual_ncols: usize,
    },
    /// A block-Hankel builder was given zero row or column block count.
    ZeroBlockCount {
        /// Identifies whether the zero count was for rows or columns.
        which: &'static str,
    },
    /// The Markov sequence is too short for the requested Hankel layout.
    SequenceTooShort {
        /// Number of available Markov blocks.
        available: usize,
        /// Number of blocks required by the requested layout.
        required: usize,
        /// Starting Markov index requested for the Hankel build.
        start_index: usize,
        /// Requested Hankel row-block count.
        row_blocks: usize,
        /// Requested Hankel column-block count.
        col_blocks: usize,
    },
}

impl fmt::Display for RealizationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

impl core::error::Error for RealizationError {}
