//! Dense and sparse state-space model types plus dense conversion routines.
//!
//! The state-space layer in this crate is intentionally dense-first.
//! Exact `c2d` conversions use dense matrix functions, and even bilinear
//! conversion generally produces dense explicit discrete-time state matrices.
//!
//! The design still keeps continuous and discrete systems distinct in the type
//! system so downstream algorithms can state their assumptions clearly.
//!
//! This module is the model layer that sits above the lower-level Gramian and
//! matrix-equation routines in [`crate::control::matrix_equations::lyapunov`]. It gives those routines a
//! structured `A/B/C/D` home and makes the time domain part of the type
//! instead of leaving it as an implicit convention at the call site.
//!
//! # Two Intuitions
//!
//! 1. **Dynamics view.** State space exposes the internal memory of a system:
//!    `x` stores what the past is still doing to the future.
//! 2. **Interconnection view.** It is also the representation in which
//!    composition, feedback, observers, and model reduction are easiest to
//!    express without inflating polynomial degree.
//!
//! # Glossary
//!
//! - **State matrix `A`:** Internal dynamics.
//! - **Input matrix `B`:** How the input drives the state.
//! - **Output matrix `C`:** How the state is observed.
//! - **Feedthrough `D`:** Instantaneous input-output map.
//! - **ZOH:** Zero-order hold, the standard sampled-input assumption for `c2d`.
//!
//! # Mathematical Formulation
//!
//! The same `A/B/C/D` storage supports both:
//!
//! - continuous time: `x' = A x + B u`, `y = C x + D u`
//! - discrete time: `x[k+1] = A x[k] + B u[k]`, `y[k] = C x[k] + D u[k]`
//!
//! Similarity-related realizations represent the same external transfer map,
//! but in different internal coordinates.
//!
//! # Implementation Notes
//!
//! - The time domain is carried in the type, not as a loose runtime flag.
//! - Dense state space is the main manipulation surface; sparse state space is
//!   available where the algorithms are already credible.
//! - Exact discrete integer delays are represented by explicit shift-register
//!   state augmentation.
//!
//! # Feature Matrix
//!
//! | Feature | Dense continuous | Dense discrete | Sparse continuous | Sparse discrete |
//! | --- | --- | --- | --- | --- |
//! | Construction / validation | yes | yes | yes | yes |
//! | `c2d` / `d2c` | yes | yes | no | no |
//! | Structural composition | yes | yes | no | no |
//! | Gramian adapters | yes | yes | no | no |
//! | Controller / observer helpers | yes | yes | no | no |
//! | Exact integer delays | no | yes | no | no |

pub(crate) mod convert;
mod domain;
mod error;
mod sparse;

pub use convert::{ContinuousizationMethod, DiscretizationMethod};
pub use domain::{ContinuousTime, DiscreteTime};
pub use error::StateSpaceError;
pub use sparse::{SparseContinuousStateSpace, SparseDiscreteStateSpace, SparseStateSpace};

use crate::control::dense_ops::dense_mul_plain;
use crate::control::matrix_equations::lyapunov::{
    DenseLyapunovSolve, LyapunovError, controllability_gramian_dense, observability_gramian_dense,
};
use crate::control::matrix_equations::stein::{
    DenseSteinSolve, SteinError, controllability_gramian_discrete_dense,
    observability_gramian_discrete_dense,
};
use crate::control::synthesis::{LqrError, LqrSolve, dlqr_dense, lqr_dense};
use crate::sparse::compensated::CompensatedField;
use faer::{Mat, MatRef};
use faer_traits::ComplexField;
use faer_traits::ext::ComplexFieldExt;
use num_traits::{Float, NumCast, Zero};

/// Dense linear time-invariant state-space system.
///
/// The same storage layout is used for both continuous and discrete systems.
/// The `Domain` type parameter carries the semantic difference:
///
/// - continuous systems interpret `A` as the infinitesimal generator in
///   `x' = A x + B u`
/// - discrete systems interpret `A` as the one-step state transition in
///   `x[k+1] = A x[k] + B u[k]`
#[derive(Clone, Debug, PartialEq)]
pub struct StateSpace<T, Domain> {
    pub(crate) a: Mat<T>,
    pub(crate) b: Mat<T>,
    pub(crate) c: Mat<T>,
    pub(crate) d: Mat<T>,
    pub(crate) domain: Domain,
}

/// Dense continuous-time state-space system.
pub type ContinuousStateSpace<T> = StateSpace<T, ContinuousTime>;

/// Dense discrete-time state-space system.
pub type DiscreteStateSpace<T> = StateSpace<T, DiscreteTime<<T as ComplexField>::Real>>;

/// Result of composing a plant with an observer-based state-feedback
/// controller.
///
/// `controller` is the standalone dynamic controller realization driven by the
/// concatenated input `[r; y]`, where `r` is the external reference/disturbance
/// input and `y` is the measured plant output. `closed_loop` is the resulting
/// plant-plus-controller closed-loop system driven only by `r`.
#[derive(Clone, Debug, PartialEq)]
pub struct ObserverControllerComposition<T, Domain> {
    /// Standalone dynamic controller realization.
    pub controller: StateSpace<T, Domain>,
    /// Augmented closed-loop plant/controller realization.
    pub closed_loop: StateSpace<T, Domain>,
}

impl<T, Domain> StateSpace<T, Domain> {
    /// Number of states.
    ///
    /// This is the size of the internal state vector `x`.
    #[must_use]
    pub fn nstates(&self) -> usize {
        self.a.nrows()
    }

    /// Number of inputs.
    ///
    /// This is the width of the input vector `u`.
    #[must_use]
    pub fn ninputs(&self) -> usize {
        self.b.ncols()
    }

    /// Number of outputs.
    ///
    /// This is the height of the output vector `y`.
    #[must_use]
    pub fn noutputs(&self) -> usize {
        self.c.nrows()
    }

    /// Returns whether the system is single-input single-output.
    #[must_use]
    pub fn is_siso(&self) -> bool {
        self.ninputs() == 1 && self.noutputs() == 1
    }

    /// State matrix `A`.
    #[must_use]
    pub fn a(&self) -> MatRef<'_, T> {
        self.a.as_ref()
    }

    /// Input matrix `B`.
    #[must_use]
    pub fn b(&self) -> MatRef<'_, T> {
        self.b.as_ref()
    }

    /// Output matrix `C`.
    #[must_use]
    pub fn c(&self) -> MatRef<'_, T> {
        self.c.as_ref()
    }

    /// Feedthrough matrix `D`.
    #[must_use]
    pub fn d(&self) -> MatRef<'_, T> {
        self.d.as_ref()
    }

    /// Domain metadata carried by the system.
    #[must_use]
    pub fn domain(&self) -> &Domain {
        &self.domain
    }

    /// Splits the state-space system back into its owned parts.
    ///
    /// This is mainly useful when a caller wants to reuse the validated model
    /// storage in a different representation without cloning the matrices.
    #[must_use]
    pub fn into_parts(self) -> (Mat<T>, Mat<T>, Mat<T>, Mat<T>, Domain) {
        (self.a, self.b, self.c, self.d, self.domain)
    }
}

impl<T> ContinuousStateSpace<T>
where
    T: ComplexField,
{
    /// Creates a continuous-time state-space system after validating the
    /// `A/B/C/D` block dimensions.
    ///
    /// The validated model represents
    ///
    /// `x' = A x + B u`
    ///
    /// `y  = C x + D u`
    pub fn new(a: Mat<T>, b: Mat<T>, c: Mat<T>, d: Mat<T>) -> Result<Self, StateSpaceError> {
        validate_blocks(
            a.nrows(),
            a.ncols(),
            b.nrows(),
            b.ncols(),
            c.nrows(),
            c.ncols(),
            d.nrows(),
            d.ncols(),
        )?;
        Ok(Self {
            a,
            b,
            c,
            d,
            domain: ContinuousTime,
        })
    }

    /// Creates a continuous-time model with a zero feedthrough matrix.
    ///
    /// This is the common case in state-space analysis workflows where the
    /// direct input-to-output path is absent or intentionally omitted.
    pub fn with_zero_feedthrough(a: Mat<T>, b: Mat<T>, c: Mat<T>) -> Result<Self, StateSpaceError> {
        let d = Mat::zeros(c.nrows(), b.ncols());
        Self::new(a, b, c, d)
    }
}

impl<T> ContinuousStateSpace<T>
where
    T: CompensatedField,
    T::Real: Float,
{
    /// Casts the dense continuous-time state-space matrices to another scalar
    /// dtype.
    ///
    /// This preserves the current realization exactly; it is not a balancing
    /// or re-realization pass.
    pub fn try_cast<U>(&self) -> Result<ContinuousStateSpace<U>, StateSpaceError>
    where
        U: CompensatedField,
        U::Real: Float,
    {
        ContinuousStateSpace::new(
            cast_mat(self.a(), "state_space.a")?,
            cast_mat(self.b(), "state_space.b")?,
            cast_mat(self.c(), "state_space.c")?,
            cast_mat(self.d(), "state_space.d")?,
        )
    }
}

impl<T> ContinuousStateSpace<T>
where
    T: ComplexField + Copy,
{
    /// Forms the parallel sum of two continuous-time systems.
    ///
    /// Both systems must have the same input and output dimensions. The result
    /// preserves the shared input and sums the two output channels.
    pub fn parallel_add(&self, rhs: &Self) -> Result<Self, StateSpaceError> {
        let (a, b, c, d) = parallel_parts(self, rhs, false)?;
        Self::new(a, b, c, d)
    }

    /// Forms the parallel difference `self - rhs` of two continuous-time
    /// systems.
    pub fn parallel_sub(&self, rhs: &Self) -> Result<Self, StateSpaceError> {
        let (a, b, c, d) = parallel_parts(self, rhs, true)?;
        Self::new(a, b, c, d)
    }

    /// Forms the series interconnection `u -> self -> next`.
    ///
    /// The output dimension of `self` must match the input dimension of
    /// `next`. The resulting system keeps the input channels of `self` and the
    /// output channels of `next`.
    pub fn series(&self, next: &Self) -> Result<Self, StateSpaceError> {
        let (a, b, c, d) = series_parts(self, next)?;
        Self::new(a, b, c, d)
    }

    /// Forms the side-by-side block-diagonal append of two systems.
    ///
    /// The appended system has independent input and output channels for the
    /// two subsystems and is useful as a structural building block for larger
    /// interconnections.
    pub fn append(&self, rhs: &Self) -> Result<Self, StateSpaceError> {
        let (a, b, c, d) = append_parts(self, rhs);
        Self::new(a, b, c, d)
    }

    /// Closes static state feedback with the convention `u = u_ext - K x`.
    ///
    /// The returned system preserves the original external input channels
    /// `u_ext`; it does not eliminate them. This makes the helper suitable for
    /// disturbance/reference analysis as well as controller assembly.
    pub fn with_state_feedback(&self, k: MatRef<'_, T>) -> Result<Self, StateSpaceError> {
        validate_state_feedback_gain(self, k)?;
        let bk = dense_mul(self.b(), k)?;
        let dk = dense_mul(self.d(), k)?;
        let a = self.a() - &bk;
        let c = self.c() - &dk;
        Self::new(a, self.b().to_owned(), c, self.d().to_owned())
    }

    /// Applies observer-style output injection with innovation gain `L`.
    ///
    /// The returned system uses the concatenated external input `[u; y_ext]`
    /// and evolves according to
    ///
    /// `x' = (A - L C) x + (B - L D) u + L y_ext`
    ///
    /// while reporting the estimated output `C x + D u`.
    pub fn with_output_injection(&self, l: MatRef<'_, T>) -> Result<Self, StateSpaceError> {
        let (a, b, c, d) = output_injection_parts(self, l)?;
        Self::new(a, b, c, d)
    }

    /// Builds the observer-based controller realization and the corresponding
    /// augmented plant/controller closed-loop model.
    ///
    /// The controller implements `u = r - K x_hat` with observer gain `L`. The
    /// returned `controller` takes the concatenated input `[r; y]`, while the
    /// returned `closed_loop` is driven only by `r`.
    pub fn observer_controller_augmented(
        &self,
        k: MatRef<'_, T>,
        l: MatRef<'_, T>,
    ) -> Result<ObserverControllerComposition<T, ContinuousTime>, StateSpaceError> {
        let (controller, closed_loop) = observer_controller_parts(self, k, l)?;
        Ok(ObserverControllerComposition {
            controller: ContinuousStateSpace::new(
                controller.0,
                controller.1,
                controller.2,
                controller.3,
            )?,
            closed_loop: ContinuousStateSpace::new(
                closed_loop.0,
                closed_loop.1,
                closed_loop.2,
                closed_loop.3,
            )?,
        })
    }
}

impl<T> ContinuousStateSpace<T>
where
    T: CompensatedField,
    T::Real: Float + faer_traits::RealField,
{
    /// Computes the dense continuous-time controllability Gramian of the model.
    ///
    /// Intuitively, this measures how strongly the input channels in `B` can
    /// drive the internal state through the stable continuous-time dynamics.
    pub fn controllability_gramian(&self) -> Result<DenseLyapunovSolve<T>, LyapunovError> {
        controllability_gramian_dense(self.a.as_ref(), self.b.as_ref())
    }

    /// Computes the dense continuous-time observability Gramian of the model.
    ///
    /// Intuitively, this measures how strongly the internal state is reflected
    /// in the outputs through `C`.
    pub fn observability_gramian(&self) -> Result<DenseLyapunovSolve<T>, LyapunovError> {
        observability_gramian_dense(self.a.as_ref(), self.c.as_ref())
    }

    /// Converts the continuous-time model into a discrete-time one using the
    /// requested method and sample interval.
    ///
    /// The method is explicit because different `c2d` conversions encode
    /// different intersample assumptions. Zero-order hold models piecewise
    /// constant inputs; bilinear/Tustin models the trapezoidal-rule map that is
    /// common in digital filter and controller design.
    pub fn discretize(
        &self,
        sample_time: T::Real,
        method: DiscretizationMethod<T::Real>,
    ) -> Result<DiscreteStateSpace<T>, StateSpaceError> {
        convert::discretize(self, sample_time, method)
    }

    /// Designs the dense infinite-horizon continuous-time LQR controller.
    ///
    /// This is a convenience wrapper around [`crate::control::synthesis::lqr_dense`] using the
    /// model's stored `A` and `B` blocks.
    pub fn lqr(&self, q: MatRef<'_, T>, r: MatRef<'_, T>) -> Result<LqrSolve<T>, LqrError> {
        lqr_dense(self.a.as_ref(), self.b.as_ref(), q, r)
    }
}

impl<T> DiscreteStateSpace<T>
where
    T: ComplexField,
    T::Real: Float,
{
    /// Creates a discrete-time state-space system after validating dimensions
    /// and sample interval.
    ///
    /// The validated model represents
    ///
    /// `x[k + 1] = A x[k] + B u[k]`
    ///
    /// `y[k]     = C x[k] + D u[k]`
    pub fn new(
        a: Mat<T>,
        b: Mat<T>,
        c: Mat<T>,
        d: Mat<T>,
        sample_time: T::Real,
    ) -> Result<Self, StateSpaceError> {
        validate_blocks(
            a.nrows(),
            a.ncols(),
            b.nrows(),
            b.ncols(),
            c.nrows(),
            c.ncols(),
            d.nrows(),
            d.ncols(),
        )?;
        if !sample_time.is_finite() || sample_time <= <T::Real as Zero>::zero() {
            return Err(StateSpaceError::InvalidSampleTime);
        }
        Ok(Self {
            a,
            b,
            c,
            d,
            domain: DiscreteTime::new(sample_time),
        })
    }

    /// Creates a discrete-time model with zero feedthrough.
    pub fn with_zero_feedthrough(
        a: Mat<T>,
        b: Mat<T>,
        c: Mat<T>,
        sample_time: T::Real,
    ) -> Result<Self, StateSpaceError> {
        let d = Mat::zeros(c.nrows(), b.ncols());
        Self::new(a, b, c, d, sample_time)
    }

    /// Sample interval used by the discrete-time model.
    ///
    /// This is the spacing between state updates in the discrete-time
    /// interpretation of the system.
    #[must_use]
    pub fn sample_time(&self) -> T::Real {
        self.domain.sample_time()
    }
}

impl<T> DiscreteStateSpace<T>
where
    T: CompensatedField,
    T::Real: Float,
{
    /// Casts the dense discrete-time state-space matrices and sample interval
    /// to another scalar dtype.
    ///
    /// This is useful for comparing runtime sensitivity across precisions
    /// without changing the current state coordinates.
    pub fn try_cast<U>(&self) -> Result<DiscreteStateSpace<U>, StateSpaceError>
    where
        U: CompensatedField,
        U::Real: Float,
    {
        DiscreteStateSpace::new(
            cast_mat(self.a(), "state_space.a")?,
            cast_mat(self.b(), "state_space.b")?,
            cast_mat(self.c(), "state_space.c")?,
            cast_mat(self.d(), "state_space.d")?,
            NumCast::from(self.sample_time()).ok_or(StateSpaceError::ScalarConversionFailed {
                which: "state_space.sample_time",
            })?,
        )
    }
}

impl<T> DiscreteStateSpace<T>
where
    T: ComplexField + Copy,
    T::Real: Float,
{
    /// Creates the exact `samples`-step pure delay with the requested channel
    /// count.
    ///
    /// The returned system maps `u[k]` to `y[k] = u[k - samples]` and is
    /// realized exactly as a per-channel shift register. For `samples = 0`,
    /// the result is the zero-state identity map.
    pub fn delay(
        samples: usize,
        sample_time: T::Real,
        channels: usize,
    ) -> Result<Self, StateSpaceError> {
        if channels == 0 {
            return Self::new(
                Mat::zeros(0, 0),
                Mat::zeros(0, 0),
                Mat::zeros(0, 0),
                Mat::zeros(0, 0),
                sample_time,
            );
        }

        if samples == 0 {
            return Self::new(
                Mat::zeros(0, 0),
                Mat::zeros(0, channels),
                Mat::zeros(channels, 0),
                Mat::identity(channels, channels),
                sample_time,
            );
        }

        let nstates = samples * channels;
        let a = Mat::from_fn(nstates, nstates, |row, col| {
            let row_block = row / channels;
            let col_block = col / channels;
            if row_block == col_block + 1 && row % channels == col % channels {
                T::one()
            } else {
                T::zero()
            }
        });
        let b = Mat::from_fn(nstates, channels, |row, col| {
            if row < channels && row == col {
                T::one()
            } else {
                T::zero()
            }
        });
        let c = Mat::from_fn(channels, nstates, |row, col| {
            if col / channels == samples - 1 && row == col % channels {
                T::one()
            } else {
                T::zero()
            }
        });
        let d = Mat::zeros(channels, channels);
        Self::new(a, b, c, d, sample_time)
    }

    /// Forms the parallel sum of two discrete-time systems.
    pub fn parallel_add(&self, rhs: &Self) -> Result<Self, StateSpaceError> {
        ensure_sample_time_match(self.sample_time(), rhs.sample_time())?;
        let (a, b, c, d) = parallel_parts(self, rhs, false)?;
        Self::new(a, b, c, d, self.sample_time())
    }

    /// Forms the parallel difference `self - rhs` of two discrete-time
    /// systems.
    pub fn parallel_sub(&self, rhs: &Self) -> Result<Self, StateSpaceError> {
        ensure_sample_time_match(self.sample_time(), rhs.sample_time())?;
        let (a, b, c, d) = parallel_parts(self, rhs, true)?;
        Self::new(a, b, c, d, self.sample_time())
    }

    /// Forms the series interconnection `u -> self -> next`.
    pub fn series(&self, next: &Self) -> Result<Self, StateSpaceError> {
        ensure_sample_time_match(self.sample_time(), next.sample_time())?;
        let (a, b, c, d) = series_parts(self, next)?;
        Self::new(a, b, c, d, self.sample_time())
    }

    /// Forms the side-by-side block-diagonal append of two discrete-time
    /// systems.
    pub fn append(&self, rhs: &Self) -> Result<Self, StateSpaceError> {
        ensure_sample_time_match(self.sample_time(), rhs.sample_time())?;
        let (a, b, c, d) = append_parts(self, rhs);
        Self::new(a, b, c, d, self.sample_time())
    }

    /// Prepends an exact integer-sample delay to the plant input channels.
    ///
    /// This is equivalent to the series interconnection `delay -> self`.
    pub fn with_input_delay(&self, samples: usize) -> Result<Self, StateSpaceError> {
        if samples == 0 {
            return Ok(self.clone());
        }
        Self::delay(samples, self.sample_time(), self.ninputs())?.series(self)
    }

    /// Appends an exact integer-sample delay to the plant output channels.
    ///
    /// This is equivalent to the series interconnection `self -> delay`.
    pub fn with_output_delay(&self, samples: usize) -> Result<Self, StateSpaceError> {
        if samples == 0 {
            return Ok(self.clone());
        }
        self.series(&Self::delay(samples, self.sample_time(), self.noutputs())?)
    }

    /// Closes static state feedback with the convention `u = u_ext - K x`.
    pub fn with_state_feedback(&self, k: MatRef<'_, T>) -> Result<Self, StateSpaceError> {
        validate_state_feedback_gain(self, k)?;
        let bk = dense_mul(self.b(), k)?;
        let dk = dense_mul(self.d(), k)?;
        let a = self.a() - &bk;
        let c = self.c() - &dk;
        Self::new(
            a,
            self.b().to_owned(),
            c,
            self.d().to_owned(),
            self.sample_time(),
        )
    }

    /// Applies predictor-form output injection with innovation gain `L`.
    ///
    /// The returned system uses the concatenated external input `[u; y_ext]`
    /// and evolves according to
    ///
    /// `x[k+1] = (A - L C) x[k] + (B - L D) u[k] + L y_ext[k]`.
    pub fn with_output_injection(&self, l: MatRef<'_, T>) -> Result<Self, StateSpaceError> {
        let (a, b, c, d) = output_injection_parts(self, l)?;
        Self::new(a, b, c, d, self.sample_time())
    }

    /// Builds the observer-based controller realization and the corresponding
    /// augmented plant/controller closed-loop model.
    pub fn observer_controller_augmented(
        &self,
        k: MatRef<'_, T>,
        l: MatRef<'_, T>,
    ) -> Result<ObserverControllerComposition<T, DiscreteTime<T::Real>>, StateSpaceError> {
        let (controller, closed_loop) = observer_controller_parts(self, k, l)?;
        Ok(ObserverControllerComposition {
            controller: DiscreteStateSpace::new(
                controller.0,
                controller.1,
                controller.2,
                controller.3,
                self.sample_time(),
            )?,
            closed_loop: DiscreteStateSpace::new(
                closed_loop.0,
                closed_loop.1,
                closed_loop.2,
                closed_loop.3,
                self.sample_time(),
            )?,
        })
    }
}

impl<T> DiscreteStateSpace<T>
where
    T: CompensatedField,
    T::Real: Float + faer_traits::RealField,
{
    /// Computes the dense discrete-time controllability Gramian of the model.
    ///
    /// This measures how strongly the sampled input channels can drive the
    /// state through repeated applications of the one-step transition matrix.
    ///
    /// This is the dense reference path. It calls the Stein solver on
    /// `A` and `B` directly, so it is appropriate for modest dense models and
    /// for validating larger-scale discrete Gramian implementations.
    pub fn controllability_gramian(&self) -> Result<DenseSteinSolve<T>, SteinError> {
        controllability_gramian_discrete_dense(self.a.as_ref(), self.b.as_ref())
    }

    /// Computes the dense discrete-time observability Gramian of the model.
    ///
    /// This measures how strongly the internal state is visible at the outputs
    /// after repeated propagation through the sampled dynamics.
    ///
    /// Like the controllability path, this is intentionally dense-first. The
    /// result is useful for discrete balanced truncation and for checking the
    /// conditioning of sampled models before moving to reduced-order analysis.
    pub fn observability_gramian(&self) -> Result<DenseSteinSolve<T>, SteinError> {
        observability_gramian_discrete_dense(self.a.as_ref(), self.c.as_ref())
    }

    /// Converts the discrete-time model back into a continuous-time one using
    /// the requested reconstruction assumption.
    ///
    /// The method is explicit because `d2c` is not unique: a sampled model
    /// only corresponds to a continuous-time model after choosing how the input
    /// behaves between samples.
    pub fn continuousize(
        &self,
        method: ContinuousizationMethod<T::Real>,
    ) -> Result<ContinuousStateSpace<T>, StateSpaceError> {
        convert::continuousize(self, method)
    }

    /// Designs the dense infinite-horizon discrete-time DLQR controller.
    ///
    /// This is a convenience wrapper around [`crate::control::synthesis::dlqr_dense`] using
    /// the model's stored `A` and `B` blocks.
    pub fn dlqr(&self, q: MatRef<'_, T>, r: MatRef<'_, T>) -> Result<LqrSolve<T>, LqrError> {
        dlqr_dense(self.a.as_ref(), self.b.as_ref(), q, r)
    }
}

fn validate_blocks(
    a_nrows: usize,
    a_ncols: usize,
    b_nrows: usize,
    b_ncols: usize,
    c_nrows: usize,
    c_ncols: usize,
    d_nrows: usize,
    d_ncols: usize,
) -> Result<(), StateSpaceError> {
    // Constructor-time validation keeps every downstream control routine from
    // having to re-check the same `A/B/C/D` compatibility rules.
    if a_nrows != a_ncols {
        return Err(StateSpaceError::NonSquareA {
            nrows: a_nrows,
            ncols: a_ncols,
        });
    }
    if b_nrows != a_nrows {
        return Err(StateSpaceError::DimensionMismatch {
            which: "b",
            expected_nrows: a_nrows,
            expected_ncols: b_ncols,
            actual_nrows: b_nrows,
            actual_ncols: b_ncols,
        });
    }
    if c_ncols != a_ncols {
        return Err(StateSpaceError::DimensionMismatch {
            which: "c",
            expected_nrows: c_nrows,
            expected_ncols: a_ncols,
            actual_nrows: c_nrows,
            actual_ncols: c_ncols,
        });
    }
    if d_nrows != c_nrows || d_ncols != b_ncols {
        return Err(StateSpaceError::DimensionMismatch {
            which: "d",
            expected_nrows: c_nrows,
            expected_ncols: b_ncols,
            actual_nrows: d_nrows,
            actual_ncols: d_ncols,
        });
    }
    Ok(())
}

fn ensure_sample_time_match<R: Float>(lhs: R, rhs: R) -> Result<(), StateSpaceError> {
    // Sample times are stored as floating scalars, so a tiny tolerance avoids
    // turning harmless roundoff from conversions into composition failures.
    let scale = R::one().max(lhs.abs()).max(rhs.abs());
    let tol = R::epsilon() * scale * R::from(128.0).unwrap_or_else(R::one);
    if (lhs - rhs).abs() <= tol {
        Ok(())
    } else {
        Err(StateSpaceError::MismatchedSampleTime)
    }
}

fn parallel_parts<T, Domain>(
    lhs: &StateSpace<T, Domain>,
    rhs: &StateSpace<T, Domain>,
    subtract_rhs: bool,
) -> Result<(Mat<T>, Mat<T>, Mat<T>, Mat<T>), StateSpaceError>
where
    T: ComplexField + Copy,
{
    ensure_same_io(
        lhs,
        rhs,
        if subtract_rhs {
            "parallel_sub"
        } else {
            "parallel_add"
        },
    )?;
    let a = block_diag(lhs.a(), rhs.a());
    let b = vcat(lhs.b(), rhs.b())?;
    let rhs_c = if subtract_rhs {
        negated(rhs.c())
    } else {
        rhs.c().to_owned()
    };
    let rhs_d = if subtract_rhs {
        negated(rhs.d())
    } else {
        rhs.d().to_owned()
    };
    // Parallel composition keeps a shared input and stacks the two internal
    // state vectors. The output map is then the horizontal concatenation of
    // the two output maps, with the right side optionally negated.
    let c = hcat(lhs.c(), rhs_c.as_ref())?;
    let d = lhs.d() + &rhs_d;
    Ok((a, b, c, d))
}

fn series_parts<T, Domain>(
    lhs: &StateSpace<T, Domain>,
    rhs: &StateSpace<T, Domain>,
) -> Result<(Mat<T>, Mat<T>, Mat<T>, Mat<T>), StateSpaceError>
where
    T: ComplexField + Copy,
{
    if lhs.noutputs() != rhs.ninputs() {
        return Err(StateSpaceError::DimensionMismatch {
            which: "series.io",
            expected_nrows: lhs.noutputs(),
            expected_ncols: 1,
            actual_nrows: rhs.ninputs(),
            actual_ncols: 1,
        });
    }

    let top_right = Mat::zeros(lhs.nstates(), rhs.nstates());
    let bottom_left = dense_mul(rhs.b(), lhs.c())?;
    // The series state is `[x_lhs; x_rhs]`. The lower-left block carries the
    // fact that the second subsystem is driven by the first subsystem's output
    // `y_lhs = C_lhs x_lhs + D_lhs u`.
    let a = block_matrix2x2(lhs.a(), top_right.as_ref(), bottom_left.as_ref(), rhs.a())?;
    let b_lower = dense_mul(rhs.b(), lhs.d())?;
    let b = vcat(lhs.b(), b_lower.as_ref())?;
    let c_left = dense_mul(rhs.d(), lhs.c())?;
    let c = hcat(c_left.as_ref(), rhs.c())?;
    let d = dense_mul(rhs.d(), lhs.d())?;
    Ok((a, b, c, d))
}

fn append_parts<T, Domain>(
    lhs: &StateSpace<T, Domain>,
    rhs: &StateSpace<T, Domain>,
) -> (Mat<T>, Mat<T>, Mat<T>, Mat<T>)
where
    T: ComplexField + Copy,
{
    // Append is the pure block-diagonal structural primitive: it keeps the two
    // systems independent and simply concatenates their state, input, and
    // output channels.
    (
        block_diag(lhs.a(), rhs.a()),
        block_diag(lhs.b(), rhs.b()),
        block_diag(lhs.c(), rhs.c()),
        block_diag(lhs.d(), rhs.d()),
    )
}

fn output_injection_parts<T, Domain>(
    system: &StateSpace<T, Domain>,
    l: MatRef<'_, T>,
) -> Result<(Mat<T>, Mat<T>, Mat<T>, Mat<T>), StateSpaceError>
where
    T: ComplexField + Copy,
{
    validate_output_injection_gain(system, l)?;
    let lc = dense_mul(l, system.c())?;
    let ld = dense_mul(l, system.d())?;
    let a = system.a() - &lc;
    let u_block = system.b() - &ld;
    let y_block = l.to_owned();
    // The injected system is driven by `[u; y_ext]`, not just `u`. This keeps
    // the innovation input explicit instead of hiding the measurement channel
    // inside a special estimator-only type.
    let b = hcat(u_block.as_ref(), y_block.as_ref())?;
    let d = hcat(
        system.d(),
        Mat::<T>::zeros(system.noutputs(), system.noutputs()).as_ref(),
    )?;
    Ok((a, b, system.c().to_owned(), d))
}

type Parts<T> = (Mat<T>, Mat<T>, Mat<T>, Mat<T>);

fn observer_controller_parts<T, Domain>(
    plant: &StateSpace<T, Domain>,
    k: MatRef<'_, T>,
    l: MatRef<'_, T>,
) -> Result<(Parts<T>, Parts<T>), StateSpaceError>
where
    T: ComplexField + Copy,
{
    validate_state_feedback_gain(plant, k)?;
    validate_output_injection_gain(plant, l)?;

    let bk = dense_mul(plant.b(), k)?;
    let lc = dense_mul(l, plant.c())?;
    let ld = dense_mul(l, plant.d())?;
    let ldk = dense_mul(ld.as_ref(), k)?;

    // The standalone controller state is the observer state `x_hat`, driven by
    // reference `r` and measurement `y`, with control law `u = r - K x_hat`.
    let controller_a = (plant.a() - &bk - &lc) + &ldk;
    let controller_b_r = plant.b() - &ld;
    let controller_b = hcat(controller_b_r.as_ref(), l)?;
    let controller_c = negated(k);
    let controller_d = hcat(
        Mat::identity(plant.ninputs(), plant.ninputs()).as_ref(),
        Mat::<T>::zeros(plant.ninputs(), plant.noutputs()).as_ref(),
    )?;

    let top_right = negated(bk.as_ref());
    let bottom_right = plant.a() - &bk - &lc;
    // The closed-loop augmented state is `[x; x_hat]`, driven only by the
    // external reference/disturbance input `r`. The plant sees the estimated
    // state through `u = r - K x_hat`, while the observer sees the true plant
    // output through the innovation term.
    let closed_loop_a = block_matrix2x2(
        plant.a(),
        top_right.as_ref(),
        lc.as_ref(),
        bottom_right.as_ref(),
    )?;
    let closed_loop_b = vcat(plant.b(), plant.b())?;
    let closed_loop_c = hcat(
        plant.c(),
        negated(dense_mul(plant.d(), k)?.as_ref()).as_ref(),
    )?;
    let closed_loop_d = plant.d().to_owned();

    Ok((
        (controller_a, controller_b, controller_c, controller_d),
        (closed_loop_a, closed_loop_b, closed_loop_c, closed_loop_d),
    ))
}

fn validate_state_feedback_gain<T, Domain>(
    system: &StateSpace<T, Domain>,
    k: MatRef<'_, T>,
) -> Result<(), StateSpaceError> {
    // State feedback always maps state to input, so `K` has shape
    // `ninputs x nstates`.
    if k.nrows() != system.ninputs() || k.ncols() != system.nstates() {
        return Err(StateSpaceError::DimensionMismatch {
            which: "state_feedback_gain",
            expected_nrows: system.ninputs(),
            expected_ncols: system.nstates(),
            actual_nrows: k.nrows(),
            actual_ncols: k.ncols(),
        });
    }
    Ok(())
}

fn validate_output_injection_gain<T, Domain>(
    system: &StateSpace<T, Domain>,
    l: MatRef<'_, T>,
) -> Result<(), StateSpaceError> {
    // Output injection maps measured output back into state coordinates, so
    // `L` has shape `nstates x noutputs`.
    if l.nrows() != system.nstates() || l.ncols() != system.noutputs() {
        return Err(StateSpaceError::DimensionMismatch {
            which: "output_injection_gain",
            expected_nrows: system.nstates(),
            expected_ncols: system.noutputs(),
            actual_nrows: l.nrows(),
            actual_ncols: l.ncols(),
        });
    }
    Ok(())
}

fn ensure_same_io<T, Domain>(
    lhs: &StateSpace<T, Domain>,
    rhs: &StateSpace<T, Domain>,
    which: &'static str,
) -> Result<(), StateSpaceError> {
    if lhs.ninputs() != rhs.ninputs() {
        return Err(StateSpaceError::DimensionMismatch {
            which,
            expected_nrows: lhs.ninputs(),
            expected_ncols: 1,
            actual_nrows: rhs.ninputs(),
            actual_ncols: 1,
        });
    }
    if lhs.noutputs() != rhs.noutputs() {
        return Err(StateSpaceError::DimensionMismatch {
            which,
            expected_nrows: lhs.noutputs(),
            expected_ncols: 1,
            actual_nrows: rhs.noutputs(),
            actual_ncols: 1,
        });
    }
    Ok(())
}

fn dense_mul<T>(lhs: MatRef<'_, T>, rhs: MatRef<'_, T>) -> Result<Mat<T>, StateSpaceError>
where
    T: ComplexField + Copy,
{
    if lhs.ncols() != rhs.nrows() {
        return Err(StateSpaceError::DimensionMismatch {
            which: "dense_mul",
            expected_nrows: lhs.ncols(),
            expected_ncols: 1,
            actual_nrows: rhs.nrows(),
            actual_ncols: 1,
        });
    }
    // The composition layer uses straightforward dense kernels here because
    // these helpers are only for assembling modest dense interconnections, not
    // for replacing the lower-level optimized linear algebra stack.
    Ok(dense_mul_plain(lhs, rhs))
}

/// Casts one dense matrix between scalar dtypes with explicit validation.
///
/// The helper validates every entry before allocating the destination matrix
/// so callers get a named `ScalarConversionFailed` error instead of a partial
/// conversion.
fn cast_mat<T, U>(matrix: MatRef<'_, T>, which: &'static str) -> Result<Mat<U>, StateSpaceError>
where
    T: CompensatedField,
    U: CompensatedField,
    T::Real: Float,
    U::Real: Float,
{
    // Validate first so the second pass can use infallible casts while keeping
    // the matrix construction itself allocation-only.
    for row in 0..matrix.nrows() {
        for col in 0..matrix.ncols() {
            let value = matrix[(row, col)];
            let _: U::Real = NumCast::from(value.real())
                .ok_or(StateSpaceError::ScalarConversionFailed { which })?;
            if U::IS_REAL && value.imag() != <T::Real as Zero>::zero() {
                return Err(StateSpaceError::ScalarConversionFailed { which });
            }
            let _: U::Real = NumCast::from(value.imag())
                .ok_or(StateSpaceError::ScalarConversionFailed { which })?;
        }
    }

    Ok(Mat::from_fn(matrix.nrows(), matrix.ncols(), |row, col| {
        let value = matrix[(row, col)];
        let real = NumCast::from(value.real()).expect("matrix entries validated before cast");
        let imag = NumCast::from(value.imag()).expect("matrix entries validated before cast");
        U::from_real_imag(real, imag)
    }))
}

fn negated<T>(matrix: MatRef<'_, T>) -> Mat<T>
where
    T: ComplexField + Copy,
{
    Mat::from_fn(matrix.nrows(), matrix.ncols(), |row, col| {
        -matrix[(row, col)]
    })
}

fn hcat<T>(lhs: MatRef<'_, T>, rhs: MatRef<'_, T>) -> Result<Mat<T>, StateSpaceError>
where
    T: ComplexField + Copy,
{
    if lhs.nrows() != rhs.nrows() {
        return Err(StateSpaceError::DimensionMismatch {
            which: "hcat",
            expected_nrows: lhs.nrows(),
            expected_ncols: 1,
            actual_nrows: rhs.nrows(),
            actual_ncols: 1,
        });
    }
    Ok(Mat::from_fn(
        lhs.nrows(),
        lhs.ncols() + rhs.ncols(),
        |row, col| {
            if col < lhs.ncols() {
                lhs[(row, col)]
            } else {
                rhs[(row, col - lhs.ncols())]
            }
        },
    ))
}

fn vcat<T>(lhs: MatRef<'_, T>, rhs: MatRef<'_, T>) -> Result<Mat<T>, StateSpaceError>
where
    T: ComplexField + Copy,
{
    if lhs.ncols() != rhs.ncols() {
        return Err(StateSpaceError::DimensionMismatch {
            which: "vcat",
            expected_nrows: 1,
            expected_ncols: lhs.ncols(),
            actual_nrows: 1,
            actual_ncols: rhs.ncols(),
        });
    }
    Ok(Mat::from_fn(
        lhs.nrows() + rhs.nrows(),
        lhs.ncols(),
        |row, col| {
            if row < lhs.nrows() {
                lhs[(row, col)]
            } else {
                rhs[(row - lhs.nrows(), col)]
            }
        },
    ))
}

fn block_diag<T>(lhs: MatRef<'_, T>, rhs: MatRef<'_, T>) -> Mat<T>
where
    T: ComplexField + Copy,
{
    Mat::from_fn(
        lhs.nrows() + rhs.nrows(),
        lhs.ncols() + rhs.ncols(),
        |row, col| {
            if row < lhs.nrows() && col < lhs.ncols() {
                lhs[(row, col)]
            } else if row >= lhs.nrows() && col >= lhs.ncols() {
                rhs[(row - lhs.nrows(), col - lhs.ncols())]
            } else {
                T::zero()
            }
        },
    )
}

fn block_matrix2x2<T>(
    top_left: MatRef<'_, T>,
    top_right: MatRef<'_, T>,
    bottom_left: MatRef<'_, T>,
    bottom_right: MatRef<'_, T>,
) -> Result<Mat<T>, StateSpaceError>
where
    T: ComplexField + Copy,
{
    // The structural composition formulas naturally produce 2x2 block
    // matrices. Centralizing that assembly here keeps the public methods
    // readable and makes the dimension checks uniform.
    if top_left.nrows() != top_right.nrows()
        || bottom_left.nrows() != bottom_right.nrows()
        || top_left.ncols() != bottom_left.ncols()
        || top_right.ncols() != bottom_right.ncols()
    {
        return Err(StateSpaceError::DimensionMismatch {
            which: "block_matrix2x2",
            expected_nrows: top_left.nrows() + bottom_left.nrows(),
            expected_ncols: top_left.ncols() + top_right.ncols(),
            actual_nrows: top_right.nrows() + bottom_right.nrows(),
            actual_ncols: bottom_left.ncols() + bottom_right.ncols(),
        });
    }

    Ok(Mat::from_fn(
        top_left.nrows() + bottom_left.nrows(),
        top_left.ncols() + top_right.ncols(),
        |row, col| {
            if row < top_left.nrows() {
                if col < top_left.ncols() {
                    top_left[(row, col)]
                } else {
                    top_right[(row, col - top_left.ncols())]
                }
            } else if col < top_left.ncols() {
                bottom_left[(row - top_left.nrows(), col)]
            } else {
                bottom_right[(row - top_left.nrows(), col - top_left.ncols())]
            }
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::{
        ContinuousStateSpace, ContinuousizationMethod, DiscreteStateSpace, DiscretizationMethod,
        StateSpaceError,
    };
    use crate::control::lti::ContinuousTransferFunction;
    use faer::{Mat, c64};
    use faer_traits::ext::ComplexFieldExt;

    fn assert_close(lhs: &Mat<f64>, rhs: &Mat<f64>, tol: f64) {
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

    fn assert_close_c64(lhs: &Mat<c64>, rhs: &Mat<c64>, tol: f64) {
        assert_eq!(lhs.nrows(), rhs.nrows());
        assert_eq!(lhs.ncols(), rhs.ncols());
        for col in 0..lhs.ncols() {
            for row in 0..lhs.nrows() {
                let err = (lhs[(row, col)] - rhs[(row, col)]).abs1();
                assert!(
                    err <= tol,
                    "entry ({row}, {col}) differs: lhs={:?}, rhs={:?}, err={err}, tol={tol}",
                    lhs[(row, col)],
                    rhs[(row, col)],
                );
            }
        }
    }

    #[test]
    fn continuous_constructor_rejects_bad_dimensions() {
        let a = Mat::<f64>::identity(2, 2);
        let b = Mat::<f64>::zeros(3, 1);
        let c = Mat::<f64>::zeros(1, 2);
        let d = Mat::<f64>::zeros(1, 1);
        let err = ContinuousStateSpace::new(a, b, c, d).unwrap_err();
        assert!(matches!(
            err,
            StateSpaceError::DimensionMismatch { which: "b", .. }
        ));
    }

    #[test]
    fn discrete_constructor_rejects_invalid_sample_time() {
        let a = Mat::<f64>::identity(1, 1);
        let b = Mat::<f64>::zeros(1, 1);
        let c = Mat::<f64>::zeros(1, 1);
        let d = Mat::<f64>::zeros(1, 1);
        let err = DiscreteStateSpace::new(a, b, c, d, 0.0).unwrap_err();
        assert_eq!(err, StateSpaceError::InvalidSampleTime);
    }

    #[test]
    fn zero_feedthrough_constructor_sizes_d_correctly() {
        let a = Mat::<f64>::identity(2, 2);
        let b = Mat::<f64>::zeros(2, 3);
        let c = Mat::<f64>::zeros(4, 2);
        let sys = ContinuousStateSpace::with_zero_feedthrough(a, b, c).unwrap();
        assert_eq!(sys.d().nrows(), 4);
        assert_eq!(sys.d().ncols(), 3);
    }

    #[test]
    fn zoh_discretize_matches_diagonal_closed_form_real_case() {
        let a = Mat::from_fn(2, 2, |row, col| match (row, col) {
            (0, 0) => -1.0,
            (1, 1) => -2.0,
            _ => 0.0,
        });
        let b = Mat::from_fn(2, 2, |row, col| if row == col { 1.0 } else { 0.0 });
        let c = Mat::<f64>::identity(2, 2);
        let d = Mat::<f64>::zeros(2, 2);
        let sys = ContinuousStateSpace::new(a, b, c, d).unwrap();

        let dt = 0.1;
        let disc = sys
            .discretize(dt, DiscretizationMethod::ZeroOrderHold)
            .unwrap();

        let expected_a = Mat::from_fn(2, 2, |row, col| match (row, col) {
            (0, 0) => (-dt).exp(),
            (1, 1) => (-2.0 * dt).exp(),
            _ => 0.0,
        });
        let expected_b = Mat::from_fn(2, 2, |row, col| match (row, col) {
            (0, 0) => 1.0 - (-dt).exp(),
            (1, 1) => (1.0 - (-2.0 * dt).exp()) / 2.0,
            _ => 0.0,
        });

        assert_close(&disc.a, &expected_a, 1e-12);
        assert_close(&disc.b, &expected_b, 1e-12);
    }

    #[test]
    fn bilinear_round_trip_recovers_original_real_system() {
        let a = Mat::from_fn(2, 2, |row, col| match (row, col) {
            (0, 0) => -2.0,
            (0, 1) => 1.0,
            (1, 0) => 0.0,
            (1, 1) => -3.0,
            _ => 0.0,
        });
        let b = Mat::from_fn(2, 1, |row, _| if row == 0 { 1.0 } else { -0.25 });
        let c = Mat::from_fn(1, 2, |_, col| if col == 0 { 0.5 } else { -1.0 });
        let d = Mat::from_fn(1, 1, |_, _| 0.2);
        let sys = ContinuousStateSpace::new(a, b, c, d).unwrap();

        let dt = 0.05;
        let disc = sys
            .discretize(
                dt,
                DiscretizationMethod::Bilinear {
                    prewarp_frequency: None,
                },
            )
            .unwrap();
        let recovered = disc
            .continuousize(ContinuousizationMethod::Bilinear {
                prewarp_frequency: None,
            })
            .unwrap();

        assert_close(&recovered.a, &sys.a, 1e-11);
        assert_close(&recovered.b, &sys.b, 1e-11);
        assert_close(&recovered.c, &sys.c, 1e-11);
        assert_close(&recovered.d, &sys.d, 1e-11);
    }

    #[test]
    fn continuous_parallel_and_series_match_transfer_function_arithmetic() {
        let lhs = ContinuousTransferFunction::continuous(vec![1.0], vec![1.0, 1.0]).unwrap();
        let rhs = ContinuousTransferFunction::continuous(vec![2.0], vec![1.0, 2.0]).unwrap();

        let lhs_ss = lhs.to_state_space().unwrap();
        let rhs_ss = rhs.to_state_space().unwrap();

        let parallel = lhs_ss
            .parallel_add(&rhs_ss)
            .unwrap()
            .to_transfer_function()
            .unwrap();
        let series = lhs_ss
            .series(&rhs_ss)
            .unwrap()
            .to_transfer_function()
            .unwrap();

        let parallel_tf = lhs.add(&rhs).unwrap();
        let series_tf = lhs.mul(&rhs).unwrap();

        assert_close(
            &Mat::from_fn(parallel.numerator().len(), 1, |row, _| {
                parallel.numerator()[row]
            }),
            &Mat::from_fn(parallel_tf.numerator().len(), 1, |row, _| {
                parallel_tf.numerator()[row]
            }),
            1.0e-10,
        );
        assert_close(
            &Mat::from_fn(parallel.denominator().len(), 1, |row, _| {
                parallel.denominator()[row]
            }),
            &Mat::from_fn(parallel_tf.denominator().len(), 1, |row, _| {
                parallel_tf.denominator()[row]
            }),
            1.0e-10,
        );
        assert_close(
            &Mat::from_fn(series.numerator().len(), 1, |row, _| {
                series.numerator()[row]
            }),
            &Mat::from_fn(series_tf.numerator().len(), 1, |row, _| {
                series_tf.numerator()[row]
            }),
            1.0e-10,
        );
        assert_close(
            &Mat::from_fn(series.denominator().len(), 1, |row, _| {
                series.denominator()[row]
            }),
            &Mat::from_fn(series_tf.denominator().len(), 1, |row, _| {
                series_tf.denominator()[row]
            }),
            1.0e-10,
        );
    }

    #[test]
    fn discrete_structural_composition_rejects_sample_time_mismatch() {
        let lhs = DiscreteStateSpace::with_zero_feedthrough(
            Mat::from_fn(1, 1, |_, _| 0.5f64),
            Mat::from_fn(1, 1, |_, _| 1.0),
            Mat::from_fn(1, 1, |_, _| 1.0),
            0.1,
        )
        .unwrap();
        let rhs = DiscreteStateSpace::with_zero_feedthrough(
            Mat::from_fn(1, 1, |_, _| 0.25f64),
            Mat::from_fn(1, 1, |_, _| 1.0),
            Mat::from_fn(1, 1, |_, _| 1.0),
            0.2,
        )
        .unwrap();

        let err = lhs.parallel_add(&rhs).unwrap_err();
        assert_eq!(err, StateSpaceError::MismatchedSampleTime);
    }

    #[test]
    fn pure_discrete_delay_simulation_matches_shifted_signal() {
        let delay = DiscreteStateSpace::<f64>::delay(2, 0.1, 2).unwrap();
        let inputs = Mat::from_fn(2, 5, |row, col| match (row, col) {
            (0, 0) => 1.0,
            (0, 1) => 2.0,
            (0, 2) => 3.0,
            (0, 3) => 4.0,
            (0, 4) => 5.0,
            (1, 0) => -1.0,
            (1, 1) => -2.0,
            (1, 2) => -3.0,
            (1, 3) => -4.0,
            _ => -5.0,
        });
        let sim = delay
            .simulate(&vec![0.0; delay.nstates()], inputs.as_ref())
            .unwrap();
        let expected = Mat::from_fn(
            2,
            5,
            |row, col| {
                if col < 2 { 0.0 } else { inputs[(row, col - 2)] }
            },
        );

        assert_eq!(delay.sample_time(), 0.1);
        assert_close(&sim.outputs, &expected, 1.0e-12);
    }

    #[test]
    fn zero_sample_delay_is_identity_map() {
        let delay = DiscreteStateSpace::<f64>::delay(0, 0.1, 1).unwrap();
        let inputs = Mat::from_fn(1, 4, |_, col| [1.0, -2.0, 3.0, -4.0][col]);
        let sim = delay.simulate(&[], inputs.as_ref()).unwrap();

        assert_eq!(delay.nstates(), 0);
        assert_close(&sim.outputs, &inputs, 1.0e-12);
    }

    #[test]
    fn input_and_output_delay_helpers_match_shifted_reference_sequences() {
        let plant = DiscreteStateSpace::new(
            Mat::from_fn(1, 1, |_, _| 0.5),
            Mat::from_fn(1, 1, |_, _| 1.0),
            Mat::from_fn(1, 1, |_, _| 1.0),
            Mat::from_fn(1, 1, |_, _| 0.25),
            0.1,
        )
        .unwrap();
        let inputs = Mat::from_fn(1, 6, |_, col| [1.0, 2.0, -1.0, 0.5, 3.0, -2.0][col]);

        let delayed_input = plant.with_input_delay(2).unwrap();
        let shifted_inputs = Mat::from_fn(1, inputs.ncols(), |_, col| {
            if col < 2 { 0.0 } else { inputs[(0, col - 2)] }
        });
        let delayed_input_sim = delayed_input
            .simulate(&vec![0.0; delayed_input.nstates()], inputs.as_ref())
            .unwrap();
        let shifted_input_sim = plant
            .simulate(&vec![0.0; plant.nstates()], shifted_inputs.as_ref())
            .unwrap();
        assert_close(
            &delayed_input_sim.outputs,
            &shifted_input_sim.outputs,
            1.0e-12,
        );

        let delayed_output = plant.with_output_delay(2).unwrap();
        let plant_sim = plant
            .simulate(&vec![0.0; plant.nstates()], inputs.as_ref())
            .unwrap();
        let delayed_output_sim = delayed_output
            .simulate(&vec![0.0; delayed_output.nstates()], inputs.as_ref())
            .unwrap();
        let expected_outputs = Mat::from_fn(1, plant_sim.outputs.ncols(), |_, col| {
            if col < 2 {
                0.0
            } else {
                plant_sim.outputs[(0, col - 2)]
            }
        });
        assert_close(&delayed_output_sim.outputs, &expected_outputs, 1.0e-12);
    }

    #[test]
    fn state_feedback_matches_lqr_closed_loop_matrix() {
        let a = Mat::from_fn(1, 1, |_, _| 1.0f64);
        let b = Mat::from_fn(1, 1, |_, _| 1.0f64);
        let c = Mat::from_fn(1, 1, |_, _| 3.0f64);
        let d = Mat::from_fn(1, 1, |_, _| 2.0f64);
        let sys = ContinuousStateSpace::new(a, b, c, d).unwrap();
        let q = Mat::from_fn(1, 1, |_, _| 1.0f64);
        let r = Mat::from_fn(1, 1, |_, _| 1.0f64);
        let lqr = sys.lqr(q.as_ref(), r.as_ref()).unwrap();

        let closed = sys.with_state_feedback(lqr.gain.as_ref()).unwrap();
        assert_close(&closed.a, &lqr.closed_loop_a, 1.0e-12);
        assert_close(&closed.b, &sys.b, 1.0e-12);
        assert_close(
            &closed.c,
            &Mat::from_fn(1, 1, |_, _| {
                sys.c[(0, 0)] - sys.d[(0, 0)] * lqr.gain[(0, 0)]
            }),
            1.0e-12,
        );
        assert_close(&closed.d, &sys.d, 1.0e-12);
    }

    #[test]
    fn output_injection_matches_lqe_estimator_dynamics() {
        let a = Mat::from_fn(1, 1, |_, _| 1.0f64);
        let b = Mat::from_fn(1, 1, |_, _| 2.0f64);
        let c = Mat::from_fn(1, 1, |_, _| 3.0f64);
        let d = Mat::from_fn(1, 1, |_, _| 4.0f64);
        let sys = ContinuousStateSpace::new(a, b, c, d).unwrap();
        let w = Mat::from_fn(1, 1, |_, _| 1.0f64);
        let v = Mat::from_fn(1, 1, |_, _| 1.0f64);
        let lqe = sys.lqe(w.as_ref(), v.as_ref()).unwrap();

        let injected = sys.with_output_injection(lqe.gain.as_ref()).unwrap();
        assert_close(&injected.a, &lqe.estimator_a, 1.0e-12);
        assert_close(
            &injected.b,
            &Mat::from_fn(1, 2, |_, col| {
                if col == 0 {
                    sys.b[(0, 0)] - lqe.gain[(0, 0)] * sys.d[(0, 0)]
                } else {
                    lqe.gain[(0, 0)]
                }
            }),
            1.0e-12,
        );
        assert_close(&injected.c, &sys.c, 1.0e-12);
        assert_close(
            &injected.d,
            &Mat::from_fn(1, 2, |_, col| if col == 0 { sys.d[(0, 0)] } else { 0.0 }),
            1.0e-12,
        );
    }

    #[test]
    fn observer_controller_augmented_matches_scalar_manual_formula() {
        let plant = ContinuousStateSpace::new(
            Mat::from_fn(1, 1, |_, _| 1.0f64),
            Mat::from_fn(1, 1, |_, _| 2.0f64),
            Mat::from_fn(1, 1, |_, _| 3.0f64),
            Mat::from_fn(1, 1, |_, _| 4.0f64),
        )
        .unwrap();
        let k = Mat::from_fn(1, 1, |_, _| 5.0f64);
        let l = Mat::from_fn(1, 1, |_, _| 6.0f64);

        let composed = plant
            .observer_controller_augmented(k.as_ref(), l.as_ref())
            .unwrap();

        assert_close(
            &composed.controller.a,
            &Mat::from_fn(1, 1, |_, _| 93.0),
            1.0e-12,
        );
        assert_close(
            &composed.controller.b,
            &Mat::from_fn(1, 2, |_, col| if col == 0 { -22.0 } else { 6.0 }),
            1.0e-12,
        );
        assert_close(
            &composed.controller.c,
            &Mat::from_fn(1, 1, |_, _| -5.0),
            1.0e-12,
        );
        assert_close(
            &composed.controller.d,
            &Mat::from_fn(1, 2, |_, col| if col == 0 { 1.0 } else { 0.0 }),
            1.0e-12,
        );

        assert_close(
            &composed.closed_loop.a,
            &Mat::from_fn(2, 2, |row, col| match (row, col) {
                (0, 0) => 1.0,
                (0, 1) => -10.0,
                (1, 0) => 18.0,
                (1, 1) => -27.0,
                _ => 0.0,
            }),
            1.0e-12,
        );
        assert_close(
            &composed.closed_loop.b,
            &Mat::from_fn(2, 1, |_, _| 2.0),
            1.0e-12,
        );
        assert_close(
            &composed.closed_loop.c,
            &Mat::from_fn(1, 2, |_, col| if col == 0 { 3.0 } else { -20.0 }),
            1.0e-12,
        );
        assert_close(
            &composed.closed_loop.d,
            &Mat::from_fn(1, 1, |_, _| 4.0),
            1.0e-12,
        );
    }

    #[test]
    fn bilinear_round_trip_recovers_original_complex_system() {
        let a = Mat::from_fn(1, 1, |_, _| c64::new(-1.0, 0.25));
        let b = Mat::from_fn(1, 1, |_, _| c64::new(0.5, -0.1));
        let c = Mat::from_fn(1, 1, |_, _| c64::new(1.25, 0.4));
        let d = Mat::from_fn(1, 1, |_, _| c64::new(-0.2, 0.1));
        let sys = ContinuousStateSpace::new(a, b, c, d).unwrap();

        let disc = sys
            .discretize(
                0.1,
                DiscretizationMethod::Bilinear {
                    prewarp_frequency: None,
                },
            )
            .unwrap();
        let recovered = disc
            .continuousize(ContinuousizationMethod::Bilinear {
                prewarp_frequency: None,
            })
            .unwrap();

        assert_close_c64(&recovered.a, &sys.a, 1e-11);
        assert_close_c64(&recovered.b, &sys.b, 1e-11);
        assert_close_c64(&recovered.c, &sys.c, 1e-11);
        assert_close_c64(&recovered.d, &sys.d, 1e-11);
    }

    #[test]
    fn discrete_state_space_try_cast_to_f32_preserves_sample_time_and_blocks() {
        let sys = DiscreteStateSpace::new(
            Mat::from_fn(2, 2, |row, col| match (row, col) {
                (0, 0) => 0.9,
                (0, 1) => 0.2,
                (1, 0) => -0.1,
                _ => 0.8,
            }),
            Mat::from_fn(2, 1, |row, _| if row == 0 { 0.5 } else { -0.25 }),
            Mat::from_fn(1, 2, |_, col| if col == 0 { 1.0 } else { -0.3 }),
            Mat::from_fn(1, 1, |_, _| 0.125),
            0.05,
        )
        .unwrap();

        let cast = sys.try_cast::<f32>().unwrap();

        assert!((cast.sample_time() - 0.05f32).abs() <= 1.0e-6);
        assert!((cast.a()[(0, 0)] - 0.9f32).abs() <= 1.0e-6);
        assert!((cast.a()[(0, 1)] - 0.2f32).abs() <= 1.0e-6);
        assert!((cast.a()[(1, 0)] + 0.1f32).abs() <= 1.0e-6);
        assert!((cast.a()[(1, 1)] - 0.8f32).abs() <= 1.0e-6);
        assert!((cast.b()[(0, 0)] - 0.5f32).abs() <= 1.0e-6);
        assert!((cast.b()[(1, 0)] + 0.25f32).abs() <= 1.0e-6);
        assert!((cast.c()[(0, 0)] - 1.0f32).abs() <= 1.0e-6);
        assert!((cast.c()[(0, 1)] + 0.3f32).abs() <= 1.0e-6);
        assert!((cast.d()[(0, 0)] - 0.125f32).abs() <= 1.0e-6);
    }

    #[test]
    fn state_space_try_cast_rejects_complex_to_real_data_loss() {
        let sys = ContinuousStateSpace::new(
            Mat::from_fn(1, 1, |_, _| c64::new(-1.0, 0.25)),
            Mat::from_fn(1, 1, |_, _| c64::new(1.0, 0.0)),
            Mat::from_fn(1, 1, |_, _| c64::new(1.0, 0.0)),
            Mat::from_fn(1, 1, |_, _| c64::new(0.0, 0.0)),
        )
        .unwrap();

        let err = sys.try_cast::<f64>().unwrap_err();

        assert_eq!(
            err,
            StateSpaceError::ScalarConversionFailed {
                which: "state_space.a"
            }
        );
    }

    #[test]
    fn continuous_gramian_adapters_call_existing_dense_lyapunov_paths() {
        let a = Mat::from_fn(2, 2, |row, col| match (row, col) {
            (0, 0) => -1.0,
            (1, 1) => -2.0,
            _ => 0.0,
        });
        let b = Mat::<f64>::identity(2, 2);
        let c = Mat::<f64>::identity(2, 2);
        let sys = ContinuousStateSpace::with_zero_feedthrough(a, b, c).unwrap();

        let wc = sys.controllability_gramian().unwrap();
        let wo = sys.observability_gramian().unwrap();
        assert!(wc.residual_norm <= 1e-12);
        assert!(wo.residual_norm <= 1e-12);
    }

    #[test]
    fn discrete_gramian_adapters_call_dense_stein_paths() {
        let a = Mat::from_fn(2, 2, |row, col| match (row, col) {
            (0, 0) => 0.25,
            (1, 1) => -0.5,
            _ => 0.0,
        });
        let b = Mat::<f64>::identity(2, 2);
        let c = Mat::<f64>::identity(2, 2);
        let sys = DiscreteStateSpace::with_zero_feedthrough(a, b, c, 0.1).unwrap();

        let wc = sys.controllability_gramian().unwrap();
        let wo = sys.observability_gramian().unwrap();
        assert!(wc.residual_norm <= 1e-12);
        assert!(wo.residual_norm <= 1e-12);
    }

    #[test]
    fn exact_zoh_d2c_is_explicitly_unsupported_for_now() {
        let a = Mat::<f64>::identity(1, 1);
        let b = Mat::<f64>::zeros(1, 1);
        let c = Mat::<f64>::zeros(1, 1);
        let d = Mat::<f64>::zeros(1, 1);
        let sys = DiscreteStateSpace::new(a, b, c, d, 0.1).unwrap();
        let err = sys
            .continuousize(ContinuousizationMethod::ZeroOrderHold)
            .unwrap_err();
        assert!(matches!(err, StateSpaceError::UnsupportedConversion(_)));
    }
}
