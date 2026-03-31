pub use operating_roundtrip::*;

/// Maximum supported ADC sample rate for the rev7 DAQ.
pub const MAX_ADC_SAMPLE_RATE_HZ: u32 = 30_000;

/// Maximum supported reporting rate for the rev7 DAQ.
pub const MAX_REPORTING_RATE_HZ: u32 = MAX_ADC_SAMPLE_RATE_HZ;

/// Return the number of ADC samples to take during one reporting cycle.
///
/// The rev7 firmware chooses the largest integer sample count that keeps the
/// nominal sample rate at or below [`MAX_ADC_SAMPLE_RATE_HZ`].
pub fn adc_samples_per_report(dt_ns: u32) -> u32 {
    let samples = (u64::from(MAX_ADC_SAMPLE_RATE_HZ) * u64::from(dt_ns)) / 1_000_000_000;
    samples.max(1) as u32
}

/// Return the nominal ADC sample rate implied by one reporting-cycle duration.
///
/// This uses the same quantization policy as [`adc_samples_per_report`], so host
/// software can predict the rev7 firmware's nominal samplerate from `dt_ns`.
pub fn expected_adc_sample_rate_hz(dt_ns: u32) -> u32 {
    let sample_rate =
        (u64::from(adc_samples_per_report(dt_ns)) * 1_000_000_000) / u64::from(dt_ns.max(1));
    sample_rate.min(u64::from(MAX_ADC_SAMPLE_RATE_HZ)) as u32
}

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
        pub pwm_duty_frac: [f32; 4],

        /// PWM frequency in Hz
        /// PWM counters are buffered, so when using PWMs as
        /// GPIO by setting duty cycle to 0%/100%, pwm
        /// frequency should be set high to produce a quick
        /// response.
        pub pwm_freq_hz: [u32; 4],

        /// Digital-to-analog converter analog output voltage.
        /// 0-2.5V range.
        pub dac_v: [f32; 2],

        /// GPIO pin states.
        /// Only bits 0-3 are used.
        pub gpio: u8
    }

    impl Default for OperatingRoundtripInput {
        /// Default PWM frequency is nonzero in order to allow rapidly updating to a new
        /// frequency from the default state.
        fn default() -> Self {
            Self {
                id: 0,
                period_delta_ns: 0,
                phase_delta_ns: 0,
                pwm_duty_frac: [0.0_f32; 4],
                pwm_freq_hz: [1_000_000_u32; 4],
                dac_v: [0.0_f32; 2],
                gpio: 0,
            }
        }
    }

    #[derive(ByteStruct, Clone, Copy, Debug, Default)]
    #[byte_struct_le]
    pub struct OperatingRoundtripOutput {
        pub metrics: OperatingMetrics,
        pub adc_voltages: [f32; 18],
        pub encoder: i64,
        pub pulse_counter: i64,
        pub frequency_meas: [f32; 2],

        /// GPIO inputs. Only bits 0-1 are used.
        pub gpio: u8
    }
}
