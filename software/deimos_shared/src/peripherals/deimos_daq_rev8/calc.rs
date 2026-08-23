//! Calibration records and engineering-unit conversions.

use byte_struct::{ByteStruct, ByteStructLen, ByteStructUnspecifiedByteOrder};

use super::{
    OperatingSnapshot, ADC_CHANNEL_COUNT, CURRENT_4_20_CHANNEL_COUNT,
    CURRENT_REFERENCE_RESISTOR_OHM, DAC_CHANNEL_COUNT, MODULE_BUS_CURRENT_SCALE,
    MODULE_BUS_VOLTAGE_SCALE, RTD_CHANNEL_COUNT, RTD_FRONTEND_GAIN, RTD_REFERENCE_CURRENT_A,
    TC_FRONTEND_GAIN, TC_FRONTEND_OFFSET_V, THERMOCOUPLE_CHANNEL_COUNT, VREF,
};

/// Maximum code accepted by each 12-bit DAC channel.
pub const DAC_MAX_CODE: u16 = 4095;

/// One affine calibration, `calibrated = slope * raw + offset`.
#[derive(ByteStruct, Clone, Copy, Debug)]
#[byte_struct_le]
pub struct LinearCalibration {
    /// Dimensionless scale factor.
    pub slope: f32,
    /// Offset in the calibrated value's units.
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
    /// Applies this affine calibration to one raw value.
    #[inline]
    pub fn apply(&self, value: f32) -> f32 {
        value * self.slope + self.offset
    }

    /// Solves this affine calibration for the uncalibrated input value.
    #[inline]
    pub fn unapply(&self, value: f32) -> f32 {
        (value - self.offset) / self.slope
    }

    /// Checks that the affine conversion is finite and invertible.
    ///
    /// Returns:
    ///   `true` when `slope` is finite and nonzero and `offset` is finite.
    pub fn is_valid(&self) -> bool {
        self.slope.is_finite() && self.slope != 0.0 && self.offset.is_finite()
    }
}

/// Complete input/output calibration image embedded in firmware.
///
/// `firmware_calibrated` is deliberately separate from protocol packet magic: it
/// records whether the coefficients were produced by the calibration procedure.
#[derive(ByteStruct, Clone, Copy, Debug)]
#[byte_struct_le]
pub struct Calibration {
    /// Status encoded as `0` for identity or `1` for procedurally calibrated.
    pub firmware_calibrated: u8,
    /// Sensed-voltage calibrations with shape `(ADC_CHANNEL_COUNT,)` and
    /// channel order `ain0..ain12, ain15..ain19`.
    pub voltage_cals: [LinearCalibration; super::ADC_CHANNEL_COUNT],
    /// DAC transfer calibrations, `actual_voltage = slope * nominal_voltage + offset`.
    pub dac_cals: [LinearCalibration; DAC_CHANNEL_COUNT],
}

impl Default for Calibration {
    fn default() -> Self {
        Self {
            firmware_calibrated: 0,
            voltage_cals: [LinearCalibration::default(); super::ADC_CHANNEL_COUNT],
            dac_cals: [LinearCalibration::default(); DAC_CHANNEL_COUNT],
        }
    }
}

impl Calibration {
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
            && self.dac_cals.iter().all(|calibration| {
                calibration.slope.is_finite()
                    && calibration.slope > 0.0
                    && calibration.offset.is_finite()
            })
    }
}

/// Converts a requested calibrated DAC voltage to a bounded hardware code.
#[inline]
pub fn dac_code(requested_voltage_v: f32, calibration: &LinearCalibration) -> u16 {
    let nominal_voltage_v = calibration.unapply(requested_voltage_v).clamp(0.0, VREF);
    (nominal_voltage_v * (DAC_MAX_CODE as f32 / VREF)) as u16
}

/// Converts the cold-junction ADC channel to unfiltered board temperature.
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
    calibration: &Calibration,
) -> f32 {
    let sensed_v = calibration.voltage_cals[2].apply(samples[2] / RTD_FRONTEND_GAIN);
    crate::calcs::pt100_temperature_k_f32(sensed_v / RTD_REFERENCE_CURRENT_A)
}

/// Populate the analog engineering fields of a coherent snapshot.
///
/// Calibration is applied to sensed voltage before the engineering conversions.
/// The caller supplies the filtered board temperature so both thermocouples use
/// the same publication-cycle value.
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
    calibration: &Calibration,
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
