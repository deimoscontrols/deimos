//! Small scalar helpers shared across the embedded runtime modules.
//!
use super::EmbeddedError;
use num_traits::Float;

/// Returns `value` if it is finite, otherwise reports a named runtime error.
pub(crate) fn ensure_finite<T>(value: T, which: &'static str) -> Result<T, EmbeddedError>
where
    T: Float,
{
    if value.is_finite() {
        Ok(value)
    } else {
        Err(EmbeddedError::NonFiniteValue { which })
    }
}
