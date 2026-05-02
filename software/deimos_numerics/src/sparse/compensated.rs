use crate::twosum::TwoSum;
use faer::complex::Complex;
use faer_traits::ComplexField;
use faer_traits::RealField;
use faer_traits::ext::ComplexFieldExt;
use num_traits::Float;

#[derive(Clone, Copy, Debug)]
struct RealCompensatedSum<R: Float> {
    acc: Option<TwoSum<R>>,
}

impl<R: Float> Default for RealCompensatedSum<R> {
    #[inline]
    fn default() -> Self {
        Self { acc: None }
    }
}

impl<R: Float> RealCompensatedSum<R> {
    #[inline]
    fn add(&mut self, value: R) {
        match self.acc.as_mut() {
            Some(acc) => acc.add(value),
            None => self.acc = Some(TwoSum::new(value)),
        }
    }

    #[inline]
    fn finish(self) -> R {
        match self.acc {
            Some(acc) => {
                let (sum, residual) = acc.finish();
                sum + residual
            }
            None => R::zero(),
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct CompensatedSum<T: CompensatedField>
where
    T::Real: Float,
{
    real: RealCompensatedSum<T::Real>,
    imag: RealCompensatedSum<T::Real>,
}

/// Scalars that support reconstruction from compensated real and imaginary parts.
///
/// `faer_traits::ComplexField` does not expose a generic `from_parts` constructor,
/// so compensated kernels need this small extension trait in order to rebuild the
/// final scalar after separately accumulating the real and imaginary components.
pub trait CompensatedField: ComplexField + Copy
where
    Self::Real: Float,
{
    /// Rebuilds a scalar from separately accumulated real and imaginary parts.
    fn from_real_imag(real: Self::Real, imag: Self::Real) -> Self;
}

impl CompensatedField for f32 {
    #[inline]
    fn from_real_imag(real: Self::Real, _imag: Self::Real) -> Self {
        real
    }
}

impl CompensatedField for f64 {
    #[inline]
    fn from_real_imag(real: Self::Real, _imag: Self::Real) -> Self {
        real
    }
}

impl<R> CompensatedField for Complex<R>
where
    R: Float + RealField,
{
    #[inline]
    fn from_real_imag(real: Self::Real, imag: Self::Real) -> Self {
        Self::new(real, imag)
    }
}

impl<T: CompensatedField> Default for CompensatedSum<T>
where
    T::Real: Float,
{
    #[inline]
    fn default() -> Self {
        Self {
            real: RealCompensatedSum::default(),
            imag: RealCompensatedSum::default(),
        }
    }
}

impl<T: CompensatedField> CompensatedSum<T>
where
    T::Real: Float,
{
    #[inline]
    pub(crate) fn add(&mut self, value: T) {
        self.real.add(value.real());
        self.imag.add(value.imag());
    }

    #[inline]
    pub(crate) fn finish(self) -> T {
        T::from_real_imag(self.real.finish(), self.imag.finish())
    }
}

#[inline]
pub(crate) fn dotc<T: CompensatedField>(lhs: &[T], rhs: &[T]) -> T
where
    T::Real: Float,
{
    assert_eq!(lhs.len(), rhs.len());

    let mut acc = CompensatedSum::<T>::default();
    for (&lhs, &rhs) in lhs.iter().zip(rhs.iter()) {
        acc.add(lhs.conj() * rhs);
    }

    acc.finish()
}

#[inline]
pub(crate) fn norm2_sq<T: CompensatedField>(values: &[T]) -> T::Real
where
    T::Real: Float,
{
    let mut acc = RealCompensatedSum::<T::Real>::default();
    for &value in values {
        acc.add(value.abs2());
    }

    acc.finish()
}

#[inline]
pub(crate) fn norm2<T: CompensatedField>(values: &[T]) -> T::Real
where
    T::Real: Float,
{
    norm2_sq::<T>(values).sqrt()
}

#[cfg(test)]
mod test {
    use super::dotc;
    use faer::c64;

    #[test]
    fn dotc_handles_real_cancellation() {
        let lhs = [1.0e16f64, 1.0, -1.0e16];
        let rhs = [1.0f64, 1.0, 1.0];

        assert_eq!(dotc(&lhs, &rhs), 1.0);
    }

    #[test]
    fn dotc_uses_conjugation_for_complex_inputs() {
        let lhs = [c64::new(1.0, 2.0), c64::new(-3.0, 4.0)];
        let rhs = [c64::new(5.0, -1.0), c64::new(2.0, 3.0)];
        let dot = dotc(&lhs, &rhs);

        let expected = lhs[0].conj() * rhs[0] + lhs[1].conj() * rhs[1];
        assert_eq!(dot, expected);
    }
}
