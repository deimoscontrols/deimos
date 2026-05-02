//! Practical minimum-order IIR filter-selection helpers.
//!
//! The implementation follows the existing design pipeline:
//!
//! - validate passband / stopband shape pairs
//! - reduce the problem to a normalized lowpass prototype
//! - compute the minimum prototype order
//! - map the resulting critical frequencies back into a design-ready filter
//!   specification for the existing `design_*` entry points
//!
//! # Glossary
//!
//! - **Passband:** Frequency region where the filter should preserve signal.
//! - **Stopband:** Frequency region where the filter should attenuate signal.
//! - **Prototype order:** Order of the normalized lowpass prototype used by the
//!   design.
//! - **Critical frequencies:** Cutoff or band-edge frequencies used to realize
//!   the selected order.

use super::digital::{maybe_prewarp_shape, maybe_unprewarp_shape};
use super::error::FilterDesignError;
use super::spec::{
    AnalogFilterFamily, AnalogFilterSpec, AnalogOrderSelectionSpec, DigitalFilterFamily,
    DigitalFilterSpec, DigitalOrderSelectionSpec, FilterShape,
};
use faer_traits::RealField;
use num_traits::Float;

/// Minimum-order Butterworth design result.
#[derive(Clone, Debug, PartialEq)]
pub struct ButterworthOrderResult<R, Spec> {
    /// Minimum order that satisfies the requested passband / stopband bounds.
    pub order: usize,
    /// Critical frequencies for the selected order.
    pub critical_shape: FilterShape<R>,
    /// Ready-to-design filter specification using the selected order and
    /// critical frequencies.
    pub spec: Spec,
}

/// Minimum-order Chebyshev Type-I design result.
#[derive(Clone, Debug, PartialEq)]
pub struct Chebyshev1OrderResult<R, Spec> {
    /// Minimum order that satisfies the requested passband / stopband bounds.
    pub order: usize,
    /// Critical frequencies for the selected order.
    ///
    /// For Chebyshev Type-I filters these are the passband-edge frequencies.
    pub critical_shape: FilterShape<R>,
    /// Ready-to-design filter specification using the selected order and
    /// critical frequencies.
    pub spec: Spec,
}

/// Selects the minimum-order analog Butterworth filter that satisfies the
/// supplied passband / stopband specification.
pub fn buttord_analog<R>(
    spec: &AnalogOrderSelectionSpec<R>,
) -> Result<ButterworthOrderResult<R, AnalogFilterSpec<R>>, FilterDesignError>
where
    R: Float + RealField,
{
    let (order, critical_shape) = butterworth_order_and_shape(
        spec.passband,
        spec.stopband,
        spec.passband_ripple_db,
        spec.stopband_attenuation_db,
    )?;
    let design_spec =
        AnalogFilterSpec::new(order, AnalogFilterFamily::Butterworth, critical_shape)?;
    Ok(ButterworthOrderResult {
        order,
        critical_shape,
        spec: design_spec,
    })
}

/// Selects the minimum-order digital Butterworth filter that satisfies the
/// supplied passband / stopband specification.
pub fn buttord_digital<R>(
    spec: &DigitalOrderSelectionSpec<R>,
) -> Result<ButterworthOrderResult<R, DigitalFilterSpec<R>>, FilterDesignError>
where
    R: Float + RealField,
{
    let analog_passband = maybe_prewarp_shape(spec.passband, spec.sample_rate, spec.prewarp);
    let analog_stopband = maybe_prewarp_shape(spec.stopband, spec.sample_rate, spec.prewarp);
    let (order, analog_critical_shape) = butterworth_order_and_shape(
        analog_passband,
        analog_stopband,
        spec.passband_ripple_db,
        spec.stopband_attenuation_db,
    )?;
    let critical_shape =
        maybe_unprewarp_shape(analog_critical_shape, spec.sample_rate, spec.prewarp);
    let design_spec = DigitalFilterSpec::new(
        order,
        DigitalFilterFamily::Butterworth,
        critical_shape,
        spec.sample_rate,
    )?
    .with_prewarp(spec.prewarp);
    Ok(ButterworthOrderResult {
        order,
        critical_shape,
        spec: design_spec,
    })
}

/// Selects the minimum-order analog Chebyshev Type-I filter that satisfies the
/// supplied passband / stopband specification.
pub fn cheb1ord_analog<R>(
    spec: &AnalogOrderSelectionSpec<R>,
) -> Result<Chebyshev1OrderResult<R, AnalogFilterSpec<R>>, FilterDesignError>
where
    R: Float + RealField,
{
    let order = chebyshev1_order(
        spec.passband,
        spec.stopband,
        spec.passband_ripple_db,
        spec.stopband_attenuation_db,
    )?;
    let design_spec = AnalogFilterSpec::new(
        order,
        AnalogFilterFamily::Chebyshev1 {
            ripple_db: spec.passband_ripple_db,
        },
        spec.passband,
    )?;
    Ok(Chebyshev1OrderResult {
        order,
        critical_shape: spec.passband,
        spec: design_spec,
    })
}

/// Selects the minimum-order digital Chebyshev Type-I filter that satisfies
/// the supplied passband / stopband specification.
pub fn cheb1ord_digital<R>(
    spec: &DigitalOrderSelectionSpec<R>,
) -> Result<Chebyshev1OrderResult<R, DigitalFilterSpec<R>>, FilterDesignError>
where
    R: Float + RealField,
{
    let analog_passband = maybe_prewarp_shape(spec.passband, spec.sample_rate, spec.prewarp);
    let analog_stopband = maybe_prewarp_shape(spec.stopband, spec.sample_rate, spec.prewarp);
    let order = chebyshev1_order(
        analog_passband,
        analog_stopband,
        spec.passband_ripple_db,
        spec.stopband_attenuation_db,
    )?;
    let design_spec = DigitalFilterSpec::new(
        order,
        DigitalFilterFamily::Chebyshev1 {
            ripple_db: spec.passband_ripple_db,
        },
        spec.passband,
        spec.sample_rate,
    )?
    .with_prewarp(spec.prewarp);
    Ok(Chebyshev1OrderResult {
        order,
        critical_shape: spec.passband,
        spec: design_spec,
    })
}

fn butterworth_order_and_shape<R>(
    passband: FilterShape<R>,
    stopband: FilterShape<R>,
    passband_ripple_db: R,
    stopband_attenuation_db: R,
) -> Result<(usize, FilterShape<R>), FilterDesignError>
where
    R: Float + RealField,
{
    // Order selection is done in normalized prototype space first, then the
    // chosen critical frequencies are mapped back into the caller's requested
    // shape.
    let omega_s = normalized_stopband_ratio(passband, stopband)?;
    let epsilon_p = ripple_epsilon(passband_ripple_db)?;
    let epsilon_s = ripple_epsilon(stopband_attenuation_db)?;
    let order = minimum_butterworth_order(epsilon_p, epsilon_s, omega_s)?;
    let critical_shape = butterworth_critical_shape(passband, order, epsilon_p);
    Ok((order, critical_shape))
}

fn chebyshev1_order<R>(
    passband: FilterShape<R>,
    stopband: FilterShape<R>,
    passband_ripple_db: R,
    stopband_attenuation_db: R,
) -> Result<usize, FilterDesignError>
where
    R: Float + RealField,
{
    let omega_s = normalized_stopband_ratio(passband, stopband)?;
    let epsilon_p = ripple_epsilon(passband_ripple_db)?;
    let epsilon_s = ripple_epsilon(stopband_attenuation_db)?;
    minimum_chebyshev1_order(epsilon_p, epsilon_s, omega_s)
}

fn normalized_stopband_ratio<R>(
    passband: FilterShape<R>,
    stopband: FilterShape<R>,
) -> Result<R, FilterDesignError>
where
    R: Float + RealField,
{
    let ratio = match (passband, stopband) {
        (FilterShape::Lowpass { cutoff: wp }, FilterShape::Lowpass { cutoff: ws }) => {
            if ws <= wp {
                return Err(FilterDesignError::InfeasibleOrderSelectionSpec);
            }
            ws / wp
        }
        (FilterShape::Highpass { cutoff: wp }, FilterShape::Highpass { cutoff: ws }) => {
            if ws >= wp {
                return Err(FilterDesignError::InfeasibleOrderSelectionSpec);
            }
            wp / ws
        }
        (
            FilterShape::Bandpass {
                low_cutoff: wp1,
                high_cutoff: wp2,
            },
            FilterShape::Bandpass {
                low_cutoff: ws1,
                high_cutoff: ws2,
            },
        ) => {
            if !(ws1 < wp1 && wp1 < wp2 && wp2 < ws2) {
                return Err(FilterDesignError::InfeasibleOrderSelectionSpec);
            }
            let bandwidth = wp2 - wp1;
            let omega0 = (wp1 * wp2).sqrt();
            // Reduce the bandpass problem to the lowpass prototype via
            // `Omega = |(w^2 - w0^2) / (B w)|` and keep the more demanding of
            // the two stopband edges.
            let low = ((ws1 * ws1 - omega0 * omega0) / (bandwidth * ws1)).abs();
            let high = ((ws2 * ws2 - omega0 * omega0) / (bandwidth * ws2)).abs();
            low.min(high)
        }
        (
            FilterShape::Bandstop {
                low_cutoff: wp1,
                high_cutoff: wp2,
            },
            FilterShape::Bandstop {
                low_cutoff: ws1,
                high_cutoff: ws2,
            },
        ) => {
            if !(wp1 < ws1 && ws1 < ws2 && ws2 < wp2) {
                return Err(FilterDesignError::InfeasibleOrderSelectionSpec);
            }
            let bandwidth = wp2 - wp1;
            let omega0 = (wp1 * wp2).sqrt();
            // For bandstop, the prototype mapping is inverted relative to the
            // bandpass case.
            let low = (bandwidth * ws1 / (omega0 * omega0 - ws1 * ws1)).abs();
            let high = (bandwidth * ws2 / (ws2 * ws2 - omega0 * omega0)).abs();
            low.min(high)
        }
        _ => return Err(FilterDesignError::IncompatibleOrderSelectionShape),
    };

    if !ratio.is_finite() || ratio <= R::one() {
        return Err(FilterDesignError::InfeasibleOrderSelectionSpec);
    }
    Ok(ratio)
}

fn ripple_epsilon<R>(attenuation_db: R) -> Result<R, FilterDesignError>
where
    R: Float + RealField,
{
    // Both Butterworth and Chebyshev-I order formulas are written in terms of
    // the prototype ripple parameter `epsilon`.
    let value = R::from(10.0)
        .unwrap()
        .powf(attenuation_db / R::from(10.0).unwrap())
        - R::one();
    if !value.is_finite() || value <= R::zero() {
        return Err(FilterDesignError::InfeasibleOrderSelectionSpec);
    }
    Ok(value.sqrt())
}

fn minimum_butterworth_order<R>(
    epsilon_p: R,
    epsilon_s: R,
    omega_s: R,
) -> Result<usize, FilterDesignError>
where
    R: Float + RealField,
{
    if epsilon_s <= epsilon_p || omega_s <= R::one() {
        return Err(FilterDesignError::InfeasibleOrderSelectionSpec);
    }
    let order = ((epsilon_s / epsilon_p).ln() / omega_s.ln())
        .ceil()
        .to_usize()
        .ok_or(FilterDesignError::InfeasibleOrderSelectionSpec)?;
    Ok(order.max(1))
}

fn minimum_chebyshev1_order<R>(
    epsilon_p: R,
    epsilon_s: R,
    omega_s: R,
) -> Result<usize, FilterDesignError>
where
    R: Float + RealField,
{
    if epsilon_s <= epsilon_p || omega_s <= R::one() {
        return Err(FilterDesignError::InfeasibleOrderSelectionSpec);
    }
    let ratio = epsilon_s / epsilon_p;
    let order = (acosh_real(ratio) / acosh_real(omega_s))
        .ceil()
        .to_usize()
        .ok_or(FilterDesignError::InfeasibleOrderSelectionSpec)?;
    Ok(order.max(1))
}

fn butterworth_critical_shape<R>(
    passband: FilterShape<R>,
    order: usize,
    epsilon_p: R,
) -> FilterShape<R>
where
    R: Float + RealField,
{
    let order_r = R::from(order).unwrap();
    // Butterworth order selection yields the prototype cutoff `Omega_c`, which
    // then scales the caller's passband edges back into physical units.
    let omega_c = R::one() / epsilon_p.powf(R::one() / order_r);
    match passband {
        FilterShape::Lowpass { cutoff } => FilterShape::Lowpass {
            cutoff: cutoff * omega_c,
        },
        FilterShape::Highpass { cutoff } => FilterShape::Highpass {
            cutoff: cutoff / omega_c,
        },
        FilterShape::Bandpass {
            low_cutoff,
            high_cutoff,
        } => {
            let bandwidth = (high_cutoff - low_cutoff) * omega_c;
            let omega0 = (low_cutoff * high_cutoff).sqrt();
            let radical = (bandwidth * bandwidth + R::from(4.0).unwrap() * omega0 * omega0).sqrt();
            FilterShape::Bandpass {
                low_cutoff: (radical - bandwidth) / R::from(2.0).unwrap(),
                high_cutoff: (radical + bandwidth) / R::from(2.0).unwrap(),
            }
        }
        FilterShape::Bandstop {
            low_cutoff,
            high_cutoff,
        } => {
            let bandwidth = (high_cutoff - low_cutoff) / omega_c;
            let omega0 = (low_cutoff * high_cutoff).sqrt();
            let radical = (bandwidth * bandwidth + R::from(4.0).unwrap() * omega0 * omega0).sqrt();
            FilterShape::Bandstop {
                low_cutoff: (radical - bandwidth) / R::from(2.0).unwrap(),
                high_cutoff: (radical + bandwidth) / R::from(2.0).unwrap(),
            }
        }
    }
}

fn acosh_real<R>(value: R) -> R
where
    R: Float + RealField,
{
    (value + (value * value - R::one()).sqrt()).ln()
}

#[cfg(test)]
mod tests {
    use super::{
        AnalogOrderSelectionSpec, DigitalOrderSelectionSpec, FilterShape, buttord_analog,
        buttord_digital, cheb1ord_digital,
    };
    use crate::control::lti::filter_design::{design_analog_filter_zpk, design_digital_filter_zpk};
    use faer::complex::Complex;
    use nalgebra::Normed;

    fn attenuation_db(value: f64) -> f64 {
        -20.0 * value.log10()
    }

    fn assert_attenuation_le(value: f64, limit_db: f64, tol_db: f64) {
        let atten = attenuation_db(value);
        assert!(
            atten <= limit_db + tol_db,
            "attenuation {atten} dB exceeds limit {limit_db} dB"
        );
    }

    fn assert_attenuation_ge(value: f64, limit_db: f64, tol_db: f64) {
        let atten = attenuation_db(value);
        assert!(
            atten + tol_db >= limit_db,
            "attenuation {atten} dB below limit {limit_db} dB"
        );
    }

    fn eval_digital_at(
        filter: &crate::control::lti::DiscreteZpk<f64>,
        sample_rate: f64,
        omega: f64,
    ) -> f64 {
        let phase = omega / sample_rate;
        filter
            .evaluate(Complex::new(phase.cos(), phase.sin()))
            .norm()
    }

    #[test]
    fn analog_butterworth_lowpass_order_selection_meets_specs() {
        let spec = AnalogOrderSelectionSpec::new(
            FilterShape::Lowpass { cutoff: 1.0 },
            FilterShape::Lowpass { cutoff: 2.0 },
            1.0,
            40.0,
        )
        .unwrap();
        let result = buttord_analog(&spec).unwrap();
        let filter = design_analog_filter_zpk(&result.spec).unwrap();

        assert_eq!(result.spec.order, result.order);
        assert_attenuation_le(filter.evaluate(Complex::new(0.0, 1.0)).norm(), 1.0, 1.0e-6);
        assert_attenuation_ge(filter.evaluate(Complex::new(0.0, 2.0)).norm(), 40.0, 1.0e-4);
    }

    #[test]
    fn digital_butterworth_bandstop_order_selection_meets_specs() {
        let spec = DigitalOrderSelectionSpec::new(
            FilterShape::Bandstop {
                low_cutoff: 20.0,
                high_cutoff: 40.0,
            },
            FilterShape::Bandstop {
                low_cutoff: 25.0,
                high_cutoff: 35.0,
            },
            1.0,
            35.0,
            100.0,
        )
        .unwrap();
        let result = buttord_digital(&spec).unwrap();
        let filter = design_digital_filter_zpk(&result.spec).unwrap();

        assert_eq!(result.spec.order, result.order);
        assert_attenuation_le(
            eval_digital_at(&filter, spec.sample_rate, 20.0),
            1.0,
            5.0e-2,
        );
        assert_attenuation_le(
            eval_digital_at(&filter, spec.sample_rate, 40.0),
            1.0,
            5.0e-2,
        );
        assert_attenuation_ge(
            eval_digital_at(&filter, spec.sample_rate, 25.0),
            35.0,
            5.0e-2,
        );
        assert_attenuation_ge(
            eval_digital_at(&filter, spec.sample_rate, 35.0),
            35.0,
            5.0e-2,
        );
    }

    #[test]
    fn digital_chebyshev_bandpass_order_selection_meets_specs() {
        let spec = DigitalOrderSelectionSpec::new(
            FilterShape::Bandpass {
                low_cutoff: 20.0,
                high_cutoff: 30.0,
            },
            FilterShape::Bandpass {
                low_cutoff: 15.0,
                high_cutoff: 35.0,
            },
            1.0,
            40.0,
            100.0,
        )
        .unwrap();
        let result = cheb1ord_digital(&spec).unwrap();
        let filter = design_digital_filter_zpk(&result.spec).unwrap();

        assert_eq!(result.spec.order, result.order);
        assert_attenuation_le(
            eval_digital_at(&filter, spec.sample_rate, 20.0),
            1.0,
            5.0e-2,
        );
        assert_attenuation_le(
            eval_digital_at(&filter, spec.sample_rate, 30.0),
            1.0,
            5.0e-2,
        );
        assert_attenuation_ge(
            eval_digital_at(&filter, spec.sample_rate, 15.0),
            40.0,
            2.0e-1,
        );
        assert_attenuation_ge(
            eval_digital_at(&filter, spec.sample_rate, 35.0),
            40.0,
            2.0e-1,
        );
    }
}
