use super::error::FilterDesignError;
use faer_traits::RealField;
use num_traits::Float;

/// Supported filter shapes.
///
/// All frequencies are interpreted as angular frequencies in the same
/// physical units as the caller's broader model.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum FilterShape<R> {
    /// Lowpass filter with one cutoff.
    Lowpass {
        /// Cutoff angular frequency.
        cutoff: R,
    },
    /// Highpass filter with one cutoff.
    Highpass {
        /// Cutoff angular frequency.
        cutoff: R,
    },
    /// Bandpass filter with lower and upper band edges.
    Bandpass {
        /// Lower cutoff angular frequency.
        low_cutoff: R,
        /// Upper cutoff angular frequency.
        high_cutoff: R,
    },
    /// Bandstop / notch filter with lower and upper stopband edges.
    Bandstop {
        /// Lower cutoff angular frequency.
        low_cutoff: R,
        /// Upper cutoff angular frequency.
        high_cutoff: R,
    },
}

/// Analog filter families supported by this design layer.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum AnalogFilterFamily<R> {
    /// Maximally flat magnitude in the passband.
    Butterworth,
    /// Equiripple passband with ripple specified in dB.
    Chebyshev1 {
        /// Peak passband ripple in decibels.
        ripple_db: R,
    },
    /// Bessel / Thomson analog prototype.
    ///
    /// This is intentionally analog only in the current design layer.
    Bessel,
}

/// Digital filter families supported by this design layer.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum DigitalFilterFamily<R> {
    /// Maximally flat magnitude in the passband.
    Butterworth,
    /// Equiripple passband with ripple specified in dB.
    Chebyshev1 {
        /// Peak passband ripple in decibels.
        ripple_db: R,
    },
}

/// Specification for analog filter design.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AnalogFilterSpec<R> {
    /// Filter order.
    pub order: usize,
    /// Prototype family.
    pub family: AnalogFilterFamily<R>,
    /// Target shape and frequencies.
    pub shape: FilterShape<R>,
}

impl<R> AnalogFilterSpec<R>
where
    R: Float + RealField,
{
    /// Creates and validates an analog filter specification.
    pub fn new(
        order: usize,
        family: AnalogFilterFamily<R>,
        shape: FilterShape<R>,
    ) -> Result<Self, FilterDesignError> {
        validate_common(order, shape)?;
        validate_analog_family(family)?;
        Ok(Self {
            order,
            family,
            shape,
        })
    }
}

/// Specification for analog filter-order selection.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AnalogOrderSelectionSpec<R> {
    /// Passband edge specification.
    pub passband: FilterShape<R>,
    /// Stopband edge specification.
    pub stopband: FilterShape<R>,
    /// Maximum passband ripple in dB.
    pub passband_ripple_db: R,
    /// Minimum stopband attenuation in dB.
    pub stopband_attenuation_db: R,
}

impl<R> AnalogOrderSelectionSpec<R>
where
    R: Float + RealField,
{
    /// Creates and validates an analog filter-order-selection specification.
    pub fn new(
        passband: FilterShape<R>,
        stopband: FilterShape<R>,
        passband_ripple_db: R,
        stopband_attenuation_db: R,
    ) -> Result<Self, FilterDesignError> {
        validate_shape(passband)?;
        validate_shape(stopband)?;
        validate_ripple(passband_ripple_db)?;
        validate_attenuation(stopband_attenuation_db)?;
        Ok(Self {
            passband,
            stopband,
            passband_ripple_db,
            stopband_attenuation_db,
        })
    }
}

/// Specification for digital IIR filter design.
///
/// The cutoff frequencies in `shape` are interpreted as physical angular
/// frequencies. `sample_rate` is the sampling rate in samples per unit time.
///
/// The digital design surface intentionally supports only Butterworth and
/// Chebyshev Type I families. Bessel remains analog-only.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DigitalFilterSpec<R> {
    /// Filter order.
    pub order: usize,
    /// Prototype family.
    pub family: DigitalFilterFamily<R>,
    /// Target shape and frequencies.
    pub shape: FilterShape<R>,
    /// Sampling rate in samples per unit time.
    pub sample_rate: R,
    /// Whether to prewarp the requested cutoff frequencies before the
    /// bilinear transform.
    pub prewarp: bool,
}

impl<R> DigitalFilterSpec<R>
where
    R: Float + RealField,
{
    /// Creates and validates a digital filter specification.
    pub fn new(
        order: usize,
        family: DigitalFilterFamily<R>,
        shape: FilterShape<R>,
        sample_rate: R,
    ) -> Result<Self, FilterDesignError> {
        validate_common(order, shape)?;
        validate_digital_family(family)?;
        if !sample_rate.is_finite() || sample_rate <= R::zero() {
            return Err(FilterDesignError::InvalidSampleRate);
        }
        validate_digital_shape(shape, sample_rate)?;
        Ok(Self {
            order,
            family,
            shape,
            sample_rate,
            prewarp: true,
        })
    }

    /// Overrides the default prewarp behavior.
    #[must_use]
    pub fn with_prewarp(mut self, prewarp: bool) -> Self {
        self.prewarp = prewarp;
        self
    }
}

/// Specification for digital filter-order selection.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DigitalOrderSelectionSpec<R> {
    /// Passband edge specification in physical angular-frequency units.
    pub passband: FilterShape<R>,
    /// Stopband edge specification in physical angular-frequency units.
    pub stopband: FilterShape<R>,
    /// Maximum passband ripple in dB.
    pub passband_ripple_db: R,
    /// Minimum stopband attenuation in dB.
    pub stopband_attenuation_db: R,
    /// Sampling rate in samples per unit time.
    pub sample_rate: R,
    /// Whether to prewarp the supplied digital edge frequencies before the
    /// bilinear-transform design path.
    pub prewarp: bool,
}

impl<R> DigitalOrderSelectionSpec<R>
where
    R: Float + RealField,
{
    /// Creates and validates a digital filter-order-selection specification.
    pub fn new(
        passband: FilterShape<R>,
        stopband: FilterShape<R>,
        passband_ripple_db: R,
        stopband_attenuation_db: R,
        sample_rate: R,
    ) -> Result<Self, FilterDesignError> {
        validate_shape(passband)?;
        validate_shape(stopband)?;
        validate_ripple(passband_ripple_db)?;
        validate_attenuation(stopband_attenuation_db)?;
        if !sample_rate.is_finite() || sample_rate <= R::zero() {
            return Err(FilterDesignError::InvalidSampleRate);
        }
        validate_digital_shape(passband, sample_rate)?;
        validate_digital_shape(stopband, sample_rate)?;
        Ok(Self {
            passband,
            stopband,
            passband_ripple_db,
            stopband_attenuation_db,
            sample_rate,
            prewarp: true,
        })
    }

    /// Overrides the default prewarp behavior.
    #[must_use]
    pub fn with_prewarp(mut self, prewarp: bool) -> Self {
        self.prewarp = prewarp;
        self
    }
}

fn validate_common<R: Float + RealField>(
    order: usize,
    shape: FilterShape<R>,
) -> Result<(), FilterDesignError> {
    if order == 0 {
        return Err(FilterDesignError::InvalidOrder);
    }
    validate_shape(shape)
}

pub(super) fn validate_shape<R: Float + RealField>(
    shape: FilterShape<R>,
) -> Result<(), FilterDesignError> {
    match shape {
        FilterShape::Lowpass { cutoff } | FilterShape::Highpass { cutoff } => {
            validate_positive_cutoff(cutoff, "cutoff")
        }
        FilterShape::Bandpass {
            low_cutoff,
            high_cutoff,
        }
        | FilterShape::Bandstop {
            low_cutoff,
            high_cutoff,
        } => {
            validate_positive_cutoff(low_cutoff, "low_cutoff")?;
            validate_positive_cutoff(high_cutoff, "high_cutoff")?;
            if low_cutoff >= high_cutoff {
                return Err(FilterDesignError::InvalidBandEdges);
            }
            Ok(())
        }
    }
}

fn validate_analog_family<R: Float + RealField>(
    family: AnalogFilterFamily<R>,
) -> Result<(), FilterDesignError> {
    match family {
        AnalogFilterFamily::Butterworth | AnalogFilterFamily::Bessel => Ok(()),
        AnalogFilterFamily::Chebyshev1 { ripple_db } => validate_ripple(ripple_db),
    }
}

fn validate_digital_family<R: Float + RealField>(
    family: DigitalFilterFamily<R>,
) -> Result<(), FilterDesignError> {
    match family {
        DigitalFilterFamily::Butterworth => Ok(()),
        DigitalFilterFamily::Chebyshev1 { ripple_db } => validate_ripple(ripple_db),
    }
}

pub(super) fn validate_digital_shape<R: Float + RealField>(
    shape: FilterShape<R>,
    sample_rate: R,
) -> Result<(), FilterDesignError> {
    let nyquist = sample_rate * R::from(core::f64::consts::PI).unwrap();
    match shape {
        FilterShape::Lowpass { cutoff } | FilterShape::Highpass { cutoff } => {
            if cutoff >= nyquist {
                return Err(FilterDesignError::InvalidCutoff { which: "cutoff" });
            }
            Ok(())
        }
        FilterShape::Bandpass {
            low_cutoff,
            high_cutoff,
        }
        | FilterShape::Bandstop {
            low_cutoff,
            high_cutoff,
        } => {
            if high_cutoff >= nyquist || low_cutoff >= nyquist {
                return Err(FilterDesignError::InvalidBandEdges);
            }
            Ok(())
        }
    }
}

fn validate_positive_cutoff<R: Float + RealField>(
    cutoff: R,
    which: &'static str,
) -> Result<(), FilterDesignError> {
    if !cutoff.is_finite() || cutoff <= R::zero() {
        Err(FilterDesignError::InvalidCutoff { which })
    } else {
        Ok(())
    }
}

pub(super) fn validate_ripple<R: Float + RealField>(ripple_db: R) -> Result<(), FilterDesignError> {
    if !ripple_db.is_finite() || ripple_db <= R::zero() {
        Err(FilterDesignError::InvalidRipple)
    } else {
        Ok(())
    }
}

pub(super) fn validate_attenuation<R: Float + RealField>(
    attenuation_db: R,
) -> Result<(), FilterDesignError> {
    if !attenuation_db.is_finite() || attenuation_db <= R::zero() {
        Err(FilterDesignError::InvalidAttenuation)
    } else {
        Ok(())
    }
}
