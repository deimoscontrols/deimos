//! Fixed-size discrete-time state-space models.
//!
//! # Glossary
//!
//! - **DC gain:** Constant-input to constant-output gain.

use crate::embedded::error::EmbeddedError;
use crate::embedded::fixed::linalg::{
    Matrix as MatrixStorage, Vector as VectorStorage, mat_vec_mul, vec_add,
};
use num_traits::Float;

/// Fixed-size row-major matrix storage.
pub type Matrix<T, const R: usize, const C: usize> = MatrixStorage<T, R, C>;

/// Fixed-size column vector storage.
pub type Vector<T, const N: usize> = VectorStorage<T, N>;

/// Fixed-size discrete-time state-space realization.
///
/// The realized dynamics are
/// `x[k+1] = A x[k] + B u[k]` and `y[k] = C x[k] + D u[k]`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DiscreteStateSpace<T, const NX: usize, const NU: usize, const NY: usize> {
    a: Matrix<T, NX, NX>,
    b: Matrix<T, NX, NU>,
    c: Matrix<T, NY, NX>,
    d: Matrix<T, NY, NU>,
    sample_time: T,
}

impl<T, const NX: usize, const NU: usize, const NY: usize> DiscreteStateSpace<T, NX, NU, NY>
where
    T: Float,
{
    /// Creates a discrete-time state-space realization.
    ///
    /// Args:
    ///   a: State matrix with shape `(NX, NX)`.
    ///   b: Input matrix with shape `(NX, NU)`.
    ///   c: Output matrix with shape `(NY, NX)`.
    ///   d: Feedthrough matrix with shape `(NY, NU)`.
    ///   sample_time: Discrete sample interval in seconds or the caller's
    ///     chosen base time unit. It must be finite and positive.
    ///
    /// Returns:
    ///   A validated fixed-size discrete-time realization.
    pub fn new(
        a: Matrix<T, NX, NX>,
        b: Matrix<T, NX, NU>,
        c: Matrix<T, NY, NX>,
        d: Matrix<T, NY, NU>,
        sample_time: T,
    ) -> Result<Self, EmbeddedError> {
        if !sample_time.is_finite() || sample_time <= T::zero() {
            return Err(EmbeddedError::InvalidSampleTime);
        }
        Ok(Self {
            a,
            b,
            c,
            d,
            sample_time,
        })
    }

    /// Returns the state matrix `A` with shape `(NX, NX)`.
    #[must_use]
    pub fn a(&self) -> &Matrix<T, NX, NX> {
        &self.a
    }

    /// Returns the input matrix `B` with shape `(NX, NU)`.
    #[must_use]
    pub fn b(&self) -> &Matrix<T, NX, NU> {
        &self.b
    }

    /// Returns the output matrix `C` with shape `(NY, NX)`.
    #[must_use]
    pub fn c(&self) -> &Matrix<T, NY, NX> {
        &self.c
    }

    /// Returns the feedthrough matrix `D` with shape `(NY, NU)`.
    #[must_use]
    pub fn d(&self) -> &Matrix<T, NY, NU> {
        &self.d
    }

    /// Returns the stored sample interval in the same time unit supplied to
    /// [`Self::new`].
    #[must_use]
    pub fn sample_time(&self) -> T {
        self.sample_time
    }

    /// Evaluates the current output `C x + D u`.
    ///
    /// Args:
    ///   x: Current state vector with shape `(NX,)`.
    ///   u: Current input vector with shape `(NU,)`.
    ///
    /// Returns:
    ///   The output vector with shape `(NY,)`, in the output units implied by
    ///   `C x + D u`.
    #[must_use]
    pub fn output(&self, x: &Vector<T, NX>, u: &Vector<T, NU>) -> Vector<T, NY> {
        vec_add(&mat_vec_mul(&self.c, x), &mat_vec_mul(&self.d, u))
    }

    /// Evaluates the next state `A x + B u`.
    ///
    /// Args:
    ///   x: Current state vector with shape `(NX,)`.
    ///   u: Current input vector with shape `(NU,)`.
    ///
    /// Returns:
    ///   The next state vector with shape `(NX,)`.
    #[must_use]
    pub fn next_state(&self, x: &Vector<T, NX>, u: &Vector<T, NU>) -> Vector<T, NX> {
        vec_add(&mat_vec_mul(&self.a, x), &mat_vec_mul(&self.b, u))
    }

    /// Advances one discrete timestep in place and returns the output.
    ///
    /// Args:
    ///   x: In-place state vector with shape `(NX,)`. On entry it is `x[k]`;
    ///     on return it is overwritten with `x[k+1]`.
    ///   u: Input vector with shape `(NU,)` applied over one sample interval.
    ///
    /// Returns:
    ///   The output vector `y[k]` with shape `(NY,)`.
    pub fn step(&self, x: &mut Vector<T, NX>, u: Vector<T, NU>) -> Vector<T, NY> {
        let y = self.output(x, &u);
        *x = self.next_state(x, &u);
        y
    }
}

impl<T, const NX: usize, const NU: usize, const NY: usize> DiscreteStateSpace<T, NX, NU, NY>
where
    T: Float,
{
    /// Returns the steady-state gain `G(1) = C (I - A)^-1 B + D`.
    ///
    /// Returns:
    ///   The DC gain matrix with shape `(NY, NU)`, mapping constant inputs to
    ///   constant outputs in units of output per unit input.
    pub fn dc_gain(&self) -> Result<Matrix<T, NY, NU>, EmbeddedError> {
        let lhs = core::array::from_fn(|row| {
            core::array::from_fn(|col| {
                if row == col {
                    T::one() - self.a[row][col]
                } else {
                    -self.a[row][col]
                }
            })
        });
        let state_gain = crate::embedded::fixed::linalg::solve_linear_system(
            &lhs,
            &self.b,
            "state_space.dc_gain",
        )?;

        Ok(crate::embedded::fixed::linalg::mat_add(
            &crate::embedded::fixed::linalg::mat_mul(&self.c, &state_gain),
            &self.d,
        ))
    }
}

#[cfg(feature = "alloc")]
impl<T, const NX: usize, const NU: usize, const NY: usize>
    TryFrom<&crate::control::lti::DiscreteStateSpace<T>> for DiscreteStateSpace<T, NX, NU, NY>
where
    T: Float + faer_traits::RealField,
{
    type Error = EmbeddedError;

    /// Converts a dynamic control-side state-space model into a fixed-size
    /// embedded representation when the dimensions match.
    ///
    /// Args:
    ///   value: Dynamic discrete-time state-space model with `NX` states,
    ///     `NU` inputs, and `NY` outputs, plus a positive sample interval.
    ///
    /// Returns:
    ///   The same realization copied into fixed-size storage.
    fn try_from(value: &crate::control::lti::DiscreteStateSpace<T>) -> Result<Self, Self::Error> {
        if value.nstates() != NX {
            return Err(EmbeddedError::DimensionMismatch {
                which: "embedded.fixed.state_space.nstates",
                expected_rows: NX,
                expected_cols: 1,
                actual_rows: value.nstates(),
                actual_cols: 1,
            });
        }
        if value.ninputs() != NU {
            return Err(EmbeddedError::DimensionMismatch {
                which: "embedded.fixed.state_space.ninputs",
                expected_rows: NU,
                expected_cols: 1,
                actual_rows: value.ninputs(),
                actual_cols: 1,
            });
        }
        if value.noutputs() != NY {
            return Err(EmbeddedError::DimensionMismatch {
                which: "embedded.fixed.state_space.noutputs",
                expected_rows: NY,
                expected_cols: 1,
                actual_rows: value.noutputs(),
                actual_cols: 1,
            });
        }

        let mut a = [[T::zero(); NX]; NX];
        let mut b = [[T::zero(); NU]; NX];
        let mut c = [[T::zero(); NX]; NY];
        let mut d = [[T::zero(); NU]; NY];

        for i in 0..NX {
            for j in 0..NX {
                a[i][j] = value.a()[(i, j)];
            }
            for j in 0..NU {
                b[i][j] = value.b()[(i, j)];
            }
        }
        for i in 0..NY {
            for j in 0..NX {
                c[i][j] = value.c()[(i, j)];
            }
            for j in 0..NU {
                d[i][j] = value.d()[(i, j)];
            }
        }

        Self::new(a, b, c, d, value.sample_time())
    }
}

#[cfg(test)]
mod tests {
    use super::DiscreteStateSpace;
    use crate::embedded::error::EmbeddedError;

    #[test]
    fn dc_gain_rejects_unit_pole() {
        let system =
            DiscreteStateSpace::<f64, 1, 1, 1>::new([[1.0]], [[1.0]], [[1.0]], [[0.0]], 0.1)
                .unwrap();

        let err = system.dc_gain().unwrap_err();

        assert_eq!(
            err,
            EmbeddedError::SingularMatrix {
                which: "state_space.dc_gain"
            }
        );
    }

    #[test]
    fn dc_gain_matches_scalar_finite_reference() {
        let system =
            DiscreteStateSpace::<f64, 1, 1, 1>::new([[0.5]], [[2.0]], [[3.0]], [[4.0]], 0.1)
                .unwrap();

        let gain = system.dc_gain().unwrap();

        assert!((gain[0][0] - 16.0).abs() < 1.0e-12);
    }
}
