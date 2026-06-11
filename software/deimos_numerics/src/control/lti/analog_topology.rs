//! Analog circuit topology transfer-function helpers.

use faer_traits::RealField;
use num_traits::Float;

use super::{ContinuousTransferFunction, LtiError};

/// Builds the continuous-time transfer function for a unity-gain Sallen-Key
/// low-pass filter.
///
/// The returned transfer function is:
///
/// `H(s) = 1 / (R1 R2 C1 C2 s^2 + C2 (R1 + R2) s + 1)`
///
/// The undamped natural frequency, commonly used as the cutoff target for this
/// topology, is:
///
/// `f0 = 1 / (2 pi sqrt(R1 R2 C1 C2))`
///
/// Coefficients are stored in the crate's usual descending-power order.
///
/// Formula reference:
/// <https://en.wikipedia.org/wiki/Sallen%E2%80%93Key_topology#Application:_low-pass_filter>
pub fn sallen_key_lowpass_transfer_function<R>(
    r1: R,
    r2: R,
    c1: R,
    c2: R,
) -> Result<ContinuousTransferFunction<R>, LtiError>
where
    R: Float + RealField,
{
    validate_positive_component(r1, "sallen_key.r1")?;
    validate_positive_component(r2, "sallen_key.r2")?;
    validate_positive_component(c1, "sallen_key.c1")?;
    validate_positive_component(c2, "sallen_key.c2")?;

    ContinuousTransferFunction::continuous(
        [R::one()],
        [c1 * c2 * r1 * r2, c2 * (r1 + r2), R::one()],
    )
}

fn validate_positive_component<R>(value: R, which: &'static str) -> Result<(), LtiError>
where
    R: Float + RealField,
{
    if value.is_finite() && value > R::zero() {
        Ok(())
    } else {
        Err(LtiError::InvalidComponentValue { which })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sallen_key_lowpass_matches_closed_form_coefficients() {
        let tf = sallen_key_lowpass_transfer_function(10.0e3, 10.0e3, 10.0e-9, 10.0e-9).unwrap();

        assert!((tf.numerator()[0] - 1.0e8).abs() < 1.0e-6);
        assert!((tf.denominator()[0] - 1.0).abs() < 1.0e-12);
        assert!((tf.denominator()[1] - 2.0e4).abs() < 1.0e-9);
        assert!((tf.denominator()[2] - 1.0e8).abs() < 1.0e-6);
        let dc_gain = tf.dc_gain().unwrap();
        assert!((dc_gain.re - 1.0).abs() < 1.0e-12);
        assert!(dc_gain.im.abs() < 1.0e-12);
    }

    #[test]
    fn sallen_key_lowpass_rejects_nonpositive_components() {
        let err = sallen_key_lowpass_transfer_function(0.0, 10.0e3, 10.0e-9, 10.0e-9).unwrap_err();
        assert!(matches!(
            err,
            LtiError::InvalidComponentValue {
                which: "sallen_key.r1"
            }
        ));
    }
}
