pub use operating_roundtrip::*;

use super::model_numbers;

/// Rev7 model number.
pub const MODEL_NUMBER: super::ModelNumber = model_numbers::DEIMOS_DAQ_REV_7_MODEL_NUMBER;

/// Number of ADC channels reported by deimos DAQ rev7.
pub const ADC_CHANNEL_COUNT: usize = 18;

/// Number of rev7 ADC low-pass filters, one per reported ADC channel.
pub const ADC_FILTER_COUNT: usize = ADC_CHANNEL_COUNT;

/// Number of unrolled counter channels reported by deimos DAQ rev7.
pub const COUNTER_CHANNEL_COUNT: usize = 2;

/// Number of frequency-measurement channels reported by deimos DAQ rev7.
pub const FREQUENCY_CHANNEL_COUNT: usize = 2;

/// Number of PWM output channels accepted by deimos DAQ rev7.
pub const PWM_CHANNEL_COUNT: usize = 4;

/// Number of DAC output channels accepted by deimos DAQ rev7.
pub const DAC_CHANNEL_COUNT: usize = 2;

/// Number of digital output bits accepted by deimos DAQ rev7.
pub const DIGITAL_OUTPUT_COUNT: usize = 4;

/// Number of digital input bits reported by deimos DAQ rev7.
pub const DIGITAL_INPUT_COUNT: usize = 2;

/// ADC sample frequency.
pub const ADC_SAMPLE_FREQ_HZ: u32 = 33_000;

/// ADC sample frequency as a floating-point rate.
pub const ADC_SAMPLE_RATE_HZ: f64 = ADC_SAMPLE_FREQ_HZ as f64;

/// ADC and DAC voltage reference.
pub const VREF: f32 = 2.5;

/// Rev7 ADC low-pass filters are second-order Butterworth filters.
pub const ADC_FILTER_ORDER: usize = 2;

/// A second-order low-pass Butterworth design has one second-order section.
pub const ADC_FILTER_SECTIONS: usize = 1;

/// Conservative upper cutoff ratio used by the firmware ADC filters.
pub const ADC_FILTER_MAX_CUTOFF_RATIO: f64 = 0.4;

/// Rev7 ADC fractional-delay filters use third-order Lagrange FIR interpolation.
pub const ADC_FRACTIONAL_DELAY_FILTER_TAPS: usize = 3;

/// ADC clock used for rev7 ADC conversion timing.
pub const ADC_CLOCK_HZ: f64 = 50_000_000.0;

/// ADC sample-and-hold duration in ADC clock cycles.
pub const ADC_SAMPLE_HOLD_CYCLES: f64 = 16.5;

/// ADC conversion duration in ADC clock cycles, from STM32H7 RM0433 25.4.13.
pub const ADC_CONVERSION_CYCLES: f64 = 7.5;

pub mod operating_roundtrip {
    use core::default::Default;

    pub use byte_struct::{ByteStruct, ByteStructLen, ByteStructUnspecifiedByteOrder};

    use crate::OperatingMetrics;

    #[derive(ByteStruct, Clone, Copy, Debug)]
    #[byte_struct_le]
    pub struct OperatingRoundtripInput {
        /// Application-level packet ID
        pub id: u64,

        /// Adjustment to apply to board cycle duration to
        /// synchronize phase.
        ///
        /// This part is preserved if a packet from the controller is missed.
        ///
        /// For a PID timing controller, this would be the integral term.
        pub period_delta_ns: i64,

        /// Adjustment to apply to board cycle duration to
        /// synchronize phase.
        ///
        /// This part is applied for a single cycle, and is not
        /// preserved between cycles.
        ///
        /// For a PID timing controller, this would be the `P` and `D` terms.
        pub phase_delta_ns: i64,

        /// PWM duty cycle in range of [0, 1]
        pub pwm_duty_frac: [f32; super::PWM_CHANNEL_COUNT],

        /// PWM frequency in Hz
        /// PWM counters are buffered, so when using PWMs as
        /// GPIO by setting duty cycle to 0%/100%, pwm
        /// frequency should be set high to produce a quick
        /// response.
        pub pwm_freq_hz: [u32; super::PWM_CHANNEL_COUNT],

        /// Digital-to-analog converter analog output voltage.
        /// 0-2.5V range.
        pub dac_v: [f32; super::DAC_CHANNEL_COUNT],

        /// GPIO pin states.
        /// Only bits 0-3 are used.
        pub gpio: u8,
    }

    impl Default for OperatingRoundtripInput {
        /// Default PWM frequency is nonzero in order to allow rapidly updating to a new
        /// frequency from the default state.
        fn default() -> Self {
            Self {
                id: 0,
                period_delta_ns: 0,
                phase_delta_ns: 0,
                pwm_duty_frac: [0.0_f32; super::PWM_CHANNEL_COUNT],
                pwm_freq_hz: [1_000_000_u32; super::PWM_CHANNEL_COUNT],
                dac_v: [0.0_f32; super::DAC_CHANNEL_COUNT],
                gpio: 0,
            }
        }
    }

    #[derive(ByteStruct, Clone, Copy, Debug, Default)]
    #[byte_struct_le]
    pub struct OperatingRoundtripOutput {
        pub metrics: OperatingMetrics,
        pub adc_voltages: [f32; super::ADC_CHANNEL_COUNT],
        pub encoder: i64,
        pub pulse_counter: i64,
        pub frequency_meas: [f32; super::FREQUENCY_CHANNEL_COUNT],

        /// GPIO inputs. Only bits 0-1 are used.
        pub gpio: u8,
    }
}

#[cfg(feature = "alloc")]
pub mod filters {
    use core::fmt;

    use super::{
        ADC_CHANNEL_COUNT, ADC_CLOCK_HZ, ADC_CONVERSION_CYCLES, ADC_FILTER_COUNT,
        ADC_FILTER_MAX_CUTOFF_RATIO, ADC_FILTER_ORDER, ADC_FILTER_SECTIONS,
        ADC_FRACTIONAL_DELAY_FILTER_TAPS, ADC_SAMPLE_HOLD_CYCLES,
    };
    use deimos_numerics::{
        control::lti::{
            butter, design_digital_filter_tf, DiscreteTransferFunction, FilterDesignError,
            Fir as DynamicFir, LtiError,
        },
        embedded::{
            error::EmbeddedError,
            fixed::lti::{
                lagrange_fractional_delay, lagrange_fractional_delay_taps,
                DeltaSos as FixedDeltaSos, DeltaSosState as FixedDeltaSosState, Fir as FixedFir,
                FirState as FixedFirState,
            },
        },
    };

    /// Runtime ADC low-pass filter used by rev7 firmware.
    pub type AdcFilter = FixedDeltaSos<f32, ADC_FILTER_SECTIONS, 1>;

    /// Runtime state for one rev7 ADC low-pass filter.
    pub type AdcFilterState = FixedDeltaSosState<f32, ADC_FILTER_SECTIONS, 1>;

    /// Full rev7 ADC low-pass filter bank.
    pub type AdcFilterBank = [AdcFilter; ADC_FILTER_COUNT];

    /// Transfer function corresponding to one rev7 ADC low-pass filter.
    pub type AdcFilterTransferFunction = DiscreteTransferFunction<f64>;

    /// Transfer functions corresponding to the full rev7 ADC low-pass filter bank.
    pub type AdcFilterTransferFunctionBank = [AdcFilterTransferFunction; ADC_FILTER_COUNT];

    /// Runtime fractional-delay filter used to align rev7 ADC channel samples.
    pub type AdcFractionalDelayFilter = FixedFir<f32, ADC_FRACTIONAL_DELAY_FILTER_TAPS, 1>;

    /// Runtime state for one rev7 ADC fractional-delay filter.
    pub type AdcFractionalDelayFilterState =
        FixedFirState<f32, ADC_FRACTIONAL_DELAY_FILTER_TAPS, 1>;

    /// Full rev7 ADC fractional-delay filter bank.
    pub type AdcFractionalDelayFilterBank = [AdcFractionalDelayFilter; ADC_FILTER_COUNT];

    /// Transfer function corresponding to one rev7 ADC fractional-delay filter.
    pub type AdcFractionalDelayTransferFunction = DiscreteTransferFunction<f64>;

    /// Transfer functions corresponding to the full rev7 ADC fractional-delay filter bank.
    pub type AdcFractionalDelayTransferFunctionBank =
        [AdcFractionalDelayTransferFunction; ADC_FILTER_COUNT];

    /// Error returned while constructing rev7 ADC filters.
    #[derive(Debug)]
    pub enum AdcFilterBuildError {
        /// Filter design failed.
        FilterDesign(FilterDesignError),
        /// LTI representation conversion failed.
        Lti(LtiError),
        /// Fixed-size embedded representation conversion failed.
        Embedded(EmbeddedError),
    }

    impl fmt::Display for AdcFilterBuildError {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            fmt::Debug::fmt(self, f)
        }
    }

    impl core::error::Error for AdcFilterBuildError {}

    impl From<FilterDesignError> for AdcFilterBuildError {
        fn from(value: FilterDesignError) -> Self {
            Self::FilterDesign(value)
        }
    }

    impl From<LtiError> for AdcFilterBuildError {
        fn from(value: LtiError) -> Self {
            Self::Lti(value)
        }
    }

    impl From<EmbeddedError> for AdcFilterBuildError {
        fn from(value: EmbeddedError) -> Self {
            Self::Embedded(value)
        }
    }

    /// Builds the fixed-size delta-SOS ADC filter bank used by rev7 firmware.
    pub fn adc_filter_bank(cutoff_ratio: f64) -> Result<AdcFilterBank, AdcFilterBuildError> {
        let filter = adc_filter(cutoff_ratio)?;
        Ok([filter; ADC_FILTER_COUNT])
    }

    /// Builds transfer functions corresponding to the rev7 ADC filter bank.
    ///
    /// The returned transfer functions use a normalized sample interval of one
    /// sample, matching the normalized cutoff-ratio basis used by the firmware
    /// filter construction.
    pub fn adc_filter_transfer_functions(
        cutoff_ratio: f64,
    ) -> Result<AdcFilterTransferFunctionBank, AdcFilterBuildError> {
        let cutoff_ratio = clamp_adc_filter_cutoff_ratio(cutoff_ratio);
        let transfer_function =
            design_digital_filter_tf(&deimos_numerics::control::lti::DigitalFilterSpec::new(
                ADC_FILTER_ORDER,
                deimos_numerics::control::lti::DigitalFilterFamily::Butterworth,
                deimos_numerics::control::lti::FilterShape::Lowpass {
                    cutoff: cutoff_ratio * core::f64::consts::TAU,
                },
                1.0,
            )?)?;

        Ok(core::array::from_fn(|_| transfer_function.clone()))
    }

    /// Builds the fractional-delay FIR filter bank used to align rev7 ADC channels.
    pub fn adc_fractional_delay_filter_bank(
        sample_rate_hz: f64,
    ) -> Result<AdcFractionalDelayFilterBank, AdcFilterBuildError> {
        let delay_samples = adc_fractional_delay_samples(sample_rate_hz)?;
        let sample_time = (1.0 / sample_rate_hz) as f32;
        let mut filters = [lagrange_fractional_delay::<ADC_FRACTIONAL_DELAY_FILTER_TAPS, 1, f32>(
            0.0,
            sample_time,
        )?; ADC_FILTER_COUNT];

        for (filter, &delay) in filters.iter_mut().zip(delay_samples.iter()) {
            *filter = lagrange_fractional_delay::<ADC_FRACTIONAL_DELAY_FILTER_TAPS, 1, f32>(
                delay as f32,
                sample_time,
            )?;
        }

        Ok(filters)
    }

    /// Builds transfer functions corresponding to the rev7 ADC fractional-delay filter bank.
    pub fn adc_fractional_delay_transfer_functions(
        sample_rate_hz: f64,
    ) -> Result<AdcFractionalDelayTransferFunctionBank, AdcFilterBuildError> {
        let delay_samples = adc_fractional_delay_samples(sample_rate_hz)?;
        let sample_time = 1.0 / sample_rate_hz;
        let mut output: [Option<AdcFractionalDelayTransferFunction>; ADC_FILTER_COUNT] =
            core::array::from_fn(|_| None);
        for (idx, delay) in delay_samples.into_iter().enumerate() {
            let taps =
                lagrange_fractional_delay_taps::<ADC_FRACTIONAL_DELAY_FILTER_TAPS, f64>(delay)?;
            output[idx] = Some(DynamicFir::new(taps, sample_time)?.to_transfer_function()?);
        }

        Ok(output.map(|transfer_function| transfer_function.unwrap()))
    }

    fn adc_filter(cutoff_ratio: f64) -> Result<AdcFilter, AdcFilterBuildError> {
        let cutoff_ratio = clamp_adc_filter_cutoff_ratio(cutoff_ratio);
        let dynamic_delta = butter::<ADC_FILTER_ORDER>(cutoff_ratio)
            .and_then(|filter| filter.try_cast::<f32>().map_err(FilterDesignError::from))?;
        Ok(AdcFilter::try_from(&dynamic_delta)?)
    }

    fn clamp_adc_filter_cutoff_ratio(cutoff_ratio: f64) -> f64 {
        cutoff_ratio.min(ADC_FILTER_MAX_CUTOFF_RATIO)
    }

    fn adc_fractional_delay_samples(
        sample_rate_hz: f64,
    ) -> Result<[f64; ADC_FILTER_COUNT], AdcFilterBuildError> {
        if !sample_rate_hz.is_finite() || sample_rate_hz <= 0.0 {
            return Err(EmbeddedError::InvalidParameter {
                which: "adc.sample_rate_hz",
            }
            .into());
        }

        let delay_per_group = (ADC_SAMPLE_HOLD_CYCLES + ADC_CONVERSION_CYCLES) / ADC_CLOCK_HZ;
        let sample_time = 1.0 / sample_rate_hz;
        let mut delays = [0.0_f64; ADC_CHANNEL_COUNT + super::DAC_CHANNEL_COUNT];

        let groups = (
            [8, 9, 0],
            [10, 12, 1],
            [11, 2],
            [15 - 2, 3],
            [16 - 2, 17 - 2, 4],
            [18 - 2, 5],
            [19 - 2, 6],
            [7],
        );

        let mut apply_delay = |group: &[usize], group_idx: usize| {
            let delay = group_idx as f64 * delay_per_group;
            for &channel in group {
                delays[channel] = delay;
            }
        };

        apply_delay(&groups.0, 0);
        apply_delay(&groups.1, 1);
        apply_delay(&groups.2, 2);
        apply_delay(&groups.3, 3);
        apply_delay(&groups.4, 4);
        apply_delay(&groups.5, 5);
        apply_delay(&groups.6, 6);
        apply_delay(&groups.7, 7);

        Ok(core::array::from_fn(|idx| delays[idx] / sample_time))
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn rev7_adc_filter_helpers_build_full_banks() {
            let filters = adc_filter_bank(0.1).unwrap();
            let transfer_functions = adc_filter_transfer_functions(0.1).unwrap();
            let fractional_delay_filters =
                adc_fractional_delay_filter_bank(super::super::ADC_SAMPLE_RATE_HZ).unwrap();
            let fractional_delay_transfer_functions =
                adc_fractional_delay_transfer_functions(super::super::ADC_SAMPLE_RATE_HZ).unwrap();

            assert_eq!(filters.len(), ADC_FILTER_COUNT);
            assert_eq!(transfer_functions.len(), ADC_FILTER_COUNT);
            assert_eq!(transfer_functions[0].domain().sample_time(), 1.0);
            assert!(!transfer_functions[0].numerator().is_empty());
            assert!(!transfer_functions[0].denominator().is_empty());
            assert_eq!(fractional_delay_filters.len(), ADC_FILTER_COUNT);
            assert_eq!(fractional_delay_transfer_functions.len(), ADC_FILTER_COUNT);
            assert_eq!(
                fractional_delay_transfer_functions[0]
                    .domain()
                    .sample_time(),
                1.0 / super::super::ADC_SAMPLE_RATE_HZ
            );
            assert!(!fractional_delay_transfer_functions[0]
                .numerator()
                .is_empty());
        }
    }
}

#[cfg(feature = "alloc")]
pub use filters::{
    adc_filter_bank, adc_filter_transfer_functions, adc_fractional_delay_filter_bank,
    adc_fractional_delay_transfer_functions, AdcFilter, AdcFilterBank, AdcFilterBuildError,
    AdcFilterState, AdcFilterTransferFunction, AdcFilterTransferFunctionBank,
    AdcFractionalDelayFilter, AdcFractionalDelayFilterBank, AdcFractionalDelayFilterState,
    AdcFractionalDelayTransferFunction, AdcFractionalDelayTransferFunctionBank,
};
