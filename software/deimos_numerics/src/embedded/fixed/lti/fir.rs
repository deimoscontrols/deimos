//! Fixed-size, allocation-free FIR filter banks.
//!
//! # Glossary
//!
//! - **FIR:** Finite impulse response filter.
//! - **Tap:** One coefficient multiplying one delayed sample.

use crate::embedded::error::EmbeddedError;
use crate::embedded::math::ensure_finite;
use num_traits::{Float, NumCast};

/// Fixed-size FIR taps shared across `LANES` independent channels.
///
/// Taps are ordered from newest to oldest sample contribution, matching the
/// main crate FIR convention:
///
/// `y[k] = h[0] x[k] + h[1] x[k-1] + ...`
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Fir<T, const TAPS: usize, const LANES: usize> {
    taps: [T; TAPS],
    sample_time: T,
}

/// Caller-owned FIR sample history for one filter bank.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FirState<T, const TAPS: usize, const LANES: usize> {
    /// Per-lane sample history stored from newest to oldest.
    pub sample_history: [[T; TAPS]; LANES],
}

impl<T, const TAPS: usize, const LANES: usize> FirState<T, TAPS, LANES>
where
    T: Float,
{
    /// Returns the zero-initialized FIR history.
    #[must_use]
    pub fn zeros() -> Self {
        Self {
            sample_history: [[T::zero(); TAPS]; LANES],
        }
    }

    /// Returns FIR history initialized to a constant lane value.
    #[must_use]
    pub fn filled(value: [T; LANES]) -> Self {
        let mut state = Self::zeros();
        for (lane, &lane_value) in value.iter().enumerate() {
            state.fill_lane(lane, lane_value);
        }
        state
    }

    /// Resets the FIR history to zero.
    pub fn reset(&mut self) {
        *self = Self::zeros();
    }

    /// Fills every stored sample in one lane with a constant value.
    pub fn fill_lane(&mut self, lane: usize, value: T) {
        for sample in &mut self.sample_history[lane] {
            *sample = value;
        }
    }
}

impl<T, const TAPS: usize, const LANES: usize> Default for FirState<T, TAPS, LANES>
where
    T: Float,
{
    fn default() -> Self {
        Self::zeros()
    }
}

impl<T, const TAPS: usize, const LANES: usize> Fir<T, TAPS, LANES>
where
    T: Float,
{
    /// Creates a fixed-size FIR bank.
    pub fn new(taps: [T; TAPS], sample_time: T) -> Result<Self, EmbeddedError> {
        if TAPS == 0 {
            return Err(EmbeddedError::InvalidParameter { which: "fir.taps" });
        }
        if !sample_time.is_finite() || sample_time <= T::zero() {
            return Err(EmbeddedError::InvalidSampleTime);
        }
        for &tap in &taps {
            ensure_finite(tap, "fir.taps")?;
        }
        Ok(Self { taps, sample_time })
    }

    /// Returns the stored taps from newest to oldest contribution.
    #[must_use]
    pub fn taps(&self) -> &[T; TAPS] {
        &self.taps
    }

    /// Returns the stored sample interval.
    #[must_use]
    pub fn sample_time(&self) -> T {
        self.sample_time
    }

    /// Returns a zero state sized for this filter bank.
    #[must_use]
    pub fn reset_state(&self) -> FirState<T, TAPS, LANES> {
        FirState::zeros()
    }

    /// Evaluates one multichannel timestep.
    pub fn step(&self, state: &mut FirState<T, TAPS, LANES>, input: [T; LANES]) -> [T; LANES] {
        let mut output = [T::zero(); LANES];

        for lane in 0..LANES {
            shift_history(&mut state.sample_history[lane], input[lane]);
            output[lane] = dot_taps(&self.taps, &state.sample_history[lane]);
        }

        output
    }

    /// Filters one caller-owned multichannel block into the destination slice.
    pub fn filter_into(
        &self,
        state: &mut FirState<T, TAPS, LANES>,
        input: &[[T; LANES]],
        output: &mut [[T; LANES]],
    ) -> Result<(), EmbeddedError> {
        if input.len() != output.len() {
            return Err(EmbeddedError::LengthMismatch {
                which: "fir.filter_into",
                expected: input.len(),
                actual: output.len(),
            });
        }

        for idx in 0..input.len() {
            output[idx] = self.step(state, input[idx]);
        }
        Ok(())
    }

    /// Returns the scalar DC gain equal to the tap sum.
    pub fn dc_gain(&self) -> Result<T, EmbeddedError> {
        let mut gain = T::zero();
        for &tap in &self.taps {
            gain = gain + tap;
        }
        ensure_finite(gain, "fir.dc_gain")
    }

    /// Casts taps and sample time to another scalar dtype.
    pub fn try_cast<S>(&self) -> Result<Fir<S, TAPS, LANES>, EmbeddedError>
    where
        S: Float,
    {
        let mut taps = [S::zero(); TAPS];
        for (idx, &tap) in self.taps.iter().enumerate() {
            taps[idx] =
                NumCast::from(tap).ok_or(EmbeddedError::InvalidParameter { which: "fir.taps" })?;
        }

        Fir::new(
            taps,
            NumCast::from(self.sample_time).ok_or(EmbeddedError::InvalidSampleTime)?,
        )
    }
}

#[cfg(feature = "alloc")]
impl<T, const TAPS: usize, const LANES: usize> TryFrom<&crate::control::lti::Fir<T>>
    for Fir<T, TAPS, LANES>
where
    T: Float + faer_traits::RealField + crate::sparse::CompensatedField,
{
    type Error = EmbeddedError;

    /// Converts the dynamic control-side FIR into a fixed-size embedded FIR.
    fn try_from(value: &crate::control::lti::Fir<T>) -> Result<Self, Self::Error> {
        if value.len() != TAPS {
            return Err(EmbeddedError::LengthMismatch {
                which: "embedded.fixed.fir.ntaps",
                expected: TAPS,
                actual: value.len(),
            });
        }

        let mut taps = [T::zero(); TAPS];
        for (idx, &tap) in value.taps().iter().enumerate() {
            taps[idx] = tap;
        }

        Self::new(taps, value.sample_time())
    }
}

/// Pushes one new sample into the newest-first history buffer.
fn shift_history<T, const TAPS: usize>(history: &mut [T; TAPS], input: T)
where
    T: Float,
{
    for idx in (1..TAPS).rev() {
        history[idx] = history[idx - 1];
    }
    history[0] = input;
}

/// Computes one FIR dot product against newest-first sample history.
fn dot_taps<T, const TAPS: usize>(taps: &[T; TAPS], history: &[T; TAPS]) -> T
where
    T: Float,
{
    let mut acc = T::zero();
    for idx in 0..TAPS {
        acc = acc + taps[idx] * history[idx];
    }
    acc
}

#[cfg(test)]
mod tests {
    use super::{Fir, FirState};

    fn assert_close(lhs: f32, rhs: f32, tol: f32) {
        let err = (lhs - rhs).abs();
        assert!(err <= tol, "lhs={lhs}, rhs={rhs}, err={err}, tol={tol}");
    }

    #[test]
    fn fixed_fir_runs_multilane_block() {
        let filter = Fir::<f32, 2, 2>::new([0.5, 0.5], 1.0).unwrap();
        let mut state = FirState::<f32, 2, 2>::zeros();
        let input = [[1.0, 10.0], [3.0, 20.0], [5.0, 30.0]];
        let mut output = [[0.0; 2]; 3];

        filter.filter_into(&mut state, &input, &mut output).unwrap();

        assert_close(output[0][0], 0.5, 1.0e-6);
        assert_close(output[0][1], 5.0, 1.0e-6);
        assert_close(output[1][0], 2.0, 1.0e-6);
        assert_close(output[1][1], 15.0, 1.0e-6);
        assert_close(output[2][0], 4.0, 1.0e-6);
        assert_close(output[2][1], 25.0, 1.0e-6);
    }

    #[cfg(feature = "alloc")]
    #[test]
    fn converts_dynamic_fir_to_fixed() {
        let dynamic = crate::control::lti::Fir::new(vec![0.25f32, 0.5, 0.25], 0.1).unwrap();
        let fixed = Fir::<f32, 3, 1>::try_from(&dynamic).unwrap();

        assert_eq!(fixed.taps(), &[0.25, 0.5, 0.25]);
        assert_close(fixed.sample_time(), 0.1, 1.0e-6);
    }
}
