//! Dense infinite-horizon linear quadratic regulator design.
//!
//! This module is the controller-design layer that sits directly on top of the
//! dense Riccati solvers in [`crate::control::matrix_equations::riccati`]. It does not reimplement CARE or
//! DARE; it packages those solves into a controller-oriented result:
//!
//! - feedback gain `K`
//! - Riccati solution `X`
//! - closed-loop state matrix `A - B K`
//! - residual and stabilizing diagnostics
//!
//! # Two Intuitions
//!
//! 1. **Optimal-tradeoff view.** LQR chooses the feedback gain that best
//!    balances state regulation against control effort for a quadratic cost.
//! 2. **Packaging view.** In code, this module is mostly a thin adapter that
//!    turns a Riccati solve into the controller-side quantities users actually
//!    want to inspect.
//!
//! # Glossary
//!
//! - **Quadratic cost:** Objective weighting state error by `Q` and control
//!   effort by `R`.
//! - **Stabilizing solution:** Riccati solution whose induced closed loop is
//!   stable.
//!
//! # Mathematical Formulation
//!
//! The returned gain satisfies:
//!
//! - continuous: `u = -K x`, `K = R^-1 B^H X`
//! - discrete: `u = -K x`, `K = (R + B^H X B)^-1 B^H X A`
//!
//! where `X` is the CARE or DARE solution.
//!
//! # Implementation Notes
//!
//! - The module intentionally contains little new linear algebra.
//! - All numerical difficulty is delegated to the Riccati layer, and the solve
//!   diagnostics are forwarded into the returned `LqrSolve`.

use crate::control::dense_ops::dense_mul;
use crate::control::matrix_equations::{RiccatiError, solve_care_dense, solve_dare_dense};
use crate::sparse::compensated::CompensatedField;
use core::fmt;
use faer::{Mat, MatRef};
use faer_traits::RealField;
use num_traits::Float;

/// Result of a dense continuous- or discrete-time LQR solve.
///
/// The gain `K` is returned for the convention `u = -K x`. The closed-loop
/// matrix is therefore `A_cl = A - B K`.
#[derive(Clone, Debug)]
pub struct LqrSolve<T: CompensatedField>
where
    T::Real: Float,
{
    /// State-feedback gain.
    pub gain: Mat<T>,
    /// Hermitian Riccati solution matrix.
    pub solution: Mat<T>,
    /// Closed-loop state matrix `A - B K`.
    pub closed_loop_a: Mat<T>,
    /// Compensated Riccati residual norm returned by the solver layer.
    pub residual_norm: T::Real,
    /// Whether the associated Riccati solution passed the stabilizing check.
    pub stabilizing: bool,
}

/// Errors produced by dense LQR / DLQR design.
#[derive(Debug)]
pub enum LqrError {
    /// The underlying Riccati solve failed.
    Riccati(RiccatiError),
}

impl fmt::Display for LqrError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

impl core::error::Error for LqrError {}

impl From<RiccatiError> for LqrError {
    fn from(value: RiccatiError) -> Self {
        Self::Riccati(value)
    }
}

/// Solves the dense continuous-time LQR problem defined by `(A, B, Q, R)`.
///
/// This computes the stabilizing CARE solution `X`, then the state-feedback
/// gain
///
/// `K = R^-1 B^H X`
///
/// and packages the resulting closed-loop matrix `A - B K`.
pub fn lqr_dense<T>(
    a: MatRef<'_, T>,
    b: MatRef<'_, T>,
    q: MatRef<'_, T>,
    r: MatRef<'_, T>,
) -> Result<LqrSolve<T>, LqrError>
where
    T: CompensatedField,
    T::Real: Float + RealField,
{
    // LQR is intentionally thin over the Riccati layer: solve CARE once, then
    // package the controller-oriented quantities callers actually need.
    let riccati = solve_care_dense(a, b, q, r)?;
    Ok(LqrSolve {
        closed_loop_a: closed_loop_matrix(a, b, riccati.gain.as_ref()),
        gain: riccati.gain,
        solution: riccati.solution,
        residual_norm: riccati.residual_norm,
        stabilizing: riccati.stabilizing,
    })
}

/// Solves the dense discrete-time DLQR problem defined by `(A, B, Q, R)`.
///
/// This computes the stabilizing DARE solution `X`, then the state-feedback
/// gain
///
/// `K = (R + B^H X B)^-1 B^H X A`
///
/// and packages the resulting closed-loop matrix `A - B K`.
pub fn dlqr_dense<T>(
    a: MatRef<'_, T>,
    b: MatRef<'_, T>,
    q: MatRef<'_, T>,
    r: MatRef<'_, T>,
) -> Result<LqrSolve<T>, LqrError>
where
    T: CompensatedField,
    T::Real: Float + RealField,
{
    // DLQR is the same packaging step on top of the discrete Riccati solve.
    let riccati = solve_dare_dense(a, b, q, r)?;
    Ok(LqrSolve {
        closed_loop_a: closed_loop_matrix(a, b, riccati.gain.as_ref()),
        gain: riccati.gain,
        solution: riccati.solution,
        residual_norm: riccati.residual_norm,
        stabilizing: riccati.stabilizing,
    })
}

fn closed_loop_matrix<T>(a: MatRef<'_, T>, b: MatRef<'_, T>, k: MatRef<'_, T>) -> Mat<T>
where
    T: CompensatedField,
    T::Real: Float,
{
    let bk = dense_mul(b, k);
    Mat::from_fn(a.nrows(), a.ncols(), |row, col| {
        // The public controller result always returns the closed-loop state
        // matrix explicitly so later response/simulation code does not need to
        // recompute `A - B K` on its own.
        a[(row, col)] - bk[(row, col)]
    })
}

#[cfg(test)]
mod test {
    use super::{LqrError, dlqr_dense, lqr_dense};
    use crate::control::lti::state_space::{ContinuousStateSpace, DiscreteStateSpace};
    use crate::control::{RiccatiError, solve_care_dense, solve_dare_dense};
    use faer::Mat;
    use faer_traits::ext::ComplexFieldExt;

    fn assert_close<T>(lhs: &Mat<T>, rhs: &Mat<T>, tol: T::Real)
    where
        T: crate::sparse::compensated::CompensatedField,
        T::Real: num_traits::Float,
    {
        assert_eq!(lhs.nrows(), rhs.nrows());
        assert_eq!(lhs.ncols(), rhs.ncols());
        for col in 0..lhs.ncols() {
            for row in 0..lhs.nrows() {
                let err = (lhs[(row, col)] - rhs[(row, col)]).abs1();
                assert!(
                    err <= tol,
                    "entry ({row}, {col}) mismatch: err={err:?}, tol={tol:?}",
                );
            }
        }
    }

    #[test]
    fn lqr_matches_scalar_closed_form() {
        let a = Mat::from_fn(1, 1, |_, _| 1.0f64);
        let b = Mat::from_fn(1, 1, |_, _| 1.0f64);
        let q = Mat::from_fn(1, 1, |_, _| 1.0f64);
        let r = Mat::from_fn(1, 1, |_, _| 1.0f64);
        let solve = lqr_dense(a.as_ref(), b.as_ref(), q.as_ref(), r.as_ref()).unwrap();

        let expected_k = 1.0 + 2.0f64.sqrt();
        let expected_acl = -2.0f64.sqrt();
        assert!((solve.gain[(0, 0)] - expected_k).abs() < 1.0e-10);
        assert!((solve.closed_loop_a[(0, 0)] - expected_acl).abs() < 1.0e-10);
        assert!(solve.stabilizing);
    }

    #[test]
    fn dlqr_matches_scalar_closed_form() {
        let a = Mat::from_fn(1, 1, |_, _| 1.2f64);
        let b = Mat::from_fn(1, 1, |_, _| 1.0f64);
        let q = Mat::from_fn(1, 1, |_, _| 1.0f64);
        let r = Mat::from_fn(1, 1, |_, _| 1.0f64);
        let solve = dlqr_dense(a.as_ref(), b.as_ref(), q.as_ref(), r.as_ref()).unwrap();

        let x = (1.44 + (1.44f64 * 1.44 + 4.0).sqrt()) / 2.0;
        let expected_k = 1.2 * x / (1.0 + x);
        let expected_acl = 1.2 - expected_k;
        assert!((solve.gain[(0, 0)] - expected_k).abs() < 1.0e-10);
        assert!((solve.closed_loop_a[(0, 0)] - expected_acl).abs() < 1.0e-10);
        assert!(solve.stabilizing);
    }

    #[test]
    fn lqr_small_diagonal_system_matches_riccati_gain() {
        let a = Mat::from_fn(2, 2, |row, col| match (row, col) {
            (0, 0) => 1.0,
            (1, 1) => -0.5,
            _ => 0.0,
        });
        let b = Mat::from_fn(2, 2, |row, col| if row == col { 1.0 } else { 0.0 });
        let q = Mat::from_fn(
            2,
            2,
            |row, col| if row == col { 1.0 + row as f64 } else { 0.0 },
        );
        let r = Mat::from_fn(2, 2, |row, col| if row == col { 1.0 } else { 0.0 });
        let lqr = lqr_dense(a.as_ref(), b.as_ref(), q.as_ref(), r.as_ref()).unwrap();
        let riccati = solve_care_dense(a.as_ref(), b.as_ref(), q.as_ref(), r.as_ref()).unwrap();

        assert_close(&lqr.gain, &riccati.gain, 1.0e-12);
        assert_close(
            &lqr.closed_loop_a,
            &Mat::from_fn(2, 2, |row, col| {
                a[(row, col)]
                    - if row == col {
                        riccati.gain[(row, col)]
                    } else {
                        0.0
                    }
            }),
            1.0e-12,
        );
        assert!(lqr.stabilizing);
    }

    #[test]
    fn dlqr_small_diagonal_system_matches_riccati_gain() {
        let a = Mat::from_fn(2, 2, |row, col| match (row, col) {
            (0, 0) => 1.2,
            (1, 1) => 0.5,
            _ => 0.0,
        });
        let b = Mat::from_fn(2, 2, |row, col| if row == col { 1.0 } else { 0.0 });
        let q = Mat::from_fn(
            2,
            2,
            |row, col| if row == col { 1.0 + row as f64 } else { 0.0 },
        );
        let r = Mat::from_fn(2, 2, |row, col| if row == col { 1.0 } else { 0.0 });
        let lqr = dlqr_dense(a.as_ref(), b.as_ref(), q.as_ref(), r.as_ref()).unwrap();
        let riccati = solve_dare_dense(a.as_ref(), b.as_ref(), q.as_ref(), r.as_ref()).unwrap();

        assert_close(&lqr.gain, &riccati.gain, 1.0e-12);
        assert!(lqr.stabilizing);
    }

    #[test]
    fn state_space_lqr_matches_free_function() {
        let a = Mat::from_fn(2, 2, |row, col| match (row, col) {
            (0, 0) => 1.0,
            (0, 1) => 2.0,
            (1, 1) => -0.5,
            _ => 0.0,
        });
        let b = Mat::from_fn(2, 1, |row, _| if row == 0 { 1.0 } else { 0.5 });
        let c = Mat::zeros(1, 2);
        let d = Mat::zeros(1, 1);
        let q = Mat::from_fn(2, 2, |row, col| if row == col { 1.0 } else { 0.0 });
        let r = Mat::from_fn(1, 1, |_, _| 1.0);

        let system = ContinuousStateSpace::new(a.clone(), b.clone(), c, d).unwrap();
        let free = lqr_dense(a.as_ref(), b.as_ref(), q.as_ref(), r.as_ref()).unwrap();
        let method = system.lqr(q.as_ref(), r.as_ref()).unwrap();
        assert_close(&free.gain, &method.gain, 1.0e-12);
        assert_close(&free.closed_loop_a, &method.closed_loop_a, 1.0e-12);
    }

    #[test]
    fn state_space_dlqr_matches_free_function() {
        let a = Mat::from_fn(2, 2, |row, col| match (row, col) {
            (0, 0) => 1.2,
            (0, 1) => 0.3,
            (1, 1) => 0.7,
            _ => 0.0,
        });
        let b = Mat::from_fn(2, 1, |row, _| if row == 0 { 1.0 } else { 0.5 });
        let c = Mat::zeros(1, 2);
        let d = Mat::zeros(1, 1);
        let q = Mat::from_fn(2, 2, |row, col| if row == col { 1.0 } else { 0.0 });
        let r = Mat::from_fn(1, 1, |_, _| 1.0);

        let system = DiscreteStateSpace::new(a.clone(), b.clone(), c, d, 0.1).unwrap();
        let free = dlqr_dense(a.as_ref(), b.as_ref(), q.as_ref(), r.as_ref()).unwrap();
        let method = system.dlqr(q.as_ref(), r.as_ref()).unwrap();
        assert_close(&free.gain, &method.gain, 1.0e-12);
        assert_close(&free.closed_loop_a, &method.closed_loop_a, 1.0e-12);
    }

    #[test]
    fn singular_r_error_propagates() {
        let a = Mat::from_fn(1, 1, |_, _| 1.0f64);
        let b = Mat::from_fn(1, 1, |_, _| 1.0f64);
        let q = Mat::from_fn(1, 1, |_, _| 1.0f64);
        let r = Mat::zeros(1, 1);
        let err = lqr_dense(a.as_ref(), b.as_ref(), q.as_ref(), r.as_ref()).unwrap_err();
        assert!(matches!(
            err,
            LqrError::Riccati(RiccatiError::SingularControlWeight { which: "r" })
        ));
    }
}
