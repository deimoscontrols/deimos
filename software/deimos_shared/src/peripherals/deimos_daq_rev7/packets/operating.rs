//! Operating packet and retained Modbus configuration definitions.

use core::default::Default;

use byte_struct::{ByteStruct, ByteStructLen, ByteStructUnspecifiedByteOrder};

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
    pub pwm_duty_frac: [f32; super::super::PWM_CHANNEL_COUNT],

    /// PWM frequencies in `Hz` with shape `(PWM_CHANNEL_COUNT,)`.
    ///
    /// PWM counters are buffered, so when using PWMs as GPIO by setting
    /// duty cycle to 0%/100%, the frequency should be high enough to
    /// produce the required response time.
    pub pwm_freq_hz: [u32; super::super::PWM_CHANNEL_COUNT],

    /// DAC output voltages in `V` with shape `(DAC_CHANNEL_COUNT,)` and
    /// range `[0, VREF]`.
    pub dac_v: [f32; super::super::DAC_CHANNEL_COUNT],

    /// GPIO output-state bit field; only bits `0..=3` are used.
    pub gpio: u8,
}

impl Default for OperatingOutputSettings {
    /// Returns the safe output state with nonzero PWM carrier frequencies.
    fn default() -> Self {
        Self {
            pwm_duty_frac: [0.0_f32; super::super::PWM_CHANNEL_COUNT],
            pwm_freq_hz: [1_000_000_u32; super::super::PWM_CHANNEL_COUNT],
            dac_v: [0.0_f32; super::super::DAC_CHANNEL_COUNT],
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
                .all(|value| value.is_finite() && (0.0..=super::super::VREF).contains(value))
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
    /// Persistent requested publishing-period correction in `ns`.
    pub period_delta_ns: i64,
    /// Requested one-cycle publishing-phase correction in `ns`.
    pub phase_delta_ns: i64,
    /// Output state to apply on operating entry.
    pub outputs: OperatingOutputSettings,
}

impl Default for ModbusInitialConfig {
    /// Returns the documented 10 Hz, one-minute, safe-output configuration.
    fn default() -> Self {
        Self {
            dt_ns: MODBUS_DEFAULT_DT_NS,
            loss_of_contact_limit: MODBUS_DEFAULT_LOSS_OF_CONTACT_LIMIT,
            period_delta_ns: 0,
            phase_delta_ns: 0,
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

    /// Return the bounded requested correction and consume its phase term.
    ///
    /// The period term remains active until another Modbus write replaces
    /// it. The phase term applies to one scheduled publishing interval and
    /// reads back as zero after this call.
    ///
    /// Returns:
    ///   Combined correction in `ns`, limited to `+/-10%` of `dt_ns`.
    pub fn take_timing_correction_ns(&mut self) -> i64 {
        let phase_delta_ns = self.phase_delta_ns;
        self.phase_delta_ns = 0;
        super::super::bounded_cycle_timing_correction_ns(
            self.dt_ns,
            self.period_delta_ns,
            phase_delta_ns,
        )
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
            magic: super::super::OPERATING_INPUT_MAGIC,
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
        self.magic == super::super::OPERATING_INPUT_MAGIC && self.outputs.is_valid()
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
    pub current_4_20_a: [f32; super::super::CURRENT_4_20_CHANNEL_COUNT],
    /// Measured external RTD resistances in `ohm` with shape `(RTD_CHANNEL_COUNT,)`.
    pub rtd_resistance_ohm: [f32; super::super::RTD_CHANNEL_COUNT],
    /// Cold-junction-compensated absolute thermocouple temperatures in `K`
    /// with shape `(THERMOCOUPLE_CHANNEL_COUNT,)`.
    pub thermocouple_temperature_k: [f32; super::super::THERMOCOUPLE_CHANNEL_COUNT],
    /// Measured voltage-channel values in `V` with shape `(VOLTAGE_CHANNEL_COUNT,)`.
    pub voltage_v: [f32; super::super::VOLTAGE_CHANNEL_COUNT],
    /// Unwrapped quadrature-encoder count.
    pub encoder: i64,
    /// Unwrapped pulse count.
    pub pulse_counter: i64,
    /// Measured input frequencies in `Hz` with shape `(FREQUENCY_CHANNEL_COUNT,)`.
    pub frequency_meas: [f32; super::super::FREQUENCY_CHANNEL_COUNT],

    /// GPIO input-state bit field; only bits `0..=1` are used.
    pub gpio: u8,
}

impl Default for OperatingSnapshot {
    fn default() -> Self {
        Self {
            magic: super::super::OPERATING_SNAPSHOT_MAGIC,
            metrics: OperatingMetrics::default(),
            sample_time_ns: 0,
            module_bus_current_a: 0.0,
            module_bus_voltage_v: 0.0,
            board_temperature_k: 0.0,
            current_4_20_a: [0.0; super::super::CURRENT_4_20_CHANNEL_COUNT],
            rtd_resistance_ohm: [0.0; super::super::RTD_CHANNEL_COUNT],
            thermocouple_temperature_k: [0.0; super::super::THERMOCOUPLE_CHANNEL_COUNT],
            voltage_v: [0.0; super::super::VOLTAGE_CHANNEL_COUNT],
            encoder: 0,
            pulse_counter: 0,
            frequency_meas: [0.0; super::super::FREQUENCY_CHANNEL_COUNT],
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
        self.magic == super::super::OPERATING_SNAPSHOT_MAGIC
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
