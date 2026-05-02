use core::borrow::Borrow;
use num_traits::Float;

/// Computes an error-free addition `a + b`.
///
/// Returns `(sum, err)` such that `sum` is the rounded floating-point sum and
/// `err` is the rounding error.
///
/// Args:
///   a: First scalar addend.
///   b: Second scalar addend.
///
/// Returns:
///   A pair `(sum, err)` where `sum + err` equals the exact real-number sum.
#[inline]
pub fn twosum<T: Float>(a: T, b: T) -> (T, T) {
    let sum = a + b;
    let b_rounded = sum - a;
    let a_rounded = sum - b_rounded;
    let a_roundoff = a - a_rounded;
    let b_roundoff = b - b_rounded;
    let err = a_roundoff + b_roundoff;

    (sum, err)
}

/// Sums an iterator of values with the `TwoSum` accumulator.
///
/// Args:
///   values: Scalar values to accumulate.
///
/// Returns:
///   A pair `(sum, err)` where `sum` is the rounded accumulated value and
///   `err` is the residual rounding term.
#[inline]
#[must_use]
pub fn sum<I, T>(values: I) -> (T, T)
where
    I: IntoIterator,
    I::Item: Borrow<T>,
    T: Float,
{
    let mut values = values.into_iter();
    let Some(value) = values.next() else {
        return (T::zero(), T::zero());
    };

    let mut acc = TwoSum::<T>::new(*value.borrow());
    for value in values {
        acc.add(*value.borrow());
    }

    acc.finish()
}

/// Accumulates values with scalar `twosum` across a fixed number of lanes.
///
/// Incoming values are staged until all lanes are full. At that point, each
/// lane performs its own scalar `twosum` update, which gives the optimizer a
/// fixed-width loop it can vectorize with SLP.
///
/// `NLANES` is the number of staged scalar lanes, not a matrix or vector
/// dimension.
#[derive(Clone, Copy, Debug)]
pub struct TwoSum<T: Float, const NLANES: usize = 8> {
    values: [T; NLANES],
    residuals: [T; NLANES],
    staged: [T; NLANES],
    staged_len: usize,
}

impl<T: Float, const NLANES: usize> TwoSum<T, NLANES> {
    /// Creates a new accumulator with an initial value and zero residual.
    ///
    /// Args:
    ///   value: Initial scalar value.
    ///
    /// Returns:
    ///   A new accumulator seeded with `value`.
    #[inline]
    #[must_use]
    pub fn new(value: T) -> Self {
        Self::with_residual(value, T::zero())
    }

    /// Creates a new accumulator with an initial value and residual.
    ///
    /// Args:
    ///   value: Initial scalar value.
    ///   residual: Initial residual term paired with `value`.
    ///
    /// Returns:
    ///   A new accumulator seeded with the supplied value and residual.
    #[inline]
    #[must_use]
    pub fn with_residual(value: T, residual: T) -> Self {
        assert!(NLANES > 0, "TwoSum requires at least one lane");

        let mut values = [T::zero(); NLANES];
        let mut residuals = [T::zero(); NLANES];

        values[0] = value;
        residuals[0] = residual;

        Self {
            values,
            residuals,
            staged: [T::zero(); NLANES],
            staged_len: 0,
        }
    }

    /// Adds a value into the staged lanes.
    ///
    /// Args:
    ///   value: Scalar value to accumulate.
    #[inline]
    pub fn add(&mut self, value: T) {
        self.staged[self.staged_len] = value;
        self.staged_len += 1;

        if self.staged_len == NLANES {
            self.flush_staged();
        }
    }

    /// Finishes the accumulation and returns `(value, residual)`.
    ///
    /// Returns:
    ///   The final rounded accumulated value and its residual rounding term.
    #[inline]
    #[must_use]
    pub fn finish(mut self) -> (T, T) {
        self.flush_tail();

        for lane in 1..NLANES {
            let value = self.values[lane];
            let residual = self.residuals[lane];

            self.consume_lane(0, value);
            self.consume_lane(0, residual);
        }

        (self.values[0], self.residuals[0])
    }

    #[inline]
    fn flush_staged(&mut self) {
        debug_assert_eq!(self.staged_len, NLANES);

        for lane in 0..NLANES {
            self.consume_lane(lane, self.staged[lane]);
        }

        self.staged_len = 0;
    }

    #[inline]
    fn flush_tail(&mut self) {
        for lane in 0..self.staged_len {
            self.consume_lane(lane, self.staged[lane]);
        }

        self.staged_len = 0;
    }

    #[inline]
    fn consume_lane(&mut self, lane: usize, value: T) {
        let (sum, err) = twosum(self.values[lane], value);
        self.values[lane] = sum;
        self.residuals[lane] = self.residuals[lane] + err;
    }
}

#[cfg(test)]
mod test {
    use super::{TwoSum, sum, twosum};
    use num_traits::Float;

    fn assert_twosum_residual<T: Float + core::fmt::Debug>() {
        let one = T::one();
        let quarter = T::from(0.25).unwrap();
        let tiny = T::epsilon() * quarter;

        let (sum, err) = twosum(tiny, one);

        assert_eq!(sum, one);
        assert_eq!(err, tiny);
    }

    #[test]
    fn twosum_returns_zero_error_for_exact_sum_f32() {
        let (sum, err) = twosum(1.0f32, 0.5f32);

        assert_eq!(sum, 1.5);
        assert_eq!(err, 0.0);
    }

    #[test]
    fn twosum_returns_zero_error_for_exact_sum_f64() {
        let (sum, err) = twosum(1.0f64, 0.5f64);

        assert_eq!(sum, 1.5);
        assert_eq!(err, 0.0);
    }

    #[test]
    fn twosum_captures_rounded_off_bits_without_ordering_f32() {
        assert_twosum_residual::<f32>();
    }

    #[test]
    fn twosum_captures_rounded_off_bits_without_ordering_f64() {
        assert_twosum_residual::<f64>();
    }

    #[test]
    fn accumulator_preserves_initial_residual() {
        let residual = f64::EPSILON * 0.25;
        let (sum, err) = TwoSum::<f64, 4>::with_residual(1.0, residual).finish();

        assert_eq!(sum, 1.0);
        assert_eq!(err, residual);
    }

    #[test]
    fn top_level_sum_accepts_owned_items() {
        let tiny = f64::EPSILON * 0.25;
        let (sum, err) = sum([1.0f64, tiny]);

        assert_eq!(sum, 1.0);
        assert_eq!(err, tiny);
    }

    #[test]
    fn top_level_sum_accepts_borrowed_items() {
        let values = [1.0f64, 2.0, 3.0];
        let (sum, err) = sum::<_, f64>(values.iter());

        assert_eq!(sum, 6.0);
        assert_eq!(err, 0.0);
    }

    #[test]
    fn top_level_sum_handles_empty_input() {
        let (sum, err) = sum(core::iter::empty::<f64>());

        assert_eq!(sum, 0.0);
        assert_eq!(err, 0.0);
    }

    #[test]
    fn accumulator_flushes_partial_tail_on_finish() {
        let tiny = f64::EPSILON * 0.25;
        let mut acc = TwoSum::<f64, 4>::new(1.0);

        acc.add(tiny);
        acc.add(tiny);

        let (sum, err) = acc.finish();

        assert_eq!(sum, 1.0);
        assert_eq!(err, tiny + tiny);
    }

    #[test]
    fn accumulator_reduces_full_and_partial_lanes_into_lane_zero() {
        let tiny = f64::EPSILON * 0.25;
        let mut acc = TwoSum::<f64, 4>::with_residual(1.0, tiny);

        for _ in 0..5 {
            acc.add(tiny);
        }

        let (sum, err) = acc.finish();

        assert_eq!(sum, 1.0);
        assert_eq!(err, tiny * 6.0);
    }

    #[test]
    fn single_lane_accumulator_matches_scalar_updates() {
        let tiny = f32::EPSILON * 0.25;
        let mut acc = TwoSum::<f32, 1>::new(1.0);

        acc.add(tiny);
        acc.add(tiny);

        let (sum, err) = acc.finish();

        assert_eq!(sum, 1.0);
        assert_eq!(err, tiny + tiny);
    }
}
