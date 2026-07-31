//! Shared protocol, sampling, calibration, and calculation definitions for the
//! Deimos DAQ rev7 peripheral.
//!
//! Feature logic is grouped into [packets], [filters], [uncertainty],
//! [modbus], [timing], and [calc]. The complete Modbus/TCP register map is
//! documented in [modbus].

pub mod calc;
pub mod filters;
pub mod modbus;
pub mod packets;
pub mod timing;
pub mod uncertainty;

pub use byte_struct::{ByteStruct, ByteStructLen, ByteStructUnspecifiedByteOrder};
pub use calc::{board_temperature_k_f32, populate_analog_snapshot_f32};
pub use filters::*;
pub use packets::*;
pub use timing::*;
pub use uncertainty::*;

/// Backward-compatible name for the rev7 timing module.
pub use timing as acquisition;

/// Backward-compatible namespace for Deimos operating packet definitions.
pub mod operating_roundtrip {
    pub use byte_struct::{ByteStruct, ByteStructLen, ByteStructUnspecifiedByteOrder};

    pub use super::packets::{
        ModbusInitialConfig, OperatingOutputSettings, OperatingRoundtripInput,
        OperatingRoundtripOutput, OperatingSnapshot,
    };
    pub use super::{MODBUS_DEFAULT_DT_NS, MODBUS_DEFAULT_LOSS_OF_CONTACT_LIMIT};
}

#[cfg(test)]
mod tests;

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

/// Slowest supported rev7 publishing rate in `cycle/s` in either protocol mode.
pub const REV7_MIN_CYCLE_RATE_HZ: u32 = 5;

/// Fastest supported Deimos UDP roundtrip rate in `cycle/s`.
///
/// Calibrated release-firmware timing measurements retain useful board margin
/// at this rate; rates near 8.9 kHz reach the measured execution-time limit.
pub const DEIMOS_MAX_CYCLE_RATE_HZ: u32 = 8_000;

/// Shortest supported Deimos UDP roundtrip period in `ns`.
pub const DEIMOS_MIN_CYCLE_PERIOD_NS: u32 = 1_000_000_000 / DEIMOS_MAX_CYCLE_RATE_HZ;

/// Longest supported Deimos UDP roundtrip period in `ns`.
pub const DEIMOS_MAX_CYCLE_PERIOD_NS: u32 = 1_000_000_000 / REV7_MIN_CYCLE_RATE_HZ;

/// Denominator of the maximum absolute per-cycle timing correction.
///
/// Both operating transports limit the combined requested period and phase
/// delta to 10% of the nominal publishing period. This retains execution-time
/// margin and keeps the synchronous counter-rate proofs below valid.
pub const MAX_CYCLE_TIMING_CORRECTION_DIVISOR: u32 = 10;

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

#[cfg(feature = "alloc")]
const SALLEN_KEY_CAPACITANCE_F: f64 = 10.0e-9;
#[cfg(feature = "alloc")]
const SALLEN_KEY_100HZ_RESISTANCE_OHMS: f64 = 100.0e3;
#[cfg(feature = "alloc")]
const SALLEN_KEY_1KHZ_RESISTANCE_OHMS: f64 = 10.0e3;
#[cfg(feature = "alloc")]
const SALLEN_KEY_3KHZ_RESISTANCE_OHMS: f64 = 3.3e3;
#[cfg(feature = "alloc")]
const ADC_INPUT_RC_RESISTANCE_OHMS: f64 = 10.0;
#[cfg(feature = "alloc")]
const ADC_INPUT_RC_CAPACITANCE_F: f64 = 1.0e-6;

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

/// Default Modbus publishing period, corresponding to 10 Hz.
pub const MODBUS_DEFAULT_DT_NS: u32 = 100_000_000;

/// Default one-minute Modbus contact timeout at 10 Hz.
pub const MODBUS_DEFAULT_LOSS_OF_CONTACT_LIMIT: u16 = 600;

const MODULE_BUS_CURRENT_SCALE: f32 = 1.0 / (0.006 * 50.0);
const MODULE_BUS_VOLTAGE_SCALE: f32 = 21.5 / 1.5;
const CURRENT_REFERENCE_RESISTOR_OHM: f32 = 75.0;
const RTD_REFERENCE_CURRENT_A: f32 = 250.0e-6;
const RTD_FRONTEND_GAIN: f32 = 25.7;
const TC_FRONTEND_GAIN: f32 = 25.7;
const TC_FRONTEND_OFFSET_V: f32 = 1.024;

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
