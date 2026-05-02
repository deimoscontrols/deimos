//! Observer/Kalman filter identification (OKID).
//!
//! # Two Intuitions
//!
//! 1. **Regression view.** OKID is a structured linear regression that solves
//!    for observer-Markov blocks from measured input/output data.
//! 2. **Impulse-recovery view.** The same computation is a way to peel the
//!    system's impulse-response sequence out of general forced-response data so
//!    a realization method like ERA can use it next.
//!
//! # Glossary
//!
//! - **Observer-Markov block:** Regression coefficient block combining input
//!   and output history.
//! - **Markov block:** One discrete impulse-response block `H_k`.
//! - **Regression horizon:** Number of observer-history lags used.
//!
//! # Mathematical Formulation
//!
//! OKID forms a lifted regression whose unknowns are observer-Markov blocks
//! and then algebraically recovers the ordinary system Markov sequence from
//! those fitted blocks.
//!
//! # Implementation Notes
//!
//! - The implementation assumes a discrete-time LTI data-generating
//!   model.
//! - The solve uses a dense SVD-based pseudoinverse path so rank-deficiency is
//!   detected explicitly instead of being hidden in a normal-equations solve.
//! - Rank acceptance is policy-driven. The default allows structurally
//!   deficient regressions that still carry useful Markov information, while
//!   stricter modes can require a minimum retained rank or full row rank.

use crate::control::dense_ops::dense_mul;
use crate::control::realization::MarkovSequence;
use crate::decomp::{DecompError, DenseDecompParams, PartialSvd, dense_svd};
use crate::sparse::compensated::CompensatedField;
use alloc::vec::Vec;
use core::fmt;
use faer::{Col, Mat, MatRef};
use faer_traits::ext::ComplexFieldExt;
use num_traits::{Float, Zero};

/// Builder-style parameters for OKID.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct OkidParams {
    /// Number of system Markov blocks to recover, including `H_0 = D`.
    pub n_markov: usize,
    /// Number of observer-Markov lags used in the regression.
    pub observer_order: usize,
    /// Rank-acceptance policy used for the lifted regression SVD.
    pub rank_policy: OkidRankPolicy,
}

/// Acceptance policy for the effective numerical rank of the OKID regression.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum OkidRankPolicy {
    /// Accept any regression with at least one retained singular direction.
    ///
    /// This is the default because exact noiseless data from small systems can
    /// produce structurally rank-deficient lifted regressions while still
    /// determining the recovered Markov sequence usefully.
    #[default]
    AllowDeficient,
    /// Require at least the requested number of retained singular directions.
    RequireAtLeast(usize),
    /// Require full numerical row rank in the lifted regression.
    RequireFullRowRank,
}

impl OkidParams {
    /// Creates OKID parameters with the required Markov horizon and observer
    /// order.
    ///
    /// The default rank policy is `OkidRankPolicy::AllowDeficient`.
    #[must_use]
    pub fn new(n_markov: usize, observer_order: usize) -> Self {
        Self {
            n_markov,
            observer_order,
            rank_policy: OkidRankPolicy::default(),
        }
    }

    /// Returns a copy of the parameters with an explicit regression-rank policy.
    #[must_use]
    pub fn with_rank_policy(mut self, rank_policy: OkidRankPolicy) -> Self {
        self.rank_policy = rank_policy;
        self
    }
}

/// Result of OKID Markov estimation.
#[derive(Clone, Debug)]
pub struct OkidResult<T> {
    /// Recovered discrete-time Markov sequence.
    pub markov: MarkovSequence<T>,
    /// Estimated direct-feedthrough block `D`.
    pub direct_feedthrough: Mat<T>,
    /// Estimated observer Markov blocks for lags `1..=observer_order`.
    ///
    /// Each block has shape `noutputs x (ninputs + noutputs)` and is partitioned
    /// as `[Y_u(i) | Y_y(i)]`.
    pub observer_markov_blocks: Vec<Mat<T>>,
}

/// Errors produced by OKID.
#[derive(Debug)]
pub enum OkidError {
    /// Input and output data did not have compatible matrix dimensions.
    DimensionMismatch {
        /// Identifies the incompatible matrix.
        which: &'static str,
        /// Required row count.
        expected_nrows: usize,
        /// Required column count.
        expected_ncols: usize,
        /// Actual row count supplied by the caller.
        actual_nrows: usize,
        /// Actual column count supplied by the caller.
        actual_ncols: usize,
    },
    /// The requested Markov horizon was zero.
    InvalidMarkovCount,
    /// The requested observer order was zero.
    InvalidObserverOrder,
    /// There were not enough time samples for the requested observer order.
    NotEnoughSamples {
        /// Number of available time samples.
        samples: usize,
        /// Requested observer-Markov regression depth.
        observer_order: usize,
    },
    /// Dense SVD failed while solving the regression.
    Decomposition(DecompError),
    /// The regression matrix was numerically rank-deficient.
    RankDeficientRegression,
    /// A recovered quantity contained a non-finite entry.
    NonFiniteResult {
        /// Identifies the recovered quantity with the non-finite entry.
        which: &'static str,
    },
}

impl fmt::Display for OkidError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

impl core::error::Error for OkidError {}

impl From<DecompError> for OkidError {
    fn from(value: DecompError) -> Self {
        Self::Decomposition(value)
    }
}

/// Estimates a discrete-time Markov sequence from input/output data using the
/// OKID observer-Markov regression.
///
/// The implementation assumes the supplied data are compatible with the
/// linear time-invariant discrete-time model used by the regression and that
/// the initial-condition effect has either decayed or been trimmed away.
pub fn okid<T>(
    outputs: MatRef<'_, T>,
    inputs: MatRef<'_, T>,
    params: &OkidParams,
) -> Result<OkidResult<T>, OkidError>
where
    T: CompensatedField,
    T::Real: Float,
{
    validate_okid_inputs(outputs, inputs, params)?;
    let observer =
        observer_markov_regression(outputs, inputs, params.observer_order, params.rank_policy)?;
    let direct_feedthrough = first_columns(observer.as_ref(), inputs.nrows());
    let observer_markov_blocks =
        observer_blocks(observer.as_ref(), outputs.nrows(), inputs.nrows());
    let markov = recover_markov_sequence(
        direct_feedthrough.as_ref(),
        &observer_markov_blocks,
        params.n_markov,
    )?;

    Ok(OkidResult {
        markov,
        direct_feedthrough,
        observer_markov_blocks,
    })
}

fn validate_okid_inputs<T>(
    outputs: MatRef<'_, T>,
    inputs: MatRef<'_, T>,
    params: &OkidParams,
) -> Result<(), OkidError>
where
    T: CompensatedField,
    T::Real: Float,
{
    if params.n_markov == 0 {
        return Err(OkidError::InvalidMarkovCount);
    }
    if params.observer_order == 0 {
        return Err(OkidError::InvalidObserverOrder);
    }
    if outputs.ncols() != inputs.ncols() {
        return Err(OkidError::DimensionMismatch {
            which: "outputs.ncols",
            expected_nrows: outputs.nrows(),
            expected_ncols: inputs.ncols(),
            actual_nrows: outputs.nrows(),
            actual_ncols: outputs.ncols(),
        });
    }
    if outputs.ncols() <= params.observer_order {
        return Err(OkidError::NotEnoughSamples {
            samples: outputs.ncols(),
            observer_order: params.observer_order,
        });
    }
    Ok(())
}

fn observer_markov_regression<T>(
    outputs: MatRef<'_, T>,
    inputs: MatRef<'_, T>,
    observer_order: usize,
    rank_policy: OkidRankPolicy,
) -> Result<Mat<T>, OkidError>
where
    T: CompensatedField,
    T::Real: Float,
{
    let noutputs = outputs.nrows();
    let ninputs = inputs.nrows();
    let nsamples = inputs.ncols();
    let regressor_rows = ninputs + observer_order * (ninputs + noutputs);
    let ncols = nsamples - observer_order;

    let phi = Mat::from_fn(regressor_rows, ncols, |row, col| {
        let k = observer_order + col;
        if row < ninputs {
            return inputs[(row, k)];
        }
        let offset = row - ninputs;
        let lag = offset / (ninputs + noutputs) + 1;
        let inner = offset % (ninputs + noutputs);
        let sample = k - lag;
        if inner < ninputs {
            inputs[(inner, sample)]
        } else {
            outputs[(inner - ninputs, sample)]
        }
    });
    let y = Mat::from_fn(noutputs, ncols, |row, col| {
        outputs[(row, observer_order + col)]
    });
    let phi_pinv = pseudo_inverse(phi.as_ref(), rank_policy)?;
    let theta = dense_mul(y.as_ref(), phi_pinv.as_ref());
    check_finite(theta.as_ref(), "observer_markov_regression")?;
    Ok(theta)
}

fn recover_markov_sequence<T>(
    direct_feedthrough: MatRef<'_, T>,
    observer_markov_blocks: &[Mat<T>],
    n_markov: usize,
) -> Result<MarkovSequence<T>, OkidError>
where
    T: CompensatedField,
    T::Real: Float,
{
    let noutputs = direct_feedthrough.nrows();
    let ninputs = direct_feedthrough.ncols();
    let observer_order = observer_markov_blocks.len();
    let mut blocks = Vec::with_capacity(n_markov);
    blocks.push(direct_feedthrough.to_owned());

    for k in 1..n_markov {
        let mut h_k = Mat::<T>::zeros(noutputs, ninputs);
        if k <= observer_order {
            let y_u = first_columns(observer_markov_blocks[k - 1].as_ref(), ninputs);
            h_k = &h_k + &y_u;
        }
        for i in 1..=k.min(observer_order) {
            let y_y = trailing_columns(observer_markov_blocks[i - 1].as_ref(), noutputs);
            let term = dense_mul(y_y.as_ref(), blocks[k - i].as_ref());
            h_k = &h_k + &term;
        }
        check_finite(h_k.as_ref(), "markov_sequence")?;
        blocks.push(h_k);
    }

    Ok(MarkovSequence::from_blocks(blocks)
        .expect("recovered OKID blocks should be shape-consistent"))
}

fn observer_blocks<T>(theta: MatRef<'_, T>, noutputs: usize, ninputs: usize) -> Vec<Mat<T>>
where
    T: Copy,
{
    let block_width = ninputs + noutputs;
    let observer_order = (theta.ncols() - ninputs) / block_width;
    (0..observer_order)
        .map(|i| {
            let start = ninputs + i * block_width;
            Mat::from_fn(noutputs, block_width, |row, col| theta[(row, start + col)])
        })
        .collect()
}

fn pseudo_inverse<T>(
    matrix: MatRef<'_, T>,
    rank_policy: OkidRankPolicy,
) -> Result<Mat<T>, OkidError>
where
    T: CompensatedField,
    T::Real: Float,
{
    let (svd, retained, _tol) = checked_svd_rank(matrix, rank_policy)?;
    build_pseudo_inverse(svd, retained)
}

fn checked_svd_rank<T>(
    matrix: MatRef<'_, T>,
    rank_policy: OkidRankPolicy,
) -> Result<(PartialSvd<T>, usize, T::Real), OkidError>
where
    T: CompensatedField,
    T::Real: Float,
{
    let svd = dense_svd(matrix, &DenseDecompParams::<T>::new())?;
    let singular_values = Col::from_fn(svd.s.nrows(), |i| svd.s[i].abs());
    let mut max_sigma = <T::Real as Zero>::zero();
    for i in 0..singular_values.nrows() {
        if singular_values[i] > max_sigma {
            max_sigma = singular_values[i];
        }
    }
    let tol = T::Real::epsilon().sqrt() * max_sigma;
    let retained = (0..singular_values.nrows())
        .filter(|&i| singular_values[i] > tol)
        .count();
    if retained == 0 {
        return Err(OkidError::RankDeficientRegression);
    }
    match rank_policy {
        OkidRankPolicy::AllowDeficient => {}
        OkidRankPolicy::RequireAtLeast(min_rank) if retained < min_rank => {
            return Err(OkidError::RankDeficientRegression);
        }
        OkidRankPolicy::RequireFullRowRank if retained != matrix.nrows() => {
            return Err(OkidError::RankDeficientRegression);
        }
        _ => {}
    }
    Ok((svd, retained, tol))
}

fn build_pseudo_inverse<T>(svd: PartialSvd<T>, retained: usize) -> Result<Mat<T>, OkidError>
where
    T: CompensatedField,
    T::Real: Float,
{
    let v_r = Mat::from_fn(svd.v.nrows(), retained, |row, col| svd.v[(row, col)]);
    let u_r_h = Mat::from_fn(retained, svd.u.nrows(), |row, col| svd.u[(col, row)].conj());
    let singular_values = Col::from_fn(svd.s.nrows(), |i| svd.s[i].abs());
    let sigma_inv = Mat::from_fn(retained, retained, |row, col| {
        if row == col {
            T::one().mul_real(singular_values[row].recip())
        } else {
            T::zero()
        }
    });
    Ok(dense_mul(
        dense_mul(v_r.as_ref(), sigma_inv.as_ref()).as_ref(),
        u_r_h.as_ref(),
    ))
}

fn first_columns<T: Copy>(matrix: MatRef<'_, T>, ncols: usize) -> Mat<T> {
    Mat::from_fn(matrix.nrows(), ncols, |row, col| matrix[(row, col)])
}

fn trailing_columns<T: Copy>(matrix: MatRef<'_, T>, ncols: usize) -> Mat<T> {
    let start = matrix.ncols() - ncols;
    Mat::from_fn(matrix.nrows(), ncols, |row, col| matrix[(row, start + col)])
}

fn check_finite<T>(matrix: MatRef<'_, T>, which: &'static str) -> Result<(), OkidError>
where
    T: CompensatedField,
    T::Real: Float,
{
    for col in 0..matrix.ncols() {
        for row in 0..matrix.nrows() {
            let value = matrix[(row, col)];
            if !value.real().is_finite() || !value.imag().is_finite() {
                return Err(OkidError::NonFiniteResult { which });
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{OkidError, OkidParams, OkidRankPolicy, okid};
    use crate::control::identification::{EraParams, era_from_markov};
    use crate::control::lti::state_space::DiscreteStateSpace;
    use faer::{Mat, MatRef};

    fn assert_close(lhs: MatRef<'_, f64>, rhs: MatRef<'_, f64>, tol: f64) {
        assert_eq!(lhs.nrows(), rhs.nrows());
        assert_eq!(lhs.ncols(), rhs.ncols());
        for col in 0..lhs.ncols() {
            for row in 0..lhs.nrows() {
                let err = (lhs[(row, col)] - rhs[(row, col)]).abs();
                assert!(
                    err <= tol,
                    "entry ({row}, {col}) differs: lhs={}, rhs={}, err={err}, tol={tol}",
                    lhs[(row, col)],
                    rhs[(row, col)],
                );
            }
        }
    }

    fn scalar_system() -> DiscreteStateSpace<f64> {
        DiscreteStateSpace::new(
            Mat::from_fn(4, 4, |row, col| match (row, col) {
                (0, 0) => 0.72,
                (0, 1) => 0.08,
                (0, 2) => 0.0,
                (0, 3) => 0.0,
                (1, 0) => -0.05,
                (1, 1) => 0.58,
                (1, 2) => 0.11,
                (1, 3) => 0.0,
                (2, 0) => 0.0,
                (2, 1) => -0.04,
                (2, 2) => 0.41,
                (2, 3) => 0.09,
                (3, 0) => 0.0,
                (3, 1) => 0.0,
                (3, 2) => -0.03,
                (3, 3) => 0.27,
                _ => unreachable!(),
            }),
            Mat::from_fn(4, 1, |row, _| [1.0, -0.45, 0.3, 0.15][row]),
            Mat::from_fn(1, 4, |_, col| [1.1, -0.8, 0.55, 0.25][col]),
            Mat::from_fn(1, 1, |_, _| 0.2),
            0.1,
        )
        .unwrap()
    }

    #[test]
    fn okid_recovers_scalar_markov_sequence_from_noiseless_data() {
        let sys = scalar_system();
        let inputs = Mat::from_fn(1, 64, |_, col| {
            let a = (((17 * col + 5) % 31) as f64 - 15.0) / 7.0;
            let b = (((29 * col + 11) % 37) as f64 - 18.0) / 11.0;
            a + b
        });
        let sim = sys
            .simulate(&[0.0, 0.0, 0.0, 0.0], inputs.as_ref())
            .unwrap();
        let result = okid(
            sim.outputs.as_ref(),
            sim.inputs.as_ref(),
            &OkidParams::new(8, 4),
        )
        .unwrap();

        let expected = sys.markov_parameters(8);
        for k in 0..expected.len() {
            assert_close(result.markov.block(k), expected.block(k), 1.0e-9);
        }
        assert_eq!(result.observer_markov_blocks.len(), 4);
    }

    #[test]
    fn okid_handles_small_mimo_data() {
        let sys = DiscreteStateSpace::new(
            Mat::from_fn(4, 4, |row, col| match (row, col) {
                (0, 0) => 0.63,
                (0, 1) => 0.07,
                (0, 2) => 0.02,
                (0, 3) => 0.0,
                (1, 0) => -0.06,
                (1, 1) => 0.54,
                (1, 2) => 0.08,
                (1, 3) => 0.01,
                (2, 0) => 0.0,
                (2, 1) => -0.05,
                (2, 2) => 0.46,
                (2, 3) => 0.09,
                (3, 0) => 0.0,
                (3, 1) => 0.0,
                (3, 2) => -0.04,
                (3, 3) => 0.31,
                _ => unreachable!(),
            }),
            Mat::from_fn(4, 2, |row, col| match (row, col) {
                (0, 0) => 1.0,
                (0, 1) => -0.35,
                (1, 0) => 0.2,
                (1, 1) => 0.75,
                (2, 0) => -0.15,
                (2, 1) => 0.45,
                (3, 0) => 0.08,
                (3, 1) => 0.25,
                _ => unreachable!(),
            }),
            Mat::from_fn(2, 4, |row, col| match (row, col) {
                (0, 0) => 1.0,
                (0, 1) => 0.15,
                (0, 2) => -0.25,
                (0, 3) => 0.1,
                (1, 0) => -0.2,
                (1, 1) => 0.95,
                (1, 2) => 0.18,
                (1, 3) => -0.12,
                _ => unreachable!(),
            }),
            Mat::from_fn(2, 2, |row, col| if row == col { 0.35 } else { 0.05 }),
            0.2,
        )
        .unwrap();

        let inputs = Mat::from_fn(2, 128, |row, col| {
            if row == 0 {
                ((((19 * col + 1) % 41) as f64 - 20.0) / 9.0)
                    + ((((7 * col + 3) % 17) as f64 - 8.0) / 13.0)
            } else {
                ((((23 * col + 2) % 43) as f64 - 21.0) / 10.0)
                    + ((((11 * col + 5) % 19) as f64 - 9.0) / 7.0)
            }
        });
        let sim = sys
            .simulate(&[0.0, 0.0, 0.0, 0.0], inputs.as_ref())
            .unwrap();
        let result = okid(
            sim.outputs.as_ref(),
            sim.inputs.as_ref(),
            &OkidParams::new(8, 5),
        )
        .unwrap();
        let expected = sys.markov_parameters(8);
        for k in 0..expected.len() {
            assert_close(result.markov.block(k), expected.block(k), 1.0e-7);
        }
    }

    #[test]
    fn okid_to_era_pipeline_recovers_response() {
        let sys = scalar_system();
        let inputs = Mat::from_fn(1, 96, |_, col| {
            let a = (((13 * col + 2) % 29) as f64 - 14.0) / 6.0;
            let b = (((31 * col + 7) % 47) as f64 - 23.0) / 15.0;
            a + b
        });
        let sim = sys
            .simulate(&[0.0, 0.0, 0.0, 0.0], inputs.as_ref())
            .unwrap();
        let identified = okid(
            sim.outputs.as_ref(),
            sim.inputs.as_ref(),
            &OkidParams::new(10, 5),
        )
        .unwrap();
        let era =
            era_from_markov(&identified.markov, 3, 3, &EraParams::new(sys.sample_time())).unwrap();
        let recovered = era.realized.markov_parameters(10);
        let expected = sys.markov_parameters(10);
        for k in 0..expected.len() {
            assert_close(recovered.block(k), expected.block(k), 1.0e-4);
        }
    }

    #[test]
    fn okid_allows_partially_rank_deficient_regression_by_default() {
        let inputs = Mat::from_fn(1, 12, |_, _| 1.0f64);
        let outputs = Mat::from_fn(1, 12, |_, _| 2.0f64);

        let result = okid(outputs.as_ref(), inputs.as_ref(), &OkidParams::new(6, 4)).unwrap();
        assert_eq!(result.markov.len(), 6);
    }

    #[test]
    fn okid_rejects_partially_rank_deficient_regression_with_full_row_rank_policy() {
        let inputs = Mat::from_fn(1, 12, |_, _| 1.0f64);
        let outputs = Mat::from_fn(1, 12, |_, _| 2.0f64);

        let err = okid(
            outputs.as_ref(),
            inputs.as_ref(),
            &OkidParams::new(6, 4).with_rank_policy(OkidRankPolicy::RequireFullRowRank),
        )
        .unwrap_err();
        assert!(matches!(err, OkidError::RankDeficientRegression));
    }

    #[test]
    fn okid_rejects_partially_rank_deficient_regression_with_minimum_rank_policy() {
        let inputs = Mat::from_fn(1, 12, |_, _| 1.0f64);
        let outputs = Mat::from_fn(1, 12, |_, _| 2.0f64);

        let err = okid(
            outputs.as_ref(),
            inputs.as_ref(),
            &OkidParams::new(6, 4).with_rank_policy(OkidRankPolicy::RequireAtLeast(4)),
        )
        .unwrap_err();
        assert!(matches!(err, OkidError::RankDeficientRegression));
    }

    #[test]
    fn okid_rejects_too_few_samples() {
        let outputs = Mat::<f64>::zeros(1, 3);
        let inputs = Mat::<f64>::zeros(1, 3);
        let err = okid(outputs.as_ref(), inputs.as_ref(), &OkidParams::new(4, 3)).unwrap_err();
        assert!(matches!(
            err,
            OkidError::NotEnoughSamples {
                samples: 3,
                observer_order: 3
            }
        ));
    }
}
