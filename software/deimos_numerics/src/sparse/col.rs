use faer::{Col, Unbind};
use faer_traits::ComplexField;
use faer_traits::math_utils::zero;

#[inline]
pub(crate) fn col_from_slice<T: Copy>(values: &[T]) -> Col<T> {
    Col::from_fn(values.len(), |i| values[i.unbound()])
}

#[inline]
pub(crate) fn zero_col<T: ComplexField>(len: usize) -> Col<T> {
    Col::from_fn(len, |_| zero::<T>())
}

#[inline]
pub(crate) fn col_slice<T>(col: &Col<T>) -> &[T] {
    col.try_as_col_major().unwrap().as_slice()
}

#[inline]
pub(crate) fn col_slice_mut<T>(col: &mut Col<T>) -> &mut [T] {
    col.try_as_col_major_mut().unwrap().as_slice_mut()
}

#[inline]
pub(crate) fn copy_col<T: Copy>(dst: &mut Col<T>, src: &Col<T>) {
    debug_assert_eq!(dst.nrows(), src.nrows());
    let src = col_slice(src);
    let dst = col_slice_mut(dst);
    for idx in 0..dst.len() {
        dst[idx] = src[idx];
    }
}
