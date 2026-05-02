//! Sampled root-locus data helpers.
//!
//! The implementation is intentionally plotting-oriented:
//!
//! - it samples closed-loop poles on a caller-supplied gain grid
//! - it tracks branches by nearest-neighbor continuation between adjacent gain
//!   samples
//! - it does not attempt exact breakaway, asymptote, or branch-point solves
//!
//! # Two Intuitions
//!
//! 1. **Controller-gain view.** Root locus answers how the closed-loop poles
//!    move as a scalar loop gain is increased.
//! 2. **Parameterized-polynomial view.** The same object is a family of roots
//!    of `D(s) + k N(s)` or `D(z) + k N(z)` sampled across `k`.
//!
//! # Glossary
//!
//! - **Branch:** One heuristically tracked pole trajectory across gains.
//! - **Open-loop poles/zeros:** Roots of the plant/controller transfer map
//!   before closing the loop.
//! - **Breakaway:** Gain where branches meet or split; not solved exactly by
//!   this helper.
//!
//! # Mathematical Formulation
//!
//! For a SISO open-loop transfer `L = N / D`, unity negative-feedback
//! closed-loop poles satisfy:
//!
//! - continuous/discrete alike: `D + k N = 0`
//!
//! The implementation samples those roots on a gain grid and then performs
//! nearest-neighbor continuation to obtain branch-like trajectories.
//!
//! # Implementation Notes
//!
//! - Branch tracking is heuristic and should be interpreted as plotting help,
//!   not as a proof of topological branch identity.
//! - Branch samples are stored as optional poles so proper-but-not-strictly-
//!   proper loops can represent gains where the closed-loop polynomial drops
//!   degree and a finite pole disappears.
//! - The helper intentionally stays SISO because scalar loop gain is the
//!   classical root-locus setting.

use super::util::{poly_add_aligned, poly_roots};
use super::{
    ContinuousSos, ContinuousStateSpace, ContinuousTransferFunction, ContinuousZpk, DiscreteSos,
    DiscreteStateSpace, DiscreteTransferFunction, DiscreteZpk, LtiError,
};
use alloc::vec::Vec;
use faer::complex::Complex;
use faer_traits::RealField;
use faer_traits::ext::ComplexFieldExt;
use num_traits::Float;

/// One tracked root-locus branch across a sampled gain grid.
#[derive(Clone, Debug, PartialEq)]
pub struct RootLocusBranch<R> {
    /// Closed-loop poles assigned to this branch at each sampled gain.
    ///
    /// `None` marks gains where the closed-loop characteristic polynomial
    /// drops degree and the corresponding branch has no finite pole.
    pub poles: Vec<Option<Complex<R>>>,
}

/// Sampled root-locus data for a SISO open-loop transfer.
#[derive(Clone, Debug, PartialEq)]
pub struct RootLocusData<R> {
    /// Nonnegative scalar gains used to close the unity-feedback loop.
    pub gains: Vec<R>,
    /// Closed-loop poles at each sampled gain before any branch interpretation.
    pub poles: Vec<Vec<Complex<R>>>,
    /// Heuristically tracked pole branches across the sampled gain grid.
    pub branches: Vec<RootLocusBranch<R>>,
}

impl<R> ContinuousTransferFunction<R>
where
    R: Float + RealField,
{
    /// Samples the unity negative-feedback root locus of the open-loop
    /// transfer on the supplied gain grid.
    pub fn root_locus_data(&self, gains: &[R]) -> Result<RootLocusData<R>, LtiError> {
        root_locus_from_transfer(self.numerator(), self.denominator(), gains)
    }
}

impl<R> DiscreteTransferFunction<R>
where
    R: Float + RealField,
{
    /// Samples the unity negative-feedback root locus of the open-loop
    /// transfer on the supplied gain grid.
    pub fn root_locus_data(&self, gains: &[R]) -> Result<RootLocusData<R>, LtiError> {
        root_locus_from_transfer(self.numerator(), self.denominator(), gains)
    }
}

impl<R> ContinuousZpk<R>
where
    R: Float + RealField,
{
    /// Samples the unity negative-feedback root locus on the supplied gain
    /// grid.
    pub fn root_locus_data(&self, gains: &[R]) -> Result<RootLocusData<R>, LtiError> {
        self.to_transfer_function()?.root_locus_data(gains)
    }
}

impl<R> DiscreteZpk<R>
where
    R: Float + RealField,
{
    /// Samples the unity negative-feedback root locus on the supplied gain
    /// grid.
    pub fn root_locus_data(&self, gains: &[R]) -> Result<RootLocusData<R>, LtiError> {
        self.to_transfer_function()?.root_locus_data(gains)
    }
}

impl<R> ContinuousSos<R>
where
    R: Float + RealField,
{
    /// Samples the unity negative-feedback root locus on the supplied gain
    /// grid.
    pub fn root_locus_data(&self, gains: &[R]) -> Result<RootLocusData<R>, LtiError> {
        self.to_transfer_function()?.root_locus_data(gains)
    }
}

impl<R> DiscreteSos<R>
where
    R: Float + RealField,
{
    /// Samples the unity negative-feedback root locus on the supplied gain
    /// grid.
    pub fn root_locus_data(&self, gains: &[R]) -> Result<RootLocusData<R>, LtiError> {
        self.to_transfer_function()?.root_locus_data(gains)
    }
}

impl<R> ContinuousStateSpace<R>
where
    R: Float + RealField,
{
    /// Samples the unity negative-feedback root locus of the represented SISO
    /// loop transfer.
    pub fn root_locus_data(&self, gains: &[R]) -> Result<RootLocusData<R>, LtiError> {
        self.to_transfer_function()?.root_locus_data(gains)
    }
}

impl<R> DiscreteStateSpace<R>
where
    R: Float + RealField,
{
    /// Samples the unity negative-feedback root locus of the represented SISO
    /// loop transfer.
    pub fn root_locus_data(&self, gains: &[R]) -> Result<RootLocusData<R>, LtiError> {
        self.to_transfer_function()?.root_locus_data(gains)
    }
}

fn root_locus_from_transfer<R>(
    numerator: &[R],
    denominator: &[R],
    gains: &[R],
) -> Result<RootLocusData<R>, LtiError>
where
    R: Float + RealField,
{
    validate_gain_grid(gains)?;
    let mut poles = Vec::with_capacity(gains.len());

    for &gain in gains {
        // For unity negative feedback, the closed-loop poles solve
        // `D(s) + k N(s) = 0`.
        let closed_loop_denominator = poly_add_aligned(denominator, &scale_poly(numerator, gain));
        let roots = sort_roots(poly_roots(&closed_loop_denominator)?);
        poles.push(roots);
    }

    let max_root_count = poles.iter().map(Vec::len).max().unwrap_or(0);
    let mut branches = (0..max_root_count)
        .map(|_| RootLocusBranch {
            poles: Vec::with_capacity(gains.len()),
        })
        .collect::<Vec<_>>();
    for roots in &poles {
        assign_roots_to_branches(&mut branches, roots);
    }

    Ok(RootLocusData {
        gains: gains.to_vec(),
        poles,
        branches,
    })
}

fn validate_gain_grid<R>(gains: &[R]) -> Result<(), LtiError>
where
    R: Float + RealField,
{
    if gains
        .iter()
        .any(|&gain| !gain.is_finite() || gain < R::zero())
    {
        return Err(LtiError::InvalidSamplePoint {
            which: "root_locus_data",
        });
    }
    if gains.windows(2).any(|window| window[1] < window[0]) {
        return Err(LtiError::InvalidSampleGrid {
            which: "root_locus_data",
        });
    }
    Ok(())
}

fn scale_poly<R>(coeffs: &[R], gain: R) -> Vec<R>
where
    R: Float + RealField,
{
    coeffs.iter().map(|&value| value * gain).collect()
}

fn sort_roots<R>(mut roots: Vec<Complex<R>>) -> Vec<Complex<R>>
where
    R: Float + RealField,
{
    // Keep the per-gain spectrum deterministic before the branch tracker does
    // its nearest-neighbor assignment.
    roots.sort_by(|lhs, rhs| {
        rhs.abs()
            .partial_cmp(&lhs.abs())
            .unwrap_or(core::cmp::Ordering::Equal)
            .then_with(|| {
                rhs.re
                    .partial_cmp(&lhs.re)
                    .unwrap_or(core::cmp::Ordering::Equal)
            })
            .then_with(|| {
                rhs.im
                    .partial_cmp(&lhs.im)
                    .unwrap_or(core::cmp::Ordering::Equal)
            })
    });
    roots
}

fn assign_roots_to_branches<R>(branches: &mut [RootLocusBranch<R>], roots: &[Complex<R>])
where
    R: Float + RealField,
{
    if branches.is_empty() {
        return;
    }

    let next_len = branches[0].poles.len() + 1;
    let mut used = vec![false; roots.len()];
    for branch in branches.iter_mut() {
        let Some(prev) = branch.poles.iter().rev().find_map(|&pole| pole) else {
            continue;
        };
        let mut best_idx = None;
        let mut best_dist = R::infinity();
        for (idx, &root) in roots.iter().enumerate() {
            if used[idx] {
                continue;
            }
            // This is intentionally heuristic: branch continuity comes from the
            // supplied gain grid, not from an exact eigenvalue-continuation
            // solve.
            let dist = (root - prev).abs();
            if dist < best_dist {
                best_dist = dist;
                best_idx = Some(idx);
            }
        }
        if let Some(idx) = best_idx {
            used[idx] = true;
            branch.poles.push(Some(roots[idx]));
        } else {
            branch.poles.push(None);
        }
    }

    let mut unused_roots = roots
        .iter()
        .enumerate()
        .filter_map(|(idx, &root)| (!used[idx]).then_some(root));
    for branch in branches.iter_mut() {
        if branch.poles.len() == next_len {
            continue;
        }
        branch
            .poles
            .push(unused_roots.next().map(Some).unwrap_or(None));
    }
}

#[cfg(test)]
mod tests {
    use super::RootLocusData;
    use crate::control::lti::ContinuousTransferFunction;
    use nalgebra::ComplexField;

    fn assert_close(lhs: f64, rhs: f64, tol: f64) {
        let err = (lhs - rhs).abs();
        assert!(err <= tol, "lhs={lhs}, rhs={rhs}, err={err}, tol={tol}");
    }

    fn assert_root_locus_close(lhs: &RootLocusData<f64>, rhs: &RootLocusData<f64>, tol: f64) {
        assert_eq!(lhs.gains, rhs.gains);
        assert_eq!(lhs.poles.len(), rhs.poles.len());
        for (lhs_roots, rhs_roots) in lhs.poles.iter().zip(rhs.poles.iter()) {
            assert_eq!(lhs_roots.len(), rhs_roots.len());
            for (&lhs_root, &rhs_root) in lhs_roots.iter().zip(rhs_roots.iter()) {
                assert!((lhs_root - rhs_root).abs() <= tol);
            }
        }
        assert_eq!(lhs.branches.len(), rhs.branches.len());
        for (lhs_branch, rhs_branch) in lhs.branches.iter().zip(rhs.branches.iter()) {
            assert_eq!(lhs_branch.poles.len(), rhs_branch.poles.len());
            for (lhs_pole, rhs_pole) in lhs_branch.poles.iter().zip(rhs_branch.poles.iter()) {
                match (lhs_pole, rhs_pole) {
                    (Some(lhs_pole), Some(rhs_pole)) => {
                        assert!((*lhs_pole - *rhs_pole).abs() <= tol);
                    }
                    (None, None) => {}
                    _ => panic!("root-locus branch occupancy mismatch"),
                }
            }
        }
    }

    #[test]
    fn root_locus_matches_scalar_closed_loop_poles() {
        let loop_tf =
            ContinuousTransferFunction::continuous(vec![1.0], vec![1.0, 1.0, 0.0]).unwrap();
        let gains = vec![0.0, 1.0, 4.0];
        let locus = loop_tf.root_locus_data(&gains).unwrap();

        assert_eq!(locus.poles[0].len(), 2);
        assert_close(locus.poles[1][0].re, -0.5, 1.0e-8);
        assert_close(locus.poles[1][1].re, -0.5, 1.0e-8);
        assert_close(locus.poles[1][0].im.abs(), (3.0f64).sqrt() / 2.0, 1.0e-8);
        assert_close(locus.poles[2][0].re, -0.5, 1.0e-8);
        assert_close(locus.poles[2][1].re, -0.5, 1.0e-8);
        assert_close(locus.poles[2][0].im.abs(), (15.0f64).sqrt() / 2.0, 1.0e-8);
    }

    #[test]
    fn root_locus_matches_across_representations() {
        let tf = ContinuousTransferFunction::continuous(vec![2.0], vec![1.0, 3.0, 2.0]).unwrap();
        let zpk = tf.to_zpk().unwrap();
        let sos = tf.to_sos().unwrap();
        let ss = tf.to_state_space().unwrap();
        let gains = vec![0.0, 0.5, 1.0, 2.0, 5.0];

        let tf_data = tf.root_locus_data(&gains).unwrap();
        let zpk_data = zpk.root_locus_data(&gains).unwrap();
        let sos_data = sos.root_locus_data(&gains).unwrap();
        let ss_data = ss.root_locus_data(&gains).unwrap();

        assert_root_locus_close(&tf_data, &zpk_data, 1.0e-10);
        assert_root_locus_close(&tf_data, &sos_data, 1.0e-10);
        assert_root_locus_close(&tf_data, &ss_data, 1.0e-10);
    }

    #[test]
    fn root_locus_branches_remain_aligned_when_finite_pole_count_drops() {
        let loop_tf =
            ContinuousTransferFunction::continuous(vec![-1.0, 1.0], vec![1.0, 2.0]).unwrap();
        let gains = vec![0.0, 1.0, 2.0];
        let locus = loop_tf.root_locus_data(&gains).unwrap();

        assert_eq!(locus.gains, gains);
        assert_eq!(locus.poles[0].len(), 1);
        assert!(locus.poles[1].is_empty());
        assert_eq!(locus.poles[2].len(), 1);
        assert_eq!(locus.branches.len(), 1);
        assert_eq!(locus.branches[0].poles.len(), locus.gains.len());
        assert!(locus.branches[0].poles[1].is_none());
    }
}
