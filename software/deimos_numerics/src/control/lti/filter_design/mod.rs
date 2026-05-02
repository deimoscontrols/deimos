//! Analog and digital IIR filter design.
//!
//! The implementation is intentionally practical and numerically
//! conservative:
//!
//! - prototype generation and frequency transformations stay in `Zpk` form
//! - `Sos` is the preferred practical realized output form
//! - raw polynomial `TransferFunction` output is treated as an interoperability
//!   view rather than the main working representation
//! - `DeltaSos` is intentionally not a design target; it is a derived runtime
//!   form obtained later from a designed `DiscreteSos` when low-cutoff
//!   execution conditioning matters
//!
//! Supported here:
//!
//! - Butterworth
//! - Chebyshev Type I
//! - Bessel, analog only
//! - lowpass / highpass / bandpass / bandstop
//! - minimum-order Butterworth and Chebyshev-I selection helpers
//!
//! Digital Bessel design is intentionally not exposed in the digital family
//! enum. A bilinear-transformed Bessel filter is mathematically possible, but
//! the API omits it so users are not misled into expecting classical
//! Bessel group-delay behavior from the resulting digital filter.
//!
//! # Two Intuitions
//!
//! 1. **Prototype-transformation view.** Design starts from a normalized
//!    lowpass prototype, then reshapes it into lowpass, highpass, bandpass, or
//!    bandstop form and optionally maps it into the digital domain.
//! 2. **Numerical-form view.** The same workflow is also about choosing the
//!    right representation while the design evolves: roots first, sections
//!    second, coefficients last.
//!
//! # Glossary
//!
//! - **Prototype:** Normalized analog lowpass filter used as a starting point.
//! - **Prewarping:** Bilinear-transform frequency correction at one chosen
//!   frequency.
//! - **SOS:** Second-order sections, the preferred realized IIR design and
//!   storage form.
//! - **Delta-SOS:** A derived discrete execution form of SOS, useful when very
//!   low normalized cutoffs make ordinary section recurrences ill-conditioned.
//! - **Order selection:** Computing the smallest filter order that meets a
//!   ripple/attenuation specification.
//!
//! # Mathematical Formulation
//!
//! The design flow is:
//!
//! 1. construct an analog lowpass prototype in `Zpk` form
//! 2. apply an analog frequency transformation for the requested shape
//! 3. for digital filters, apply the bilinear map `s = c * (1 - z^-1) / (1 + z^-1)`
//! 4. convert to `Sos` or polynomial form only at the edge of the API
//!
//! # Implementation Notes
//!
//! - Root-based construction avoids unnecessary high-order polynomial
//!   arithmetic during the numerically sensitive stages.
//! - Digital Bessel is intentionally omitted from the type surface.
//! - Order-selection helpers are spec-driven and produce the minimum order
//!   consistent with the formulas in this module.
//!
//! # Feature Matrix
//!
//! | Family / shape | Analog | Digital | Lowpass | Highpass | Bandpass | Bandstop |
//! | --- | --- | --- | --- | --- | --- | --- |
//! | Butterworth | yes | yes | yes | yes | yes | yes |
//! | Chebyshev I | yes | yes | yes | yes | yes | yes |
//! | Bessel | yes | no | yes | yes | yes | yes |
//! | Minimum-order selection | yes | yes | yes | yes | yes | yes |

mod digital;
mod error;
mod order;
mod prototype;
mod spec;
mod transform;

pub use error::FilterDesignError;
pub use order::{
    ButterworthOrderResult, Chebyshev1OrderResult, buttord_analog, buttord_digital,
    cheb1ord_analog, cheb1ord_digital,
};
pub use spec::{
    AnalogFilterFamily, AnalogFilterSpec, AnalogOrderSelectionSpec, DigitalFilterFamily,
    DigitalFilterSpec, DigitalOrderSelectionSpec, FilterShape,
};

use digital::maybe_prewarp_shape;
use prototype::analog_lowpass_prototype_zpk;
use transform::{analog_shape_transform, bilinear_transform_zpk};

use super::{
    ContinuousSos, ContinuousTransferFunction, ContinuousZpk, DiscreteSos,
    DiscreteTransferFunction, DiscreteZpk,
};
use faer_traits::RealField;
use num_traits::Float;

/// Designs an analog IIR filter and returns it in `Zpk` form.
///
/// This is the numerically preferred internal representation because prototype
/// generation and frequency transformations operate directly on roots instead
/// of high-order polynomials.
pub fn design_analog_filter_zpk<R>(
    spec: &AnalogFilterSpec<R>,
) -> Result<ContinuousZpk<R>, FilterDesignError>
where
    R: Float + RealField,
{
    let prototype = analog_lowpass_prototype_zpk(spec.order, spec.family)?;
    analog_shape_transform(&prototype, spec.shape)
}

/// Designs an analog IIR filter and returns it in `Sos` form.
///
/// `Sos` is usually the best practical representation for realized high-order
/// filters because it localizes rounding error section by section. It is also
/// the canonical sectioned storage form in this crate; delta-SOS is only a
/// derived discrete runtime form.
pub fn design_analog_filter_sos<R>(
    spec: &AnalogFilterSpec<R>,
) -> Result<ContinuousSos<R>, FilterDesignError>
where
    R: Float + RealField,
{
    Ok(design_analog_filter_zpk(spec)?.to_sos()?)
}

/// Designs an analog IIR filter and returns it in coefficient form.
///
/// This is mainly an interoperability view. High-order direct-form
/// coefficients are more numerically fragile than `Zpk` or `Sos`.
pub fn design_analog_filter_tf<R>(
    spec: &AnalogFilterSpec<R>,
) -> Result<ContinuousTransferFunction<R>, FilterDesignError>
where
    R: Float + RealField,
{
    Ok(design_analog_filter_zpk(spec)?.to_transfer_function()?)
}

/// Designs a digital IIR filter and returns it in `Zpk` form.
///
/// The implementation builds an analog prototype, applies the requested analog
/// frequency transformation, optionally prewarps the cutoff frequencies, then
/// maps the result through the bilinear transform.
pub fn design_digital_filter_zpk<R>(
    spec: &DigitalFilterSpec<R>,
) -> Result<DiscreteZpk<R>, FilterDesignError>
where
    R: Float + RealField,
{
    let analog_family = match spec.family {
        DigitalFilterFamily::Butterworth => AnalogFilterFamily::Butterworth,
        DigitalFilterFamily::Chebyshev1 { ripple_db } => {
            AnalogFilterFamily::Chebyshev1 { ripple_db }
        }
    };
    let analog_spec = AnalogFilterSpec::new(
        spec.order,
        analog_family,
        maybe_prewarp_shape(spec.shape, spec.sample_rate, spec.prewarp),
    )?;
    let analog = design_analog_filter_zpk(&analog_spec)?;
    bilinear_transform_zpk(&analog, spec.sample_rate)
}

/// Designs a digital IIR filter and returns it in `Sos` form.
///
/// This is the canonical realized digital IIR representation in this crate.
/// It is the preferred storage and interchange form for high-order filters.
/// Callers that need a different view can convert from this form after design,
/// including deriving `DeltaSos` later when low-cutoff runtime conditioning is
/// the main concern.
pub fn design_digital_filter_sos<R>(
    spec: &DigitalFilterSpec<R>,
) -> Result<DiscreteSos<R>, FilterDesignError>
where
    R: Float + RealField,
{
    Ok(design_digital_filter_zpk(spec)?.to_sos()?)
}

/// Designs a digital IIR filter and returns it in coefficient form.
///
/// This is kept for interoperability and inspection. For numerically sensitive
/// high-order filters, prefer `design_digital_filter_sos`.
pub fn design_digital_filter_tf<R>(
    spec: &DigitalFilterSpec<R>,
) -> Result<DiscreteTransferFunction<R>, FilterDesignError>
where
    R: Float + RealField,
{
    Ok(design_digital_filter_zpk(spec)?.to_transfer_function()?)
}

#[cfg(test)]
mod tests {
    use super::{
        AnalogFilterFamily, AnalogFilterSpec, DigitalFilterFamily, DigitalFilterSpec, FilterShape,
        design_analog_filter_sos, design_analog_filter_tf, design_analog_filter_zpk,
        design_digital_filter_sos, design_digital_filter_tf, design_digital_filter_zpk,
    };
    use alloc::vec::Vec;
    use faer::complex::Complex;
    use nalgebra::Normed;

    fn assert_close(lhs: f64, rhs: f64, tol: f64) {
        let err = (lhs - rhs).abs();
        assert!(err <= tol, "lhs={lhs}, rhs={rhs}, err={err}, tol={tol}");
    }

    fn assert_monotone_nonincreasing(values: &[f64], tol: f64) {
        for pair in values.windows(2) {
            assert!(
                pair[1] <= pair[0] + tol,
                "sequence is not monotone nonincreasing: {} then {}",
                pair[0],
                pair[1]
            );
        }
    }

    #[test]
    fn analog_butterworth_lowpass_has_unity_dc_gain_and_lhp_poles() {
        let spec = AnalogFilterSpec::new(
            3,
            AnalogFilterFamily::Butterworth,
            FilterShape::Lowpass { cutoff: 2.0 },
        )
        .unwrap();
        let zpk = design_analog_filter_zpk(&spec).unwrap();
        assert_close(zpk.evaluate(Complex::new(0.0, 0.0)).norm(), 1.0, 1.0e-10);
        assert!(zpk.poles().iter().all(|pole| pole.re < 0.0));
    }

    #[test]
    fn analog_chebyshev_bandstop_rejects_center_frequency() {
        let spec = AnalogFilterSpec::new(
            2,
            AnalogFilterFamily::Chebyshev1 { ripple_db: 1.0 },
            FilterShape::Bandstop {
                low_cutoff: 8.0,
                high_cutoff: 12.0,
            },
        )
        .unwrap();
        let tf = design_analog_filter_tf(&spec).unwrap();
        let dc = tf.evaluate(Complex::new(0.0, 0.0)).norm();
        let center = tf
            .evaluate(Complex::new(0.0, (8.0f64 * 12.0f64).sqrt()))
            .norm();
        assert!(center < dc);
    }

    #[test]
    fn analog_even_order_chebyshev_lowpass_has_unity_dc_gain() {
        let spec = AnalogFilterSpec::new(
            4,
            AnalogFilterFamily::Chebyshev1 { ripple_db: 1.0 },
            FilterShape::Lowpass { cutoff: 2.0 },
        )
        .unwrap();
        let zpk = design_analog_filter_zpk(&spec).unwrap();
        assert_close(zpk.evaluate(Complex::new(0.0, 0.0)).norm(), 1.0, 1.0e-10);
    }

    #[test]
    fn analog_bessel_designs_and_converts_consistently() {
        let spec = AnalogFilterSpec::new(
            3,
            AnalogFilterFamily::Bessel,
            FilterShape::Highpass { cutoff: 4.0 },
        )
        .unwrap();
        let zpk = design_analog_filter_zpk(&spec).unwrap();
        let sos = design_analog_filter_sos(&spec).unwrap();
        let tf = design_analog_filter_tf(&spec).unwrap();
        let point = Complex::new(0.0, 20.0);
        let lhs = zpk.evaluate(point);
        let rhs = sos.evaluate(point);
        let ref_value = tf.evaluate(point);
        assert!((lhs - rhs).norm() <= 1.0e-8);
        assert!((lhs - ref_value).norm() <= 1.0e-8);
    }

    #[test]
    fn analog_bessel_lowpass_uses_minus_three_db_cutoff_normalization() {
        let spec = AnalogFilterSpec::new(
            4,
            AnalogFilterFamily::Bessel,
            FilterShape::Lowpass { cutoff: 1.0 },
        )
        .unwrap();
        let tf = design_analog_filter_tf(&spec).unwrap();
        let cutoff_gain: f64 = tf.evaluate(Complex::new(0.0, 1.0)).norm();
        let expected = 1.0f64 / 2.0f64.sqrt();
        assert!((cutoff_gain - expected).abs() <= 1.0e-6);
    }

    #[test]
    fn digital_butterworth_lowpass_is_stable_and_lowpass_like() {
        let spec = DigitalFilterSpec::new(
            4,
            DigitalFilterFamily::Butterworth,
            FilterShape::Lowpass { cutoff: 10.0 },
            100.0,
        )
        .unwrap();
        let zpk = design_digital_filter_zpk(&spec).unwrap();
        assert!(zpk.poles().iter().all(|pole| pole.norm() < 1.0));
        let dc = zpk.evaluate(Complex::new(1.0, 0.0)).norm();
        let nyquist = zpk.evaluate(Complex::new(-1.0, 0.0)).norm();
        assert!(dc > nyquist);
    }

    #[test]
    fn digital_bandpass_hits_center_more_than_dc() {
        let spec = DigitalFilterSpec::new(
            2,
            DigitalFilterFamily::Chebyshev1 { ripple_db: 1.0 },
            FilterShape::Bandpass {
                low_cutoff: 15.0,
                high_cutoff: 25.0,
            },
            100.0,
        )
        .unwrap();
        let tf = design_digital_filter_tf(&spec).unwrap();
        let dc = tf.evaluate(Complex::new(1.0, 0.0)).norm();
        let center_omega = (15.0f64 * 25.0f64).sqrt();
        let phase = center_omega / 100.0;
        let center = tf.evaluate(Complex::new(phase.cos(), phase.sin())).norm();
        assert!(center > dc);
    }

    #[test]
    fn digital_butterworth_lowpass_bode_magnitude_and_phase_decrease_monotonically() {
        let spec = DigitalFilterSpec::new(
            4,
            DigitalFilterFamily::Butterworth,
            FilterShape::Lowpass { cutoff: 8.0 },
            20.0,
        )
        .unwrap();
        let filter = design_digital_filter_sos(&spec).unwrap();
        assert_close(filter.sample_time(), 1.0 / spec.sample_rate, 1.0e-12);
        let nyquist = spec.sample_rate * core::f64::consts::PI;
        let frequencies = (0..260)
            .map(|i| {
                let t = (i as f64) / 259.0;
                10.0_f64.powf(-1.0 + t * (0.98 * nyquist).log10())
            })
            .collect::<Vec<_>>();
        let bode = filter.bode_data(&frequencies).unwrap();

        assert_monotone_nonincreasing(&bode.magnitude_db, 1.0e-9);
        assert_monotone_nonincreasing(&bode.phase_deg, 1.0e-9);
    }
}
