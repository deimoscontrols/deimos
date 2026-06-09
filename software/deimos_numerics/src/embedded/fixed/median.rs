//! Fixed-size median filters and small-sort helpers.
//!
//! These utilities are allocation-free and intended for very small windows
//! common in embedded signal conditioning.

/// Sorts a slice in place with insertion sort.
fn insertion_sort<T: PartialOrd, const N: usize>(values: &mut [T; N]) {
    for i in 1..N {
        for j in (1..=i).rev() {
            if values[j - 1] > values[j] {
                values.swap(j - 1, j);
            }
        }
    }
}

/// Fixed-size median filter for partially ordered scalar values.
///
/// `N` must be odd and at least 3 so the median is unique. The implementation
/// keeps the most recent `N` samples in a fixed array, copies them into a
/// scratch buffer on each update, sorts that scratch buffer with insertion
/// sort, and returns the middle element.
///
/// The filter accepts `PartialOrd` so it can be used with floating-point
/// samples, but it does not handle unordered values specially. Supplying `NaN`
/// can silently produce an invalid median.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MedianFilter<T, const N: usize> {
    values: [T; N],
    scratch: [T; N],
    mid: usize,
}

impl<T, const N: usize> MedianFilter<T, N>
where
    T: PartialOrd + Copy,
{
    /// Initializes the filter with all samples set to `value`.
    #[must_use]
    pub fn new(value: T) -> Self {
        const {
            assert!(N >= 3);
            assert!(N % 2 == 1);
        }

        Self {
            values: [value; N],
            scratch: [value; N],
            mid: N / 2,
        }
    }

    /// Pushes a new value and returns the updated median.
    pub fn update(&mut self, value: T) -> T {
        for idx in 0..(N - 1) {
            self.values[idx] = self.values[idx + 1];
        }
        self.values[N - 1] = value;

        self.scratch.copy_from_slice(&self.values);
        insertion_sort(&mut self.scratch);
        self.scratch[self.mid]
    }

    /// Returns the current window ordered from oldest to newest.
    #[must_use]
    pub fn values(&self) -> &[T; N] {
        &self.values
    }
}

#[cfg(test)]
mod tests {
    use super::{MedianFilter, insertion_sort};

    #[test]
    fn insertion_sort_orders_small_slice() {
        let mut values = [4, 2, 5, 1, 3];
        insertion_sort(&mut values);
        assert_eq!(values, [1, 2, 3, 4, 5]);
    }

    #[test]
    fn median_filter_matches_reference_sequences() {
        let input = [2_u32, 5, 6, 7, 8, 4, 9, 0, 1, 3];

        let expected = [0, 2, 5, 6, 7, 7, 8, 4, 1, 1];
        let mut filter = MedianFilter::<u32, 3>::new(0);
        for (idx, (&input, &expected)) in input.iter().zip(expected.iter()).enumerate() {
            let actual = filter.update(input);
            assert_eq!(actual, expected, "{idx}: {actual} != {expected}");
        }

        let expected = [0, 0, 2, 5, 6, 6, 7, 7, 4, 3];
        let mut filter = MedianFilter::<u32, 5>::new(0);
        for (idx, (&input, &expected)) in input.iter().zip(expected.iter()).enumerate() {
            let actual = filter.update(input);
            assert_eq!(actual, expected, "{idx}: {actual} != {expected}");
        }
    }
}
