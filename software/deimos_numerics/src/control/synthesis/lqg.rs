//! Dense steady-state linear quadratic Gaussian design.
//!
//! This module is intentionally a workflow layer, not a new solver layer. It
//! composes:
//!
//! - `LQR` / `DLQR` from [`super::lqr`]
//! - `LQE` / `DLQE` from [`crate::control::estimation`]
//! - observer/controller interconnection from
//!   [`crate::control::lti::state_space::ObserverControllerComposition`]
//!
//! The design follows the standard steady-state separation-principle layout:
//! compute a state-feedback gain `K`, compute an observer gain `L`, then wire
//! them together into the dynamic controller that uses `u = r - K x_hat`.
//!
//! # Two Intuitions
//!
//! 1. **Composition view.** LQG is not a new low-level solver here; it is the
//!    act of wiring together the regulator and estimator that already exist.
//! 2. **Architecture view.** It is the standard observer-based output-feedback
//!    controller: estimate the state, then feed that estimate into the LQR
//!    law.
//!
//! # Glossary
//!
//! - **Separation principle:** The regulator and estimator can be designed
//!   independently under the standard linear-Gaussian assumptions.
//! - **Dynamic controller realization:** State-space model of the compensator
//!   itself.
//!
//! # Mathematical Formulation
//!
//! The packaged controller uses the regulator gain `K` and observer gain `L`
//! to form a dynamic output-feedback controller whose internal state is the
//! observer state `x_hat`.
//!
//! # Implementation Notes
//!
//! - This module reuses existing `LQR` / `LQE` layers instead of duplicating
//!   their math.
//! - The returned `controller` and `closed_loop` realizations are built with
//!   the same observer/controller composition helper used elsewhere in the
//!   state-space layer.

use super::lqr::{LqrError, LqrSolve, dlqr_dense, lqr_dense};
use crate::control::estimation::{EstimatorError, LqeSolve, dlqe_dense, lqe_dense};
use crate::control::lti::{
    ContinuousStateSpace, ContinuousTime, DiscreteStateSpace, DiscreteTime, StateSpace,
    StateSpaceError, state_space::ObserverControllerComposition,
};
use crate::sparse::compensated::CompensatedField;
use core::fmt;
use faer::MatRef;
use faer_traits::RealField;
use num_traits::Float;

/// Result of a dense steady-state LQG design.
///
/// The nested `regulator` and `estimator` results preserve the diagnostics from
/// the underlying design layers, while `controller` and `closed_loop` provide
/// the operational realizations users typically need next.
///
/// `controller` is the standalone dynamic compensator driven by the
/// concatenated signal `[r; y]`. `closed_loop` is the augmented plant plus
/// compensator realization driven only by `r`.
#[derive(Clone, Debug)]
pub struct LqgSolve<T, Domain>
where
    T: CompensatedField,
    T::Real: Float,
{
    /// Regulator-side LQR or DLQR result.
    pub regulator: LqrSolve<T>,
    /// Estimator-side LQE or DLQE result.
    pub estimator: LqeSolve<T>,
    /// Standalone dynamic controller realization driven by `[r; y]`.
    pub controller: StateSpace<T, Domain>,
    /// Augmented plant/controller closed-loop realization driven by `r`.
    pub closed_loop: StateSpace<T, Domain>,
}

impl<T, Domain> LqgSolve<T, Domain>
where
    T: CompensatedField,
    T::Real: Float,
{
    /// Regulator gain `K` for the convention `u = r - K x_hat`.
    #[must_use]
    pub fn regulator_gain(&self) -> MatRef<'_, T> {
        self.regulator.gain.as_ref()
    }

    /// Observer gain `L`.
    #[must_use]
    pub fn observer_gain(&self) -> MatRef<'_, T> {
        self.estimator.gain.as_ref()
    }
}

/// Errors produced by dense LQG design and composition.
#[derive(Debug)]
pub enum LqgError {
    /// The regulator-side design failed.
    Lqr(LqrError),
    /// The estimator-side design failed.
    Estimator(EstimatorError),
    /// Building the controller or closed-loop state-space realization failed.
    StateSpace(StateSpaceError),
}

impl fmt::Display for LqgError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

impl core::error::Error for LqgError {}

impl From<LqrError> for LqgError {
    fn from(value: LqrError) -> Self {
        Self::Lqr(value)
    }
}

impl From<EstimatorError> for LqgError {
    fn from(value: EstimatorError) -> Self {
        Self::Estimator(value)
    }
}

impl From<StateSpaceError> for LqgError {
    fn from(value: StateSpaceError) -> Self {
        Self::StateSpace(value)
    }
}

/// Designs the dense continuous-time steady-state LQG controller.
///
/// This computes:
///
/// - the regulator gain `K` from `LQR(A, B, Q, R)`
/// - the observer gain `L` from `LQE(A, C, W, V)`
///
/// and then packages the controller and augmented closed-loop realizations
/// using the convention `u = r - K x_hat`.
pub fn lqg_dense<T>(
    a: MatRef<'_, T>,
    b: MatRef<'_, T>,
    c: MatRef<'_, T>,
    d: MatRef<'_, T>,
    q: MatRef<'_, T>,
    r: MatRef<'_, T>,
    w: MatRef<'_, T>,
    v: MatRef<'_, T>,
) -> Result<LqgSolve<T, ContinuousTime>, LqgError>
where
    T: CompensatedField,
    T::Real: Float + RealField,
{
    // Construct the validated plant model once, then reuse the existing
    // observer/controller interconnection algebra instead of re-deriving the
    // block formulas here.
    let system = ContinuousStateSpace::new(a.to_owned(), b.to_owned(), c.to_owned(), d.to_owned())?;
    let regulator = lqr_dense(a, b, q, r)?;
    let estimator = lqe_dense(a, c, w, v)?;
    // LQG stays intentionally thin: compute `K`, compute `L`, then hand the
    // actual assembly to the state-space composition layer.
    let ObserverControllerComposition {
        controller,
        closed_loop,
    } = system.observer_controller_augmented(regulator.gain.as_ref(), estimator.gain.as_ref())?;

    Ok(LqgSolve {
        regulator,
        estimator,
        controller,
        closed_loop,
    })
}

/// Designs the dense discrete-time steady-state LQG controller.
///
/// The discrete path is the same composition pattern as the continuous one,
/// but it requires an explicit `sample_time` so the returned controller and
/// closed-loop realizations carry the correct domain metadata.
///
/// The raw matrix blocks do not encode whether they represent a sampled system,
/// so the free function must accept the sample interval explicitly. The method
/// on [`DiscreteStateSpace`] reuses the sample time already stored on the
/// validated model.
pub fn dlqg_dense<T>(
    a: MatRef<'_, T>,
    b: MatRef<'_, T>,
    c: MatRef<'_, T>,
    d: MatRef<'_, T>,
    sample_time: T::Real,
    q: MatRef<'_, T>,
    r: MatRef<'_, T>,
    w: MatRef<'_, T>,
    v: MatRef<'_, T>,
) -> Result<LqgSolve<T, DiscreteTime<T::Real>>, LqgError>
where
    T: CompensatedField,
    T::Real: Float + RealField,
{
    // The discrete controller and closed-loop models must preserve the sample
    // interval, so the transient validated `StateSpace` object is part of the
    // packaging step rather than an optional convenience.
    let system = DiscreteStateSpace::new(
        a.to_owned(),
        b.to_owned(),
        c.to_owned(),
        d.to_owned(),
        sample_time,
    )?;
    let regulator = dlqr_dense(a, b, q, r)?;
    let estimator = dlqe_dense(a, c, w, v)?;
    let ObserverControllerComposition {
        controller,
        closed_loop,
    } = system.observer_controller_augmented(regulator.gain.as_ref(), estimator.gain.as_ref())?;

    Ok(LqgSolve {
        regulator,
        estimator,
        controller,
        closed_loop,
    })
}

impl<T> ContinuousStateSpace<T>
where
    T: CompensatedField,
    T::Real: Float + RealField,
{
    /// Designs the dense steady-state continuous-time LQG controller for the
    /// current plant.
    ///
    /// This is a thin convenience wrapper over [`lqg_dense`].
    pub fn lqg(
        &self,
        q: MatRef<'_, T>,
        r: MatRef<'_, T>,
        w: MatRef<'_, T>,
        v: MatRef<'_, T>,
    ) -> Result<LqgSolve<T, ContinuousTime>, LqgError> {
        lqg_dense(self.a(), self.b(), self.c(), self.d(), q, r, w, v)
    }
}

impl<T> DiscreteStateSpace<T>
where
    T: CompensatedField,
    T::Real: Float + RealField,
{
    /// Designs the dense steady-state discrete-time LQG controller for the
    /// current plant.
    ///
    /// This is a thin convenience wrapper over [`dlqg_dense`] that reuses the
    /// validated sample time stored on the model.
    pub fn dlqg(
        &self,
        q: MatRef<'_, T>,
        r: MatRef<'_, T>,
        w: MatRef<'_, T>,
        v: MatRef<'_, T>,
    ) -> Result<LqgSolve<T, DiscreteTime<T::Real>>, LqgError> {
        dlqg_dense(
            self.a(),
            self.b(),
            self.c(),
            self.d(),
            self.sample_time(),
            q,
            r,
            w,
            v,
        )
    }
}

#[cfg(test)]
mod test {
    use super::{dlqg_dense, lqg_dense};
    use crate::control::lti::state_space::{ContinuousStateSpace, DiscreteStateSpace};
    use crate::control::{dlqe_dense, dlqr_dense, lqe_dense, lqr_dense};
    use faer::Mat;

    fn assert_close(lhs: &Mat<f64>, rhs: &Mat<f64>, tol: f64) {
        assert_eq!(lhs.nrows(), rhs.nrows());
        assert_eq!(lhs.ncols(), rhs.ncols());
        for col in 0..lhs.ncols() {
            for row in 0..lhs.nrows() {
                let err = (lhs[(row, col)] - rhs[(row, col)]).abs();
                assert!(
                    err <= tol,
                    "entry ({row}, {col}) mismatch: lhs={}, rhs={}, err={err}, tol={tol}",
                    lhs[(row, col)],
                    rhs[(row, col)],
                );
            }
        }
    }

    #[test]
    fn continuous_lqg_matches_regulator_estimator_and_composition() {
        let system = ContinuousStateSpace::new(
            Mat::from_fn(1, 1, |_, _| 1.0f64),
            Mat::from_fn(1, 1, |_, _| 2.0f64),
            Mat::from_fn(1, 1, |_, _| 3.0f64),
            Mat::from_fn(1, 1, |_, _| 4.0f64),
        )
        .unwrap();
        let q = Mat::from_fn(1, 1, |_, _| 1.0f64);
        let r = Mat::from_fn(1, 1, |_, _| 1.0f64);
        let w = Mat::from_fn(1, 1, |_, _| 1.0f64);
        let v = Mat::from_fn(1, 1, |_, _| 1.0f64);

        let lqg = lqg_dense(
            system.a(),
            system.b(),
            system.c(),
            system.d(),
            q.as_ref(),
            r.as_ref(),
            w.as_ref(),
            v.as_ref(),
        )
        .unwrap();
        let lqr = lqr_dense(system.a(), system.b(), q.as_ref(), r.as_ref()).unwrap();
        let lqe = lqe_dense(system.a(), system.c(), w.as_ref(), v.as_ref()).unwrap();
        let composed = system
            .observer_controller_augmented(lqr.gain.as_ref(), lqe.gain.as_ref())
            .unwrap();

        assert_close(&lqg.regulator.gain, &lqr.gain, 1.0e-12);
        assert_close(&lqg.estimator.gain, &lqe.gain, 1.0e-12);
        assert_close(&lqg.controller.a, &composed.controller.a, 1.0e-12);
        assert_close(&lqg.controller.b, &composed.controller.b, 1.0e-12);
        assert_close(&lqg.controller.c, &composed.controller.c, 1.0e-12);
        assert_close(&lqg.controller.d, &composed.controller.d, 1.0e-12);
        assert_close(&lqg.closed_loop.a, &composed.closed_loop.a, 1.0e-12);
        assert_close(&lqg.closed_loop.b, &composed.closed_loop.b, 1.0e-12);
        assert_close(&lqg.closed_loop.c, &composed.closed_loop.c, 1.0e-12);
        assert_close(&lqg.closed_loop.d, &composed.closed_loop.d, 1.0e-12);
    }

    #[test]
    fn discrete_lqg_matches_regulator_estimator_and_methods() {
        let system = DiscreteStateSpace::new(
            Mat::from_fn(1, 1, |_, _| 1.2f64),
            Mat::from_fn(1, 1, |_, _| 1.0f64),
            Mat::from_fn(1, 1, |_, _| 0.5f64),
            Mat::from_fn(1, 1, |_, _| 0.25f64),
            0.1,
        )
        .unwrap();
        let q = Mat::from_fn(1, 1, |_, _| 1.0f64);
        let r = Mat::from_fn(1, 1, |_, _| 1.0f64);
        let w = Mat::from_fn(1, 1, |_, _| 1.0f64);
        let v = Mat::from_fn(1, 1, |_, _| 1.0f64);

        let free = dlqg_dense(
            system.a(),
            system.b(),
            system.c(),
            system.d(),
            system.sample_time(),
            q.as_ref(),
            r.as_ref(),
            w.as_ref(),
            v.as_ref(),
        )
        .unwrap();
        let method = system
            .dlqg(q.as_ref(), r.as_ref(), w.as_ref(), v.as_ref())
            .unwrap();
        let dlqr = dlqr_dense(system.a(), system.b(), q.as_ref(), r.as_ref()).unwrap();
        let dlqe = dlqe_dense(system.a(), system.c(), w.as_ref(), v.as_ref()).unwrap();

        assert_close(&free.regulator.gain, &dlqr.gain, 1.0e-12);
        assert_close(&free.estimator.gain, &dlqe.gain, 1.0e-12);
        assert_close(&free.controller.a, &method.controller.a, 1.0e-12);
        assert_close(&free.controller.b, &method.controller.b, 1.0e-12);
        assert_close(&free.controller.c, &method.controller.c, 1.0e-12);
        assert_close(&free.controller.d, &method.controller.d, 1.0e-12);
        assert_close(&free.closed_loop.a, &method.closed_loop.a, 1.0e-12);
        assert_close(&free.closed_loop.b, &method.closed_loop.b, 1.0e-12);
        assert_close(&free.closed_loop.c, &method.closed_loop.c, 1.0e-12);
        assert_close(&free.closed_loop.d, &method.closed_loop.d, 1.0e-12);
    }
}
