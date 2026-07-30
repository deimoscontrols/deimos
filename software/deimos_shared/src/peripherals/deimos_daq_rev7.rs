pub use operating_roundtrip::*;

#[path = "deimos_daq_rev7_modbus.rs"]
pub mod modbus;

#[path = "deimos_daq_rev7_acquisition.rs"]
pub mod acquisition;

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

/// Number of rev7 4-20 mA measurement channels.
pub const CURRENT_4_20_CHANNEL_COUNT: usize = 4;

/// Number of rev7 resistance/RTD measurement channels.
pub const RTD_CHANNEL_COUNT: usize = 3;

/// Number of rev7 thermocouple measurement channels.
pub const THERMOCOUPLE_CHANNEL_COUNT: usize = 2;

/// Number of rev7 voltage measurement channels.
pub const VOLTAGE_CHANNEL_COUNT: usize = 6;

/// Target ADC-group rate for synchronous oversampling.
///
/// The actual rate is the publishing rate multiplied by the nearest integer
/// number of samples per cycle, so it moves slightly around this target.
pub const ADC_OVERSAMPLE_TARGET_HZ: u32 = 9_000;

/// Target ADC-group rate as a floating-point value.
pub const ADC_OVERSAMPLE_TARGET_RATE_HZ: f64 = ADC_OVERSAMPLE_TARGET_HZ as f64;

/// Minimum ADC groups taken in one synchronously oversampled cycle.
pub const ADC_OVERSAMPLE_MIN_SAMPLES_PER_CYCLE: u32 = 3;

/// Lowest publishing rate which uses one ADC group per cycle.
///
/// Below this compiled, non-protocol setting, the nearest integer sample count
/// targets [`ADC_OVERSAMPLE_TARGET_HZ`]. At and above it, the firmware omits the
/// ADC IIR and takes one fractional-delay-corrected group per publishing cycle.
pub const ADC_SINGLE_SAMPLE_CUTOVER_HZ: u32 = 3_000;

/// Synchronous ADC topology selected for one reporting cycle rate.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AdcSamplingMode {
    /// Take an integer number of samples near the internal-rate target and run
    /// both the fractional-delay FIR and ADC IIR on every group.
    Oversampled,
    /// Take one sample per reporting cycle and run only the fractional-delay
    /// FIR.
    Direct,
}

/// Derived rev7 sampling and filtering parameters.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AdcSamplingPolicy {
    /// Selected synchronous acquisition topology.
    pub mode: AdcSamplingMode,
    /// ADC groups acquired in each reporting cycle, in `sample/cycle`.
    pub samples_per_cycle: u32,
    /// Actual ADC-group samplerate, in `sample/s`.
    pub sample_rate_hz: f64,
    /// ADC IIR cutoff, in `Hz`, or `None` when the direct path omits the IIR.
    pub iir_cutoff_hz: Option<f64>,
    /// ADC IIR cutoff divided by samplerate, or `None` in direct mode.
    pub iir_cutoff_ratio: Option<f64>,
}

/// Derive the rev7 ADC samplerate and filter cutoff from a reporting cycle rate.
///
/// Below [`ADC_SINGLE_SAMPLE_CUTOVER_HZ`], the nearest integer sample count
/// targets [`ADC_OVERSAMPLE_TARGET_HZ`] with a minimum of
/// [`ADC_OVERSAMPLE_MIN_SAMPLES_PER_CYCLE`]. The ADC IIR cutoff remains at the
/// reporting cycle rate. At and above the cutover, one sample is acquired per
/// cycle and the ADC IIR is omitted.
///
/// Args:
///   cycle_rate_hz: Requested reporting cycle rate scalar in `cycle/s`.
///
/// Returns:
///   Derived synchronous sampling policy, or `None` for a non-finite or
///   non-positive cycle rate or an unrepresentable integer sample count.
pub fn adc_sampling_policy(cycle_rate_hz: f64) -> Option<AdcSamplingPolicy> {
    if !cycle_rate_hz.is_finite() || cycle_rate_hz <= 0.0 {
        return None;
    }

    if cycle_rate_hz >= f64::from(ADC_SINGLE_SAMPLE_CUTOVER_HZ) {
        return Some(AdcSamplingPolicy {
            mode: AdcSamplingMode::Direct,
            samples_per_cycle: 1,
            sample_rate_hz: cycle_rate_hz,
            iir_cutoff_hz: None,
            iir_cutoff_ratio: None,
        });
    }

    // Adding one half before the float-to-integer conversion gives nearest-
    // integer rounding without requiring a target libm operation.
    let rounded_samples = ADC_OVERSAMPLE_TARGET_RATE_HZ / cycle_rate_hz + 0.5;
    if rounded_samples > f64::from(u32::MAX) {
        return None;
    }
    let rounded_samples = rounded_samples as u32;
    let samples_per_cycle = rounded_samples.max(ADC_OVERSAMPLE_MIN_SAMPLES_PER_CYCLE);
    let sample_rate_hz = cycle_rate_hz * f64::from(samples_per_cycle);
    Some(AdcSamplingPolicy {
        mode: AdcSamplingMode::Oversampled,
        samples_per_cycle,
        sample_rate_hz,
        iir_cutoff_hz: Some(cycle_rate_hz),
        iir_cutoff_ratio: Some(cycle_rate_hz / sample_rate_hz),
    })
}

/// Maximum supported post-quadrature encoder count and pulse-counter edge rate.
///
/// This is the fastest rate the configured timer peripherals can count. The
/// cutover assertions below prove that it cannot move by an ambiguous half of a
/// 16-bit timer modulus between samples in either synchronous topology.
pub const COUNTER_MAX_EDGE_RATE_HZ: u32 = 50_000_000;

const _: () = assert!(ADC_OVERSAMPLE_TARGET_HZ > 0);
const _: () = assert!(ADC_OVERSAMPLE_MIN_SAMPLES_PER_CYCLE > 0);
const _: () = assert!(
    ADC_OVERSAMPLE_TARGET_HZ == ADC_SINGLE_SAMPLE_CUTOVER_HZ * ADC_OVERSAMPLE_MIN_SAMPLES_PER_CYCLE
);
// Rounding N = target / cycle to the nearest integer gives a minimum nominal
// internal rate of target * N / (N + 0.5). Its worst case occurs at the minimum
// N=3 and is 6/7 of target. Including the longest +10% timing correction makes
// the maximum oversampled interval 77 / (60 * target) seconds. Keep the counter
// change strictly below half of its 2^16 modulus.
const _: () = assert!(
    COUNTER_MAX_EDGE_RATE_HZ as u64 * 77 < (1_u64 << 15) * ADC_OVERSAMPLE_TARGET_HZ as u64 * 60
);
// Direct operation's longest interval is 1.1 / cutover.
const _: () = assert!(
    COUNTER_MAX_EDGE_RATE_HZ as u64 * 11 < (1_u64 << 15) * ADC_SINGLE_SAMPLE_CUTOVER_HZ as u64 * 10
);

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

/// Magic marker for controller-to-board binding packets.
pub const BINDING_INPUT_MAGIC: u32 = 0xD7B1_0001;
/// Magic marker for board-to-controller binding packets.
pub const BINDING_OUTPUT_MAGIC: u32 = 0xD7B1_0002;
/// Magic marker for controller-to-board configuring packets.
pub const CONFIGURING_INPUT_MAGIC: u32 = 0xD7C0_0001;
/// Magic marker for board-to-controller configuring packets.
pub const CONFIGURING_OUTPUT_MAGIC: u32 = 0xD7C0_0002;
/// Magic marker for controller-to-board Deimos operating packets.
pub const OPERATING_INPUT_MAGIC: u32 = 0xD700_0001;
/// Magic marker for board-to-controller engineering snapshots.
pub const OPERATING_SNAPSHOT_MAGIC: u32 = 0xD700_0002;

pub use packets::*;

const MODULE_BUS_CURRENT_SCALE: f32 = 1.0 / (0.006 * 50.0);
const MODULE_BUS_VOLTAGE_SCALE: f32 = 21.5 / 1.5;
const CURRENT_REFERENCE_RESISTOR_OHM: f32 = 75.0;
const RTD_REFERENCE_CURRENT_A: f32 = 250.0e-6;
const RTD_FRONTEND_GAIN: f32 = 25.7;
const TC_FRONTEND_GAIN: f32 = 25.7;
const TC_FRONTEND_OFFSET_V: f32 = 1.024;

/// Converts the rev7 cold-junction ADC channel to unfiltered board temperature.
///
/// Channel `ain2` is divided by the analog-front-end gain, calibrated at the
/// sensed-voltage point, converted to Pt100 resistance, and evaluated with the
/// shared IEC 60751 inverse.
///
/// Args:
///   samples: Coherent ADC output-voltage group in `V` with shape
///     `(ADC_CHANNEL_COUNT,)` and channel order `ain0..ain12, ain15..ain19`.
///   calibration: Per-channel sensed-voltage calibration record.
///
/// Returns:
///   Unfiltered absolute board temperature scalar in `K`.
#[inline]
pub fn board_temperature_k_f32(
    samples: &[f32; ADC_CHANNEL_COUNT],
    calibration: &Rev7Calibration,
) -> f32 {
    let sensed_v = calibration.voltage_cals[2].apply(samples[2] / RTD_FRONTEND_GAIN);
    crate::calcs::pt100_temperature_k_f32(sensed_v / RTD_REFERENCE_CURRENT_A)
}

/// Populate the analog engineering fields of a coherent rev7 snapshot.
///
/// Calibration is applied at the sensed-voltage point used by the former host
/// standard-calculation graph. The caller supplies the already-filtered board
/// temperature so both thermocouples use the same publication-cycle value.
///
/// Args:
///   output: Snapshot record whose analog engineering fields are overwritten.
///   samples: Coherent ADC output-voltage group in `V` with shape
///     `(ADC_CHANNEL_COUNT,)` and channel order `ain0..ain12, ain15..ain19`.
///   calibration: Per-channel sensed-voltage calibration record.
///   filtered_board_temperature_k: Publication-cycle cold-junction temperature
///     scalar in `K` after the `1 Hz` digital filter.
#[inline]
pub fn populate_analog_snapshot_f32(
    output: &mut OperatingSnapshot,
    samples: &[f32; ADC_CHANNEL_COUNT],
    calibration: &Rev7Calibration,
    filtered_board_temperature_k: f32,
) {
    output.module_bus_current_a = samples[0] * MODULE_BUS_CURRENT_SCALE;
    output.module_bus_voltage_v = samples[1] * MODULE_BUS_VOLTAGE_SCALE;
    output.board_temperature_k = filtered_board_temperature_k;

    for index in 0..CURRENT_4_20_CHANNEL_COUNT {
        let sample_index = 3 + index;
        let sensed_v = calibration.voltage_cals[sample_index].apply(samples[sample_index]);
        output.current_4_20_a[index] = sensed_v / CURRENT_REFERENCE_RESISTOR_OHM;
    }
    for index in 0..RTD_CHANNEL_COUNT {
        let sample_index = 7 + index;
        let sensed_v =
            calibration.voltage_cals[sample_index].apply(samples[sample_index] / RTD_FRONTEND_GAIN);
        output.rtd_resistance_ohm[index] = sensed_v / RTD_REFERENCE_CURRENT_A;
    }
    for index in 0..THERMOCOUPLE_CHANNEL_COUNT {
        let sample_index = 10 + index;
        let sensed_v = calibration.voltage_cals[sample_index]
            .apply((samples[sample_index] - TC_FRONTEND_OFFSET_V) / TC_FRONTEND_GAIN);
        output.thermocouple_temperature_k[index] =
            crate::calcs::ktype_corrected_temperature_k_f32(sensed_v, filtered_board_temperature_k);
    }

    output.voltage_v[0] = calibration.voltage_cals[12].apply(samples[12]);
    output.voltage_v[1] = calibration.voltage_cals[13].apply(samples[13]);
    output.voltage_v[2] = calibration.voltage_cals[14].apply(samples[14] * 6.0);
    output.voltage_v[3] = calibration.voltage_cals[15].apply(samples[15] * 6.0);
    output.voltage_v[4] =
        calibration.voltage_cals[16].apply((samples[16] - TC_FRONTEND_OFFSET_V) / TC_FRONTEND_GAIN);
    output.voltage_v[5] =
        calibration.voltage_cals[17].apply((samples[17] - TC_FRONTEND_OFFSET_V) / TC_FRONTEND_GAIN);
}

/// Rev7 setup packets and calibration image records.
pub mod packets {
    use byte_struct::{ByteStruct, ByteStructLen, ByteStructUnspecifiedByteOrder};

    use crate::{
        peripherals::PeripheralId,
        states::{AcknowledgeConfiguration, ConfiguringInput, Mode},
    };

    /// Rev7-specific binding request. Older hardware continues to use the generic request.
    ///
    /// Fields are serialized contiguously in little-endian order.
    #[derive(ByteStruct, Clone, Copy, Debug, Default)]
    #[byte_struct_le]
    pub struct Rev7BindingInput {
        /// Direction- and state-specific packet marker.
        pub magic: u32,
        /// Maximum configuring-state inactivity duration in `ms`.
        pub configuring_timeout_ms: u16,
    }

    impl Rev7BindingInput {
        /// Builds a binding request with the required rev7 packet marker.
        ///
        /// Args:
        ///   configuring_timeout_ms: Maximum configuring-state inactivity
        ///     duration in `ms`.
        ///
        /// Returns:
        ///   Initialized rev7 binding request.
        pub fn new(configuring_timeout_ms: u16) -> Self {
            Self {
                magic: super::BINDING_INPUT_MAGIC,
                configuring_timeout_ms,
            }
        }

        /// Checks the direction- and state-specific packet marker.
        ///
        /// Returns:
        ///   `true` when `magic` matches [`super::BINDING_INPUT_MAGIC`].
        pub fn is_valid(&self) -> bool {
            self.magic == super::BINDING_INPUT_MAGIC
        }
    }

    /// Rev7-specific binding response.
    ///
    /// Fields are serialized contiguously in little-endian order.
    #[derive(ByteStruct, Clone, Copy, Debug, Default)]
    #[byte_struct_le]
    pub struct Rev7BindingOutput {
        /// Direction- and state-specific packet marker.
        pub magic: u32,
        /// Model and serial-number identity of the responding board.
        pub peripheral_id: PeripheralId,
    }

    impl Rev7BindingOutput {
        /// Builds a binding response with the required rev7 packet marker.
        ///
        /// Args:
        ///   peripheral_id: Model and serial-number identity of the board.
        ///
        /// Returns:
        ///   Initialized rev7 binding response.
        pub fn new(peripheral_id: PeripheralId) -> Self {
            Self {
                magic: super::BINDING_OUTPUT_MAGIC,
                peripheral_id,
            }
        }

        /// Checks the packet marker and rev7 model number.
        ///
        /// Returns:
        ///   `true` when the response is marked as a rev7 binding response and
        ///   identifies a rev7 board.
        pub fn is_valid(&self) -> bool {
            self.magic == super::BINDING_OUTPUT_MAGIC
                && self.peripheral_id.model_number == super::MODEL_NUMBER
        }
    }

    /// Rev7 configuration request with a direction-specific packet marker.
    ///
    /// Fields are serialized contiguously in little-endian order.
    #[derive(ByteStruct, Clone, Copy, Debug, Default)]
    #[byte_struct_le]
    pub struct Rev7ConfiguringInput {
        /// Direction- and state-specific packet marker.
        pub magic: u32,
        /// Nominal operating-cycle duration in `ns`.
        pub dt_ns: u32,
        /// Requested operating protocol mode.
        pub mode: Mode,
        /// Delay from accepted configuration to operating entry in `ns`.
        pub timeout_to_operating_ns: u32,
        /// Consecutive missed cycles allowed before loss-of-contact shutdown.
        pub loss_of_contact_limit: u16,
    }

    impl Rev7ConfiguringInput {
        /// Adds the rev7 packet marker to a generic configuration request.
        ///
        /// Args:
        ///   base: Generic Deimos configuration values.
        ///
        /// Returns:
        ///   Equivalent rev7-specific request.
        pub fn from_base(base: ConfiguringInput) -> Self {
            Self {
                magic: super::CONFIGURING_INPUT_MAGIC,
                dt_ns: base.dt_ns,
                mode: base.mode,
                timeout_to_operating_ns: base.timeout_to_operating_ns,
                loss_of_contact_limit: base.loss_of_contact_limit,
            }
        }

        /// Checks the packet marker and currently supported operating settings.
        ///
        /// Returns:
        ///   `true` for a marked, nonzero-period roundtrip configuration.
        pub fn is_valid(&self) -> bool {
            self.magic == super::CONFIGURING_INPUT_MAGIC
                && self.dt_ns != 0
                && matches!(self.mode, Mode::Roundtrip)
        }
    }

    /// Rev7 configuration response carrying the firmware calibration status.
    ///
    /// Fields are serialized contiguously in little-endian order.
    #[derive(ByteStruct, Clone, Copy, Debug, Default)]
    #[byte_struct_le]
    pub struct Rev7ConfiguringOutput {
        /// Direction- and state-specific packet marker.
        pub magic: u32,
        /// Firmware acceptance or rejection of the configuration.
        pub acknowledge: AcknowledgeConfiguration,
        /// Calibration status encoded as `0` for identity or `1` for calibrated.
        pub firmware_calibrated: u8,
    }

    impl Rev7ConfiguringOutput {
        /// Builds a configuration response with a canonical calibration flag.
        ///
        /// Args:
        ///   acknowledge: Firmware configuration result.
        ///   firmware_calibrated: Whether the installed coefficients were
        ///     produced by the calibration procedure.
        ///
        /// Returns:
        ///   Initialized rev7 configuration response.
        pub fn new(acknowledge: AcknowledgeConfiguration, firmware_calibrated: bool) -> Self {
            Self {
                magic: super::CONFIGURING_OUTPUT_MAGIC,
                acknowledge,
                firmware_calibrated: u8::from(firmware_calibrated),
            }
        }

        /// Checks the packet marker and calibration-flag encoding.
        ///
        /// Returns:
        ///   `true` when the marker matches and the flag is either `0` or `1`.
        pub fn is_valid(&self) -> bool {
            self.magic == super::CONFIGURING_OUTPUT_MAGIC && self.firmware_calibrated <= 1
        }
    }

    /// One affine sensed-voltage calibration, `calibrated = slope * raw + offset`.
    #[derive(ByteStruct, Clone, Copy, Debug)]
    #[byte_struct_le]
    pub struct LinearCalibration {
        /// Dimensionless sensed-voltage scale factor.
        pub slope: f32,
        /// Sensed-voltage offset in `V`.
        pub offset: f32,
    }

    impl Default for LinearCalibration {
        fn default() -> Self {
            Self {
                slope: 1.0,
                offset: 0.0,
            }
        }
    }

    impl LinearCalibration {
        /// Applies this affine calibration to one sensed voltage.
        ///
        /// Args:
        ///   value: Uncalibrated sensed-voltage scalar in `V`.
        ///
        /// Returns:
        ///   Calibrated sensed-voltage scalar in `V`.
        #[inline]
        pub fn apply(&self, value: f32) -> f32 {
            value * self.slope + self.offset
        }

        /// Checks that the affine conversion is finite and invertible.
        ///
        /// Returns:
        ///   `true` when `slope` is finite and nonzero and `offset` is finite.
        pub fn is_valid(&self) -> bool {
            self.slope.is_finite() && self.slope != 0.0 && self.offset.is_finite()
        }
    }

    /// Complete calibration image embedded in rev7 firmware.
    ///
    /// `firmware_calibrated` is deliberately separate from protocol packet magic: it
    /// records whether the coefficients were produced by the calibration procedure.
    #[derive(ByteStruct, Clone, Copy, Debug)]
    #[byte_struct_le]
    pub struct Rev7Calibration {
        /// Status encoded as `0` for identity or `1` for procedurally calibrated.
        pub firmware_calibrated: u8,
        /// Sensed-voltage calibrations with shape `(ADC_CHANNEL_COUNT,)` and
        /// channel order `ain0..ain12, ain15..ain19`.
        pub voltage_cals: [LinearCalibration; super::ADC_CHANNEL_COUNT],
    }

    impl Default for Rev7Calibration {
        fn default() -> Self {
            Self {
                firmware_calibrated: 0,
                voltage_cals: [LinearCalibration::default(); super::ADC_CHANNEL_COUNT],
            }
        }
    }

    impl Rev7Calibration {
        /// Reports whether this image contains procedurally generated coefficients.
        ///
        /// Returns:
        ///   `true` only when `firmware_calibrated` is exactly `1`.
        pub fn is_calibrated(&self) -> bool {
            self.firmware_calibrated == 1
        }

        /// Checks the flag encoding and every affine calibration.
        ///
        /// Returns:
        ///   `true` when the record is safe to evaluate. Identity records are
        ///   valid but are reported as uncalibrated by [`Self::is_calibrated`].
        pub fn is_valid(&self) -> bool {
            self.firmware_calibrated <= 1
                && self.voltage_cals.iter().all(LinearCalibration::is_valid)
        }
    }
}

/// Rev7 analog front-end low-pass filter variants, ordered by reported ADC channel.
///
/// The channel order is `ain0..ain12` followed by `ain15..ain19`.
pub const ADC_ANALOG_FRONTEND_FILTER_KINDS: [AdcAnalogFrontendFilterKind; ADC_CHANNEL_COUNT] = [
    AdcAnalogFrontendFilterKind::Unfiltered,
    AdcAnalogFrontendFilterKind::Unfiltered,
    AdcAnalogFrontendFilterKind::SallenKey100Hz,
    AdcAnalogFrontendFilterKind::SallenKey3kHz,
    AdcAnalogFrontendFilterKind::SallenKey3kHz,
    AdcAnalogFrontendFilterKind::SallenKey3kHz,
    AdcAnalogFrontendFilterKind::SallenKey3kHz,
    AdcAnalogFrontendFilterKind::SallenKey3kHz,
    AdcAnalogFrontendFilterKind::SallenKey3kHz,
    AdcAnalogFrontendFilterKind::SallenKey3kHz,
    AdcAnalogFrontendFilterKind::SallenKey1kHz,
    AdcAnalogFrontendFilterKind::SallenKey1kHz,
    AdcAnalogFrontendFilterKind::SallenKey3kHz,
    AdcAnalogFrontendFilterKind::SallenKey3kHz,
    AdcAnalogFrontendFilterKind::SallenKey3kHz,
    AdcAnalogFrontendFilterKind::SallenKey3kHz,
    AdcAnalogFrontendFilterKind::SallenKey1kHz,
    AdcAnalogFrontendFilterKind::SallenKey1kHz,
];

/// Rev7 analog front-end filter variant for one reported ADC voltage channel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AdcAnalogFrontendFilterKind {
    /// No analog low-pass filter is modeled for this channel.
    Unfiltered,
    /// Sallen-Key 100 Hz target, R1 = R2 = 100 kohm and C1 = C2 = 10 nF.
    SallenKey100Hz,
    /// Sallen-Key 1 kHz target, R1 = R2 = 10 kohm and C1 = C2 = 10 nF.
    SallenKey1kHz,
    /// Sallen-Key 3 kHz target, R1 = R2 = 3.3 kohm and C1 = C2 = 10 nF.
    SallenKey3kHz,
}

pub mod operating_roundtrip {
    use core::default::Default;

    pub use byte_struct::{ByteStruct, ByteStructLen, ByteStructUnspecifiedByteOrder};

    use crate::OperatingMetrics;

    /// Complete rev7 output state shared by Deimos and Modbus operating modes.
    ///
    /// Fixed-size array fields state their wire shapes below. The struct is
    /// serialized inline in the Deimos roundtrip input and is also retained
    /// directly across future Modbus operating re-entry.
    #[derive(ByteStruct, Clone, Copy, Debug, PartialEq)]
    #[byte_struct_le]
    pub struct OperatingOutputSettings {
        /// PWM duty fractions with shape `(PWM_CHANNEL_COUNT,)` and range `[0, 1]`.
        pub pwm_duty_frac: [f32; super::PWM_CHANNEL_COUNT],

        /// PWM frequencies in `Hz` with shape `(PWM_CHANNEL_COUNT,)`.
        ///
        /// PWM counters are buffered, so when using PWMs as GPIO by setting
        /// duty cycle to 0%/100%, the frequency should be high enough to
        /// produce the required response time.
        pub pwm_freq_hz: [u32; super::PWM_CHANNEL_COUNT],

        /// DAC output voltages in `V` with shape `(DAC_CHANNEL_COUNT,)` and
        /// range `[0, VREF]`.
        pub dac_v: [f32; super::DAC_CHANNEL_COUNT],

        /// GPIO output-state bit field; only bits `0..=3` are used.
        pub gpio: u8,
    }

    impl Default for OperatingOutputSettings {
        /// Returns the safe output state with nonzero PWM carrier frequencies.
        fn default() -> Self {
            Self {
                pwm_duty_frac: [0.0_f32; super::PWM_CHANNEL_COUNT],
                pwm_freq_hz: [1_000_000_u32; super::PWM_CHANNEL_COUNT],
                dac_v: [0.0_f32; super::DAC_CHANNEL_COUNT],
                gpio: 0,
            }
        }
    }

    impl OperatingOutputSettings {
        /// Checks all safety-relevant output ranges.
        ///
        /// Returns:
        ///   `true` when every PWM, DAC, and GPIO setting is valid for rev7
        ///   firmware.
        pub fn is_valid(&self) -> bool {
            self.pwm_duty_frac
                .iter()
                .all(|value| value.is_finite() && (0.0..=1.0).contains(value))
                && self.pwm_freq_hz.iter().all(|&value| value != 0)
                && self
                    .dac_v
                    .iter()
                    .all(|value| value.is_finite() && (0.0..=super::VREF).contains(value))
                && self.gpio & !0x0f == 0
        }
    }

    /// Default Modbus publishing period, corresponding to 10 Hz.
    pub const MODBUS_DEFAULT_DT_NS: u32 = 100_000_000;
    /// Default one-minute Modbus contact timeout at 10 Hz.
    pub const MODBUS_DEFAULT_LOSS_OF_CONTACT_LIMIT: u16 = 600;

    /// Fully resolved values needed to enter or re-enter Modbus operation.
    ///
    /// This is internal operating state rather than a serialized protocol
    /// record. Keeping the complete output state in one copyable value prevents
    /// a rare cycle-rate re-entry from briefly applying safe defaults.
    #[derive(Clone, Copy, Debug, PartialEq)]
    pub struct ModbusInitialConfig {
        /// Nominal publishing-cycle duration in `ns`.
        pub dt_ns: u32,
        /// Consecutive cycles without an accepted request before shutdown.
        pub loss_of_contact_limit: u16,
        /// Output state to apply on operating entry.
        pub outputs: OperatingOutputSettings,
    }

    impl Default for ModbusInitialConfig {
        /// Returns the documented 10 Hz, one-minute, safe-output configuration.
        fn default() -> Self {
            Self {
                dt_ns: MODBUS_DEFAULT_DT_NS,
                loss_of_contact_limit: MODBUS_DEFAULT_LOSS_OF_CONTACT_LIMIT,
                outputs: OperatingOutputSettings::default(),
            }
        }
    }

    impl ModbusInitialConfig {
        /// Builds a rate-change re-entry configuration without altering outputs.
        ///
        /// Args:
        ///   dt_ns: New nominal publishing-cycle duration in `ns`.
        ///
        /// Returns:
        ///   A configuration with the new period and the existing timeout count
        ///   and complete output state.
        pub fn reenter_at_period(self, dt_ns: u32) -> Self {
            Self { dt_ns, ..self }
        }
    }

    /// Controller command and timing-correction packet for Deimos roundtrip mode.
    ///
    /// Fixed-size array fields state their wire shapes below. Fields are serialized
    /// contiguously in little-endian order.
    #[derive(ByteStruct, Clone, Copy, Debug)]
    #[byte_struct_le]
    pub struct OperatingRoundtripInput {
        /// Direction- and state-specific packet marker.
        pub magic: u32,

        /// Monotonically increasing application-level packet identifier.
        pub id: u64,

        /// Adjustment to apply to board cycle duration to
        /// synchronize phase.
        ///
        /// This part is preserved if a packet from the controller is missed.
        ///
        /// For a PID timing controller, this would be the integral term.
        /// The scalar value is in `ns`.
        pub period_delta_ns: i64,

        /// Adjustment to apply to board cycle duration to
        /// synchronize phase.
        ///
        /// This part is applied for a single cycle, and is not
        /// preserved between cycles.
        ///
        /// For a PID timing controller, this would be the `P` and `D` terms.
        /// The scalar value is in `ns`.
        pub phase_delta_ns: i64,

        /// Complete output state applied for this Deimos roundtrip cycle.
        pub outputs: OperatingOutputSettings,
    }

    impl Default for OperatingRoundtripInput {
        /// Default PWM frequency is nonzero in order to allow rapidly updating to a new
        /// frequency from the default state.
        fn default() -> Self {
            Self {
                magic: super::OPERATING_INPUT_MAGIC,
                id: 0,
                period_delta_ns: 0,
                phase_delta_ns: 0,
                outputs: OperatingOutputSettings::default(),
            }
        }
    }

    impl OperatingRoundtripInput {
        /// Checks the packet marker and all safety-relevant output ranges.
        ///
        /// Returns:
        ///   `true` when the marker, PWM settings, DAC voltages, and GPIO mask
        ///   are valid for rev7 firmware.
        pub fn is_valid(&self) -> bool {
            self.magic == super::OPERATING_INPUT_MAGIC && self.outputs.is_valid()
        }
    }

    /// Coherent engineering-unit snapshot published by both operating transports.
    ///
    /// Analog values are the final firmware-converted outputs except for the
    /// external RTD channels, which intentionally publish resistance for the
    /// software-side temperature conversion. Fixed-size array fields state their
    /// wire shapes below. Fields are serialized contiguously in little-endian order.
    #[derive(ByteStruct, Clone, Copy, Debug)]
    #[byte_struct_le]
    pub struct OperatingSnapshot {
        /// Direction- and state-specific packet marker.
        pub magic: u32,
        /// Board timing, packet-ID, and loss-of-contact metrics.
        pub metrics: OperatingMetrics,
        /// Board time immediately before acquisition of the published ADC group, in `ns`.
        ///
        /// This is the acquisition-start instant and is not corrected for the
        /// fractional-delay or low-pass filter group delay.
        pub sample_time_ns: i64,
        /// Measured module-bus current scalar in `A`.
        pub module_bus_current_a: f32,
        /// Measured module-bus voltage scalar in `V`.
        pub module_bus_voltage_v: f32,
        /// Filtered absolute cold-junction/board temperature scalar in `K`.
        pub board_temperature_k: f32,
        /// Measured loop currents in `A` with shape `(CURRENT_4_20_CHANNEL_COUNT,)`.
        pub current_4_20_a: [f32; super::CURRENT_4_20_CHANNEL_COUNT],
        /// Measured external RTD resistances in `ohm` with shape `(RTD_CHANNEL_COUNT,)`.
        pub rtd_resistance_ohm: [f32; super::RTD_CHANNEL_COUNT],
        /// Cold-junction-compensated absolute thermocouple temperatures in `K`
        /// with shape `(THERMOCOUPLE_CHANNEL_COUNT,)`.
        pub thermocouple_temperature_k: [f32; super::THERMOCOUPLE_CHANNEL_COUNT],
        /// Measured voltage-channel values in `V` with shape `(VOLTAGE_CHANNEL_COUNT,)`.
        pub voltage_v: [f32; super::VOLTAGE_CHANNEL_COUNT],
        /// Unwrapped quadrature-encoder count.
        pub encoder: i64,
        /// Unwrapped pulse count.
        pub pulse_counter: i64,
        /// Measured input frequencies in `Hz` with shape `(FREQUENCY_CHANNEL_COUNT,)`.
        pub frequency_meas: [f32; super::FREQUENCY_CHANNEL_COUNT],

        /// GPIO input-state bit field; only bits `0..=1` are used.
        pub gpio: u8,
    }

    impl Default for OperatingSnapshot {
        fn default() -> Self {
            Self {
                magic: super::OPERATING_SNAPSHOT_MAGIC,
                metrics: OperatingMetrics::default(),
                sample_time_ns: 0,
                module_bus_current_a: 0.0,
                module_bus_voltage_v: 0.0,
                board_temperature_k: 0.0,
                current_4_20_a: [0.0; super::CURRENT_4_20_CHANNEL_COUNT],
                rtd_resistance_ohm: [0.0; super::RTD_CHANNEL_COUNT],
                thermocouple_temperature_k: [0.0; super::THERMOCOUPLE_CHANNEL_COUNT],
                voltage_v: [0.0; super::VOLTAGE_CHANNEL_COUNT],
                encoder: 0,
                pulse_counter: 0,
                frequency_meas: [0.0; super::FREQUENCY_CHANNEL_COUNT],
                gpio: 0,
            }
        }
    }

    impl OperatingSnapshot {
        /// Checks the packet marker and all finite-value and GPIO invariants.
        ///
        /// Returns:
        ///   `true` when the snapshot is safe for the software calc graph.
        pub fn is_valid(&self) -> bool {
            self.magic == super::OPERATING_SNAPSHOT_MAGIC
                && self.module_bus_current_a.is_finite()
                && self.module_bus_voltage_v.is_finite()
                && self.board_temperature_k.is_finite()
                && self.current_4_20_a.iter().all(|value| value.is_finite())
                && self
                    .rtd_resistance_ohm
                    .iter()
                    .all(|value| value.is_finite())
                && self
                    .thermocouple_temperature_k
                    .iter()
                    .all(|value| value.is_finite())
                && self.voltage_v.iter().all(|value| value.is_finite())
                && self.frequency_meas.iter().all(|value| value.is_finite())
                && self.gpio & !0x03 == 0
        }
    }

    /// Backward-compatible name for the Deimos roundtrip snapshot packet.
    pub type OperatingRoundtripOutput = OperatingSnapshot;
}

#[cfg(test)]
mod packet_tests {
    use super::*;
    use crate::{
        peripherals::PeripheralId,
        states::{AcknowledgeConfiguration, ByteStruct, ByteStructLen, ConfiguringInput},
    };

    fn round_trip<T>(value: T) -> T
    where
        T: ByteStruct + ByteStructLen,
    {
        let mut bytes = [0_u8; 256];
        value.write_bytes(&mut bytes[..T::BYTE_LEN]);
        T::read_bytes(&bytes[..T::BYTE_LEN])
    }

    #[test]
    fn sampling_policy_derives_samplerate_and_iir_cutoff_from_cycle_rate() {
        let low_rate = adc_sampling_policy(4.0).unwrap();
        assert_eq!(low_rate.mode, AdcSamplingMode::Oversampled);
        assert_eq!(low_rate.samples_per_cycle, 2_250);
        assert_eq!(low_rate.sample_rate_hz, 9_000.0);
        assert_eq!(low_rate.iir_cutoff_hz, Some(4.0));
        assert_eq!(low_rate.iir_cutoff_ratio, Some(1.0 / 2_250.0));

        let rounded_rate = adc_sampling_policy(2_500.0).unwrap();
        assert_eq!(rounded_rate.mode, AdcSamplingMode::Oversampled);
        assert_eq!(rounded_rate.samples_per_cycle, 4);
        assert_eq!(rounded_rate.sample_rate_hz, 10_000.0);
        assert_eq!(rounded_rate.iir_cutoff_hz, Some(2_500.0));

        let below_cutover = adc_sampling_policy(1.0e9 / 333_334.0).unwrap();
        assert_eq!(below_cutover.mode, AdcSamplingMode::Oversampled);
        assert_eq!(below_cutover.samples_per_cycle, 3);

        let cutover = adc_sampling_policy(3_000.0).unwrap();
        assert_eq!(cutover.mode, AdcSamplingMode::Direct);
        assert_eq!(cutover.samples_per_cycle, 1);
        assert_eq!(cutover.sample_rate_hz, 3_000.0);
        assert_eq!(cutover.iir_cutoff_hz, None);
        assert_eq!(cutover.iir_cutoff_ratio, None);

        assert!(adc_sampling_policy(0.0).is_none());
        assert!(adc_sampling_policy(f64::NAN).is_none());
        assert!(adc_sampling_policy(f64::MIN_POSITIVE).is_none());
    }

    #[test]
    fn packet_magics_are_direction_specific_and_validated() {
        let markers = [
            BINDING_INPUT_MAGIC,
            BINDING_OUTPUT_MAGIC,
            CONFIGURING_INPUT_MAGIC,
            CONFIGURING_OUTPUT_MAGIC,
            OPERATING_INPUT_MAGIC,
            OPERATING_SNAPSHOT_MAGIC,
        ];
        for (index, marker) in markers.iter().enumerate() {
            assert!(!markers[..index].contains(marker));
        }

        let mut binding_input = Rev7BindingInput::new(1_000);
        assert!(round_trip(binding_input).is_valid());
        binding_input.magic ^= 1;
        assert!(!round_trip(binding_input).is_valid());

        let mut binding_output = Rev7BindingOutput::new(PeripheralId {
            model_number: MODEL_NUMBER,
            serial_number: 3,
        });
        assert!(round_trip(binding_output).is_valid());
        binding_output.peripheral_id.model_number ^= 1;
        assert!(!round_trip(binding_output).is_valid());

        let mut configuring_input = Rev7ConfiguringInput::from_base(ConfiguringInput::default());
        configuring_input.dt_ns = 1;
        assert!(round_trip(configuring_input).is_valid());
        configuring_input.magic ^= 1;
        assert!(!round_trip(configuring_input).is_valid());

        let mut configuring_output =
            Rev7ConfiguringOutput::new(AcknowledgeConfiguration::Ack, false);
        assert!(round_trip(configuring_output).is_valid());
        configuring_output.firmware_calibrated = 2;
        assert!(!round_trip(configuring_output).is_valid());

        let mut operating_input = OperatingRoundtripInput::default();
        assert!(round_trip(operating_input).is_valid());
        operating_input.outputs.pwm_duty_frac[0] = f32::NAN;
        assert!(!round_trip(operating_input).is_valid());

        let mut snapshot = OperatingSnapshot {
            sample_time_ns: 0x0012_3456_789a_bcde,
            ..OperatingSnapshot::default()
        };
        assert!(round_trip(snapshot).is_valid());
        assert_eq!(round_trip(snapshot).sample_time_ns, snapshot.sample_time_ns);
        assert_eq!(OperatingSnapshot::BYTE_LEN, 157);
        snapshot.magic ^= 1;
        assert!(!round_trip(snapshot).is_valid());
    }

    #[test]
    fn calibration_binary_round_trips_without_protocol_magic() {
        let mut calibration = Rev7Calibration::default();
        calibration.firmware_calibrated = 1;
        calibration.voltage_cals[4] = LinearCalibration {
            slope: 1.25,
            offset: -0.125,
        };

        let decoded = round_trip(calibration);
        assert!(decoded.is_valid());
        assert!(decoded.is_calibrated());
        assert_eq!(decoded.voltage_cals[4].slope, 1.25);
        assert_eq!(decoded.voltage_cals[4].offset, -0.125);
        assert_eq!(Rev7Calibration::BYTE_LEN, 1 + ADC_CHANNEL_COUNT * 8);
    }

    #[test]
    fn operating_output_settings_round_trip_as_one_preserved_value() {
        let settings = OperatingOutputSettings {
            pwm_duty_frac: [0.1, 0.2, 0.3, 0.4],
            pwm_freq_hz: [1_000, 2_000, 3_000, 4_000],
            dac_v: [0.5, 2.0],
            gpio: 0b1010,
        };
        assert!(settings.is_valid());

        let retained_for_reentry = settings;
        assert_eq!(retained_for_reentry, settings);

        let initial_config = ModbusInitialConfig {
            outputs: retained_for_reentry,
            ..ModbusInitialConfig::default()
        };
        assert_eq!(initial_config.dt_ns, MODBUS_DEFAULT_DT_NS);
        assert_eq!(
            initial_config.loss_of_contact_limit,
            MODBUS_DEFAULT_LOSS_OF_CONTACT_LIMIT
        );
        let reentry_config = initial_config.reenter_at_period(200_000);
        assert_eq!(reentry_config.dt_ns, 200_000);
        assert_eq!(
            reentry_config.loss_of_contact_limit,
            initial_config.loss_of_contact_limit
        );
        assert_eq!(reentry_config.outputs, settings);

        let packet = OperatingRoundtripInput {
            outputs: reentry_config.outputs,
            ..OperatingRoundtripInput::default()
        };
        let mut encoded = [0_u8; 69];
        packet.write_bytes(&mut encoded);
        assert_eq!(&encoded[28..32], &0.1_f32.to_le_bytes());
        assert_eq!(&encoded[44..48], &1_000_u32.to_le_bytes());
        assert_eq!(&encoded[60..64], &0.5_f32.to_le_bytes());
        assert_eq!(encoded[68], 0b1010);

        let decoded = round_trip(packet);
        assert_eq!(decoded.outputs, settings);
        assert_eq!(OperatingOutputSettings::BYTE_LEN, 41);
        assert_eq!(OperatingRoundtripInput::BYTE_LEN, 69);
    }

    #[test]
    fn engineering_conversion_preserves_channel_order_and_calibration_placement() {
        use crate::calcs::{ktype_voltage_v_f32, pt100_resistance_ohm_f32};

        let cold_junction_k = 300.0_f32;
        let hot_junction_k = 500.0_f32;
        let mut samples = [0.0_f32; ADC_CHANNEL_COUNT];
        samples[0] = 0.3;
        samples[1] = 1.5;
        samples[2] =
            pt100_resistance_ohm_f32(cold_junction_k) * RTD_REFERENCE_CURRENT_A * RTD_FRONTEND_GAIN;
        samples[3] = 0.75;
        samples[7] = 100.0 * RTD_REFERENCE_CURRENT_A * RTD_FRONTEND_GAIN;
        samples[10] = TC_FRONTEND_OFFSET_V
            + TC_FRONTEND_GAIN
                * (ktype_voltage_v_f32(hot_junction_k) - ktype_voltage_v_f32(cold_junction_k));
        samples[12] = 1.25;
        samples[13] = 2.0;
        samples[14] = 2.0;
        samples[15] = 1.5;
        samples[16] = TC_FRONTEND_OFFSET_V + TC_FRONTEND_GAIN * 0.01;
        samples[17] = TC_FRONTEND_OFFSET_V - TC_FRONTEND_GAIN * 0.005;

        let mut calibration = Rev7Calibration::default();
        calibration.voltage_cals[3] = LinearCalibration {
            slope: 2.0,
            offset: 0.15,
        };
        calibration.voltage_cals[7] = LinearCalibration {
            slope: 2.0,
            offset: 0.001,
        };
        calibration.voltage_cals[14] = LinearCalibration {
            slope: 2.0,
            offset: 1.0,
        };
        calibration.voltage_cals[16] = LinearCalibration {
            slope: 2.0,
            offset: 0.001,
        };

        let calculated_board_k = board_temperature_k_f32(&samples, &calibration);
        assert!((calculated_board_k - cold_junction_k).abs() <= 0.01);

        let mut snapshot = OperatingSnapshot::default();
        populate_analog_snapshot_f32(&mut snapshot, &samples, &calibration, cold_junction_k);
        assert!((snapshot.module_bus_current_a - 1.0).abs() < 1.0e-6);
        assert!((snapshot.module_bus_voltage_v - 21.5).abs() < 1.0e-6);
        assert_eq!(snapshot.board_temperature_k, cold_junction_k);
        assert!((snapshot.current_4_20_a[0] - 0.022).abs() < 1.0e-7);
        assert!((snapshot.rtd_resistance_ohm[0] - 204.0).abs() < 1.0e-4);
        assert!((snapshot.thermocouple_temperature_k[0] - hot_junction_k).abs() < 0.02);
        assert_eq!(snapshot.voltage_v[0], 1.25);
        assert_eq!(snapshot.voltage_v[1], 2.0);
        assert_eq!(snapshot.voltage_v[2], 25.0);
        assert_eq!(snapshot.voltage_v[3], 9.0);
        assert!((snapshot.voltage_v[4] - 0.021).abs() < 1.0e-6);
        assert!((snapshot.voltage_v[5] + 0.005).abs() < 1.0e-6);
    }
}

#[cfg(feature = "alloc")]
pub mod filters {
    use core::fmt;

    use super::{
        adc_sampling_policy, AdcSamplingPolicy, ADC_ANALOG_FRONTEND_FILTER_KINDS,
        ADC_CHANNEL_COUNT, ADC_CLOCK_HZ, ADC_CONVERSION_CYCLES, ADC_FILTER_COUNT,
        ADC_FILTER_MAX_CUTOFF_RATIO, ADC_FILTER_ORDER, ADC_FILTER_SECTIONS,
        ADC_FRACTIONAL_DELAY_FILTER_TAPS, ADC_SAMPLE_HOLD_CYCLES,
    };
    use deimos_numerics::{
        control::lti::{
            butter, design_digital_filter_tf, sallen_key_lowpass_transfer_function, BodeData,
            ContinuousTransferFunction, DiscreteTransferFunction, FilterDesignError,
            Fir as DynamicFir, LtiError,
        },
        control::DiscretizationMethod,
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

    /// Transfer function for one complete rev7 digital ADC filter path.
    pub type AdcDigitalTransferFunction = DiscreteTransferFunction<f64>;

    /// Transfer functions for all complete rev7 digital ADC filter paths.
    pub type AdcDigitalTransferFunctionBank = [AdcDigitalTransferFunction; ADC_FILTER_COUNT];

    /// Continuous-time transfer function for one rev7 ADC analog front end.
    pub type AdcAnalogFrontendTransferFunction = ContinuousTransferFunction<f64>;

    /// Continuous-time transfer functions for all rev7 ADC analog front ends.
    pub type AdcAnalogFrontendTransferFunctionBank =
        [AdcAnalogFrontendTransferFunction; ADC_CHANNEL_COUNT];

    /// Sampled transfer function for one full rev7 ADC measurement filter chain.
    pub type AdcSampledTransferFunction = DiscreteTransferFunction<f64>;

    /// Sampled transfer functions for all rev7 ADC measurement filter chains.
    pub type AdcSampledTransferFunctionBank = [AdcSampledTransferFunction; ADC_CHANNEL_COUNT];

    /// Bode data for one full rev7 ADC measurement filter chain.
    pub type AdcSampledBodeData = BodeData<f64>;

    /// Bode data for all rev7 ADC measurement filter chains.
    pub type AdcSampledBodeDataBank = [AdcSampledBodeData; ADC_CHANNEL_COUNT];

    const SALLEN_KEY_CAPACITANCE_F: f64 = 10.0e-9;
    const SALLEN_KEY_100HZ_RESISTANCE_OHMS: f64 = 100.0e3;
    const SALLEN_KEY_1KHZ_RESISTANCE_OHMS: f64 = 10.0e3;
    const SALLEN_KEY_3KHZ_RESISTANCE_OHMS: f64 = 3.3e3;
    const ADC_INPUT_RC_RESISTANCE_OHMS: f64 = 10.0;
    const ADC_INPUT_RC_CAPACITANCE_F: f64 = 1.0e-6;

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

    /// Build the rev7 digital ADC paths implied by a reporting cycle rate.
    ///
    /// Oversampled paths contain the fractional-delay FIR followed by the ADC
    /// IIR. Direct paths contain only the fractional-delay FIR, matching the
    /// firmware hot path selected by [`super::adc_sampling_policy`].
    ///
    /// Args:
    ///   cycle_rate_hz: Reporting cycle rate scalar in `cycle/s`.
    ///
    /// Returns:
    ///   Digital transfer-function bank with shape `(ADC_FILTER_COUNT,)`.
    pub fn adc_digital_transfer_functions_for_cycle_rate(
        cycle_rate_hz: f64,
    ) -> Result<AdcDigitalTransferFunctionBank, AdcFilterBuildError> {
        let policy = validated_sampling_policy(cycle_rate_hz)?;
        adc_digital_transfer_functions(policy.iir_cutoff_ratio, policy.sample_rate_hz)
    }

    /// Builds continuous-time transfer functions for the rev7 analog voltage front ends.
    ///
    /// The modeled filtered channels are a unity-gain active Sallen-Key low-pass
    /// followed by the ADC input RC filter. Board current and board voltage are
    /// modeled as unity transfer functions.
    pub fn adc_analog_frontend_transfer_functions(
    ) -> Result<AdcAnalogFrontendTransferFunctionBank, AdcFilterBuildError> {
        let unfiltered = unfiltered_transfer_function()?;
        let sallen_key_100hz =
            sallen_key_with_adc_rc_transfer_function(SALLEN_KEY_100HZ_RESISTANCE_OHMS)?;
        let sallen_key_1khz =
            sallen_key_with_adc_rc_transfer_function(SALLEN_KEY_1KHZ_RESISTANCE_OHMS)?;
        let sallen_key_3khz =
            sallen_key_with_adc_rc_transfer_function(SALLEN_KEY_3KHZ_RESISTANCE_OHMS)?;

        Ok(core::array::from_fn(|idx| {
            match ADC_ANALOG_FRONTEND_FILTER_KINDS[idx] {
                super::AdcAnalogFrontendFilterKind::Unfiltered => &unfiltered,
                super::AdcAnalogFrontendFilterKind::SallenKey100Hz => &sallen_key_100hz,
                super::AdcAnalogFrontendFilterKind::SallenKey1kHz => &sallen_key_1khz,
                super::AdcAnalogFrontendFilterKind::SallenKey3kHz => &sallen_key_3khz,
            }
            .clone()
        }))
    }

    /// Builds sampled-sequence transfer functions for the rev7 ADC measurement filter chain.
    ///
    /// Each returned transfer function models the channel's analog front end
    /// sampled with a bilinear transform at `sample_rate_hz`, followed by the
    /// channel's fractional-delay FIR and the firmware ADC Butterworth
    /// low-pass filter. The returned bank is ordered like reported ADC
    /// voltages: `ain0..ain12` followed by `ain15..ain19`.
    ///
    /// These transfer functions are useful for baseband sampled-sequence
    /// analysis. For physical input-frequency Bode data, use
    /// [`adc_sampled_bode_data`], which keeps the analog frontend response in
    /// continuous time so high-frequency analog attenuation is not aliased.
    pub fn adc_sampled_transfer_functions(
        cutoff_ratio: f64,
        sample_rate_hz: f64,
    ) -> Result<AdcSampledTransferFunctionBank, AdcFilterBuildError> {
        adc_sampled_transfer_functions_with_iir(Some(cutoff_ratio), sample_rate_hz)
    }

    /// Build sampled-sequence transfer functions implied by a reporting rate.
    ///
    /// Args:
    ///   cycle_rate_hz: Reporting cycle rate scalar in `cycle/s`.
    ///
    /// Returns:
    ///   Full sampled transfer-function bank with shape `(ADC_CHANNEL_COUNT,)`.
    pub fn adc_sampled_transfer_functions_for_cycle_rate(
        cycle_rate_hz: f64,
    ) -> Result<AdcSampledTransferFunctionBank, AdcFilterBuildError> {
        let policy = validated_sampling_policy(cycle_rate_hz)?;
        adc_sampled_transfer_functions_with_iir(policy.iir_cutoff_ratio, policy.sample_rate_hz)
    }

    fn adc_sampled_transfer_functions_with_iir(
        iir_cutoff_ratio: Option<f64>,
        sample_rate_hz: f64,
    ) -> Result<AdcSampledTransferFunctionBank, AdcFilterBuildError> {
        let sample_time = validate_sample_rate_hz(sample_rate_hz)?;
        let analog_transfer_functions = adc_analog_frontend_transfer_functions()?;
        let digital_transfer_functions =
            adc_digital_transfer_functions(iir_cutoff_ratio, sample_rate_hz)?;

        let mut output: [Option<AdcSampledTransferFunction>; ADC_CHANNEL_COUNT] =
            core::array::from_fn(|_| None);
        for idx in 0..ADC_CHANNEL_COUNT {
            let sampled_analog = analog_transfer_functions[idx]
                .to_state_space()?
                .discretize(
                    sample_time,
                    DiscretizationMethod::Bilinear {
                        prewarp_frequency: None,
                    },
                )
                .map_err(LtiError::from)?
                .to_transfer_function()?;

            output[idx] = Some(sampled_analog.mul(&digital_transfer_functions[idx])?);
        }

        Ok(output.map(|transfer_function| transfer_function.unwrap()))
    }

    /// Builds physical-input-frequency Bode data for the full rev7 ADC measurement filter chain.
    ///
    /// For each requested physical input frequency, this evaluates the analog
    /// frontend as a continuous-time response and the fractional-delay plus
    /// digital ADC filter as a discrete-time response. The returned magnitude
    /// is therefore `|H_analog(jw)| * |H_digital(exp(jwT))|`, so analog
    /// attenuation at high input frequencies is preserved even when the sampled
    /// digital response aliases.
    pub fn adc_sampled_bode_data(
        cutoff_ratio: f64,
        sample_rate_hz: f64,
        frequencies_hz: &[f64],
    ) -> Result<AdcSampledBodeDataBank, AdcFilterBuildError> {
        adc_sampled_bode_data_with_iir(Some(cutoff_ratio), sample_rate_hz, frequencies_hz)
    }

    /// Build physical-input-frequency Bode data implied by a reporting rate.
    ///
    /// Args:
    ///   cycle_rate_hz: Reporting cycle rate scalar in `cycle/s`.
    ///   frequencies_hz: Physical input-frequency grid in `Hz` with shape `(n,)`.
    ///
    /// Returns:
    ///   Full measurement-path Bode bank with shape `(ADC_CHANNEL_COUNT,)`.
    pub fn adc_sampled_bode_data_for_cycle_rate(
        cycle_rate_hz: f64,
        frequencies_hz: &[f64],
    ) -> Result<AdcSampledBodeDataBank, AdcFilterBuildError> {
        let policy = validated_sampling_policy(cycle_rate_hz)?;
        adc_sampled_bode_data_with_iir(
            policy.iir_cutoff_ratio,
            policy.sample_rate_hz,
            frequencies_hz,
        )
    }

    fn adc_sampled_bode_data_with_iir(
        iir_cutoff_ratio: Option<f64>,
        sample_rate_hz: f64,
        frequencies_hz: &[f64],
    ) -> Result<AdcSampledBodeDataBank, AdcFilterBuildError> {
        validate_sample_rate_hz(sample_rate_hz)?;
        let analog_transfer_functions = adc_analog_frontend_transfer_functions()?;
        let digital_transfer_functions =
            adc_digital_transfer_functions(iir_cutoff_ratio, sample_rate_hz)?;
        let angular_frequencies: alloc::vec::Vec<f64> = frequencies_hz
            .iter()
            .map(|frequency_hz| frequency_hz * core::f64::consts::TAU)
            .collect();
        let mut output: [Option<AdcSampledBodeData>; ADC_CHANNEL_COUNT] =
            core::array::from_fn(|_| None);
        for idx in 0..ADC_CHANNEL_COUNT {
            let analog_bode = analog_transfer_functions[idx].bode_data(&angular_frequencies)?;
            let digital_bode = digital_transfer_functions[idx].bode_data(&angular_frequencies)?;
            output[idx] = Some(combine_bode_data(&analog_bode, &digital_bode)?);
        }

        Ok(output.map(|bode_data| bode_data.unwrap()))
    }

    fn adc_digital_transfer_functions(
        iir_cutoff_ratio: Option<f64>,
        sample_rate_hz: f64,
    ) -> Result<AdcDigitalTransferFunctionBank, AdcFilterBuildError> {
        let fractional_delay_transfer_functions =
            adc_fractional_delay_transfer_functions(sample_rate_hz)?;
        let adc_filter_transfer_function = iir_cutoff_ratio
            .map(|cutoff_ratio| {
                adc_filter_transfer_function_at_sample_rate(cutoff_ratio, sample_rate_hz)
            })
            .transpose()?;

        let mut output: [Option<AdcDigitalTransferFunction>; ADC_CHANNEL_COUNT] =
            core::array::from_fn(|_| None);
        for (idx, fractional_delay) in fractional_delay_transfer_functions.into_iter().enumerate() {
            output[idx] = Some(match &adc_filter_transfer_function {
                Some(adc_filter) => fractional_delay.mul(adc_filter)?,
                None => fractional_delay,
            });
        }
        Ok(output.map(|transfer_function| transfer_function.unwrap()))
    }

    fn validated_sampling_policy(
        cycle_rate_hz: f64,
    ) -> Result<AdcSamplingPolicy, AdcFilterBuildError> {
        adc_sampling_policy(cycle_rate_hz).ok_or_else(|| {
            AdcFilterBuildError::from(EmbeddedError::InvalidParameter {
                which: "adc.cycle_rate_hz",
            })
        })
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

    fn adc_filter_transfer_function_at_sample_rate(
        cutoff_ratio: f64,
        sample_rate_hz: f64,
    ) -> Result<AdcFilterTransferFunction, AdcFilterBuildError> {
        validate_sample_rate_hz(sample_rate_hz)?;
        let cutoff_ratio = clamp_adc_filter_cutoff_ratio(cutoff_ratio);
        Ok(design_digital_filter_tf(
            &deimos_numerics::control::lti::DigitalFilterSpec::new(
                ADC_FILTER_ORDER,
                deimos_numerics::control::lti::DigitalFilterFamily::Butterworth,
                deimos_numerics::control::lti::FilterShape::Lowpass {
                    cutoff: cutoff_ratio * sample_rate_hz * core::f64::consts::TAU,
                },
                sample_rate_hz,
            )?,
        )?)
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

    fn validate_sample_rate_hz(sample_rate_hz: f64) -> Result<f64, AdcFilterBuildError> {
        if !sample_rate_hz.is_finite() || sample_rate_hz <= 0.0 {
            return Err(EmbeddedError::InvalidParameter {
                which: "adc.sample_rate_hz",
            }
            .into());
        }
        Ok(1.0 / sample_rate_hz)
    }

    fn unfiltered_transfer_function() -> Result<AdcAnalogFrontendTransferFunction, LtiError> {
        ContinuousTransferFunction::continuous([1.0], [1.0])
    }

    fn sallen_key_with_adc_rc_transfer_function(
        resistance_ohms: f64,
    ) -> Result<AdcAnalogFrontendTransferFunction, LtiError> {
        sallen_key_lowpass_transfer_function(
            resistance_ohms,
            resistance_ohms,
            SALLEN_KEY_CAPACITANCE_F,
            SALLEN_KEY_CAPACITANCE_F,
        )?
        .mul(&rc_lowpass_transfer_function(
            ADC_INPUT_RC_RESISTANCE_OHMS,
            ADC_INPUT_RC_CAPACITANCE_F,
        )?)
    }

    fn rc_lowpass_transfer_function(
        resistance_ohms: f64,
        capacitance_f: f64,
    ) -> Result<AdcAnalogFrontendTransferFunction, LtiError> {
        ContinuousTransferFunction::continuous([1.0], [resistance_ohms * capacitance_f, 1.0])
    }

    fn combine_bode_data(
        lhs: &BodeData<f64>,
        rhs: &BodeData<f64>,
    ) -> Result<BodeData<f64>, AdcFilterBuildError> {
        if lhs.angular_frequencies != rhs.angular_frequencies {
            return Err(LtiError::InvalidSampleGrid {
                which: "combine_bode_data",
            }
            .into());
        }
        Ok(BodeData {
            angular_frequencies: lhs.angular_frequencies.clone(),
            magnitude_db: lhs
                .magnitude_db
                .iter()
                .zip(rhs.magnitude_db.iter())
                .map(|(lhs, rhs)| lhs + rhs)
                .collect(),
            phase_deg: lhs
                .phase_deg
                .iter()
                .zip(rhs.phase_deg.iter())
                .map(|(lhs, rhs)| lhs + rhs)
                .collect(),
        })
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn rev7_adc_filter_helpers_build_full_banks() {
            let filters = adc_filter_bank(0.1).unwrap();
            let transfer_functions = adc_filter_transfer_functions(0.1).unwrap();
            let fractional_delay_filters =
                adc_fractional_delay_filter_bank(super::super::ADC_OVERSAMPLE_TARGET_RATE_HZ)
                    .unwrap();
            let fractional_delay_transfer_functions = adc_fractional_delay_transfer_functions(
                super::super::ADC_OVERSAMPLE_TARGET_RATE_HZ,
            )
            .unwrap();

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
                1.0 / super::super::ADC_OVERSAMPLE_TARGET_RATE_HZ
            );
            assert!(!fractional_delay_transfer_functions[0]
                .numerator()
                .is_empty());
        }

        #[test]
        fn rev7_low_rate_adc_filter_holds_a_primed_steady_state() {
            let filter = adc_filter_bank(1.0 / 2_250.0).unwrap()[0];
            let mut state = filter.reset_state();
            filter.set_steady_state(&mut state, [1.25]);

            for _ in 0..16 {
                let output = filter.step(&mut state, [1.25])[0];
                assert!(output.is_finite());
                assert!((output - 1.25).abs() < 1.0e-5);
            }
        }

        #[test]
        fn rev7_adc_analog_frontend_transfer_functions_match_channel_mapping() {
            let transfer_functions = adc_analog_frontend_transfer_functions().unwrap();

            assert_eq!(transfer_functions.len(), ADC_CHANNEL_COUNT);
            assert_eq!(
                ADC_ANALOG_FRONTEND_FILTER_KINDS[0],
                super::super::AdcAnalogFrontendFilterKind::Unfiltered
            );
            assert_eq!(
                ADC_ANALOG_FRONTEND_FILTER_KINDS[1],
                super::super::AdcAnalogFrontendFilterKind::Unfiltered
            );
            assert_eq!(
                ADC_ANALOG_FRONTEND_FILTER_KINDS[2],
                super::super::AdcAnalogFrontendFilterKind::SallenKey100Hz
            );
            assert_eq!(
                ADC_ANALOG_FRONTEND_FILTER_KINDS[10],
                super::super::AdcAnalogFrontendFilterKind::SallenKey1kHz
            );
            assert_eq!(
                ADC_ANALOG_FRONTEND_FILTER_KINDS[11],
                super::super::AdcAnalogFrontendFilterKind::SallenKey1kHz
            );
            assert_eq!(
                ADC_ANALOG_FRONTEND_FILTER_KINDS[16],
                super::super::AdcAnalogFrontendFilterKind::SallenKey1kHz
            );
            assert_eq!(
                ADC_ANALOG_FRONTEND_FILTER_KINDS[17],
                super::super::AdcAnalogFrontendFilterKind::SallenKey1kHz
            );

            assert_eq!(transfer_functions[0].numerator(), &[1.0]);
            assert_eq!(transfer_functions[0].denominator(), &[1.0]);
            assert_eq!(transfer_functions[2].denominator().len(), 4);
            assert_eq!(transfer_functions[3].denominator().len(), 4);
            assert_eq!(transfer_functions[10].denominator().len(), 4);

            for transfer_function in transfer_functions {
                let dc_gain = transfer_function.dc_gain().unwrap();
                assert!((dc_gain.re - 1.0).abs() < 1.0e-9);
                assert!(dc_gain.im.abs() < 1.0e-12);
            }
        }

        #[test]
        fn rev7_adc_sampled_transfer_functions_include_full_filter_chain() {
            let transfer_functions =
                adc_sampled_transfer_functions(0.1, super::super::ADC_OVERSAMPLE_TARGET_RATE_HZ)
                    .unwrap();

            assert_eq!(transfer_functions.len(), ADC_CHANNEL_COUNT);
            for transfer_function in transfer_functions {
                assert_eq!(
                    transfer_function.sample_time(),
                    1.0 / super::super::ADC_OVERSAMPLE_TARGET_RATE_HZ
                );
                let dc_gain = transfer_function.dc_gain().unwrap();
                assert!((dc_gain.re - 1.0).abs() < 1.0e-4);
                assert!(dc_gain.im.abs() < 1.0e-10);
                assert!(!transfer_function.numerator().is_empty());
                assert!(!transfer_function.denominator().is_empty());
            }
        }

        #[test]
        fn cycle_rate_filter_helpers_follow_shared_sampling_policy() {
            let oversampled = adc_sampled_transfer_functions_for_cycle_rate(1_000.0).unwrap();
            for transfer_function in oversampled {
                assert_eq!(transfer_function.sample_time(), 1.0 / 9_000.0);
            }

            let direct = adc_digital_transfer_functions_for_cycle_rate(5_000.0).unwrap();
            let fractional = adc_fractional_delay_transfer_functions(5_000.0).unwrap();
            for (direct, fractional) in direct.iter().zip(fractional.iter()) {
                assert_eq!(direct.sample_time(), 1.0 / 5_000.0);
                assert_eq!(direct.numerator(), fractional.numerator());
                assert_eq!(direct.denominator(), fractional.denominator());
            }

            assert!(adc_sampled_transfer_functions_for_cycle_rate(0.0).is_err());
        }

        #[test]
        fn rev7_adc_sampled_bode_data_builds_for_all_channels() {
            let frequencies_hz = [0.0, 10.0, 100.0, 1_000.0];
            let angular_frequencies: alloc::vec::Vec<f64> = frequencies_hz
                .iter()
                .map(|frequency_hz| frequency_hz * core::f64::consts::TAU)
                .collect();
            let bode_data = adc_sampled_bode_data(
                0.1,
                super::super::ADC_OVERSAMPLE_TARGET_RATE_HZ,
                &frequencies_hz,
            )
            .unwrap();

            assert_eq!(bode_data.len(), ADC_CHANNEL_COUNT);
            for channel_bode in bode_data {
                assert_eq!(channel_bode.angular_frequencies, angular_frequencies);
                assert_eq!(channel_bode.magnitude_db.len(), angular_frequencies.len());
                assert_eq!(channel_bode.phase_deg.len(), angular_frequencies.len());
                assert!(channel_bode
                    .magnitude_db
                    .iter()
                    .all(|value| value.is_finite()));
                assert!(channel_bode.phase_deg.iter().all(|value| value.is_finite()));
            }
        }

        #[test]
        fn rev7_adc_combined_bode_preserves_high_frequency_analog_attenuation() {
            let sample_rate_hz = super::super::ADC_OVERSAMPLE_TARGET_RATE_HZ;
            let frequencies_hz = [1_000.0, 10_000.0, 16_000.0, 30_000.0, 100_000.0];
            let angular_frequencies: alloc::vec::Vec<f64> = frequencies_hz
                .iter()
                .map(|frequency_hz| frequency_hz * core::f64::consts::TAU)
                .collect();

            let analog_transfer_functions = adc_analog_frontend_transfer_functions().unwrap();
            let analog_bode = analog_transfer_functions[10]
                .bode_data(&angular_frequencies)
                .unwrap();
            let combined_bode =
                adc_sampled_bode_data(0.1, sample_rate_hz, &frequencies_hz).unwrap()[10].clone();

            for ((&frequency_hz, &combined_magnitude_db), &analog_magnitude_db) in frequencies_hz
                .iter()
                .zip(combined_bode.magnitude_db.iter())
                .zip(analog_bode.magnitude_db.iter())
            {
                assert!(
                    combined_magnitude_db <= analog_magnitude_db + 1.0e-9,
                    "combined magnitude at {frequency_hz} Hz is {combined_magnitude_db} dB, analog magnitude is {analog_magnitude_db} dB",
                );
            }
        }
    }
}

#[cfg(feature = "alloc")]
pub use filters::{
    adc_analog_frontend_transfer_functions, adc_digital_transfer_functions_for_cycle_rate,
    adc_filter_bank, adc_filter_transfer_functions, adc_fractional_delay_filter_bank,
    adc_fractional_delay_transfer_functions, adc_sampled_bode_data,
    adc_sampled_bode_data_for_cycle_rate, adc_sampled_transfer_functions,
    adc_sampled_transfer_functions_for_cycle_rate, AdcAnalogFrontendTransferFunction,
    AdcAnalogFrontendTransferFunctionBank, AdcDigitalTransferFunction,
    AdcDigitalTransferFunctionBank, AdcFilter, AdcFilterBank, AdcFilterBuildError, AdcFilterState,
    AdcFilterTransferFunction, AdcFilterTransferFunctionBank, AdcFractionalDelayFilter,
    AdcFractionalDelayFilterBank, AdcFractionalDelayFilterState,
    AdcFractionalDelayTransferFunction, AdcFractionalDelayTransferFunctionBank, AdcSampledBodeData,
    AdcSampledBodeDataBank, AdcSampledTransferFunction, AdcSampledTransferFunctionBank,
};
