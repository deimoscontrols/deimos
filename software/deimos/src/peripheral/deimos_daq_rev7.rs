use super::Peripheral;
use crate::{
    calc::{Calc, RtdPt100},
    fmt_time,
};
use deimos_shared::{
    OperatingMetrics,
    peripherals::{PeripheralId, deimos_daq_rev7::*},
    states::{AcknowledgeConfiguration, ConfiguringInput as BaseConfiguringInput},
};
use std::{collections::BTreeMap, time::SystemTime};

use serde::{Deserialize, Serialize};

#[cfg(feature = "python")]
use pyo3::prelude::*;

use crate::py_peripheral_methods;

/// Schema version for the shared fields in a calibration record.
pub const CURRENT_CAL_SCHEMA_VERSION: u16 = 1;

/// Procedure and instrument provenance for a generated calibration.
#[derive(Serialize, Deserialize, Debug, Default)]
pub struct CalRecordCore {
    /// Schema version for these shared top-level fields.
    pub schema_version: u16,
    /// Peripheral implementation kind.
    pub peripheral_kind: String,
    /// Numeric peripheral model identifier.
    pub model_number: u64,
    /// Unit serial number.
    pub serial_number: u64,
    /// Calibration procedure identifier.
    pub procedure: String,
    /// Version of the calibration procedure that produced this record.
    pub procedure_version: u16,
    /// UTC timestamp when this record was generated.
    pub generated_at_utc: String,
    /// Records-folder references for calibrators used by the procedure.
    pub calibrators: Vec<String>,
}

impl CalRecordCore {
    /// Construct calibration-record provenance using the current schema version.
    ///
    /// Args:
    ///   peripheral_kind: Stable software type name for the calibrated device.
    ///   model_number: Numeric peripheral model identifier.
    ///   serial_number: Unit serial number.
    ///   procedure: Calibration procedure identifier.
    ///   procedure_version: Version of the calibration procedure.
    ///   calibrators: Records-folder references for instruments used by the procedure.
    ///
    /// Returns:
    ///   Provenance populated with the current UTC generation timestamp.
    pub fn new(
        peripheral_kind: impl Into<String>,
        model_number: u64,
        serial_number: u64,
        procedure: impl Into<String>,
        procedure_version: u16,
        calibrators: Vec<String>,
    ) -> Self {
        Self {
            schema_version: CURRENT_CAL_SCHEMA_VERSION,
            peripheral_kind: peripheral_kind.into(),
            model_number,
            serial_number,
            procedure: procedure.into(),
            procedure_version,
            generated_at_utc: fmt_time(SystemTime::now()),
            calibrators,
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy)]
/// Human-readable affine sensed-voltage calibration.
pub struct LinearCal {
    /// Dimensionless sensed-voltage scale factor.
    pub slope: f64,
    /// Sensed-voltage offset in `V`.
    pub offset: f64,
}

impl Default for LinearCal {
    fn default() -> Self {
        Self {
            slope: 1.0,
            offset: 0.0,
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Default)]
/// Human-readable calibration artifact and its provenance.
pub struct CalRecord {
    /// Calibration procedure metadata shared by all peripheral kinds.
    pub core: CalRecordCore,
    /// Sensed-voltage calibrations with shape `(ADC_CHANNEL_COUNT,)` and
    /// channel order `ain0..ain12, ain15..ain19`.
    pub voltage_cals: [LinearCal; ADC_CHANNEL_COUNT],
}

impl CalRecord {
    /// Converts the human-readable calibration artifact to its firmware image record.
    ///
    /// Coefficients are narrowed from `f64` to the firmware's `f32` representation
    /// and validated after conversion.
    ///
    /// Args:
    ///   calibrated: Status to encode after a calibration procedure (`true`) or
    ///     for an identity image installed before calibration (`false`).
    ///
    /// Returns:
    ///   Validated fixed-layout firmware calibration record, or an error if a
    ///   narrowed coefficient is nonfinite or has zero slope.
    pub fn firmware_calibration(&self, calibrated: bool) -> Result<Calibration, String> {
        let voltage_cals = self.voltage_cals.map(|cal| LinearCalibration {
            slope: cal.slope as f32,
            offset: cal.offset as f32,
        });
        let calibration = Calibration {
            firmware_calibrated: u8::from(calibrated),
            voltage_cals,
        };
        if !calibration.is_valid() {
            return Err("Calibration contains an invalid or non-finite coefficient".to_owned());
        }
        Ok(calibration)
    }
}

/// Software interface for a Deimos DAQ rev7 peripheral.
///
/// The complete Modbus/TCP register map is documented in
/// [`deimos_shared::peripherals::deimos_daq_rev7::modbus`].
#[derive(Serialize, Deserialize, Debug, Default)]
#[cfg_attr(feature = "python", pyclass)]
pub struct DeimosDaqRev7 {
    pub serial_number: u64,
}

py_peripheral_methods!(DeimosDaqRev7);

#[typetag::serde]
impl Peripheral for DeimosDaqRev7 {
    fn id(&self) -> PeripheralId {
        PeripheralId {
            model_number: MODEL_NUMBER,
            serial_number: self.serial_number,
        }
    }

    fn input_names(&self) -> Vec<String> {
        let mut names = Vec::new();
        for i in 0..PWM_CHANNEL_COUNT {
            names.push(format!("pwm{i}_duty"));
        }
        for i in 0..PWM_CHANNEL_COUNT {
            names.push(format!("pwm{i}_freq"));
        }
        for i in 0..DAC_CHANNEL_COUNT {
            names.push(format!("dac{i}"));
        }
        for i in 0..DIGITAL_OUTPUT_COUNT {
            names.push(format!("do{i}"));
        }
        names
    }

    fn output_names(&self) -> Vec<String> {
        let mut names = vec![
            "sample_time_ns".to_owned(),
            "bus_A".to_owned(),
            "bus_V".to_owned(),
            "board_temp_K".to_owned(),
        ];
        for i in 0..CURRENT_4_20_CHANNEL_COUNT {
            names.push(format!("4_20_{i}_A"));
        }
        for i in 0..RTD_CHANNEL_COUNT {
            names.push(format!("res_{i}_ohm"));
        }
        for i in 0..THERMOCOUPLE_CHANNEL_COUNT {
            names.push(format!("tc_{i}_K"));
        }
        for name in [
            "2V5_0_V", "2V5_1_V", "15V_0_V", "15V_1_V", "35mV_0_V", "35mV_1_V",
        ] {
            names.push(name.to_owned());
        }
        names.extend(["encoder", "counter", "freq0", "freq1", "di0", "di1"].map(str::to_owned));
        names
    }

    fn operating_roundtrip_input_size(&self) -> usize {
        OperatingRoundtripInput::BYTE_LEN
    }

    fn operating_roundtrip_output_size(&self) -> usize {
        OperatingSnapshot::BYTE_LEN
    }

    fn emit_operating_roundtrip(
        &self,
        id: u64,
        period_delta_ns: i64,
        phase_delta_ns: i64,
        inputs: &[f64],
        bytes: &mut [u8],
    ) {
        let mut packet = OperatingRoundtripInput {
            id,
            period_delta_ns,
            phase_delta_ns,
            ..OperatingRoundtripInput::default()
        };
        for i in 0..PWM_CHANNEL_COUNT {
            packet.outputs.pwm_duty_frac[i] = inputs[i] as f32;
            packet.outputs.pwm_freq_hz[i] =
                inputs[i + PWM_CHANNEL_COUNT].clamp(1.0, u32::MAX as f64) as u32;
        }
        let dac_start = PWM_CHANNEL_COUNT * 2;
        for i in 0..DAC_CHANNEL_COUNT {
            packet.outputs.dac_v[i] = inputs[dac_start + i] as f32;
        }
        let digital_output_start = dac_start + DAC_CHANNEL_COUNT;
        for i in 0..DIGITAL_OUTPUT_COUNT {
            let value = inputs[digital_output_start + i];
            packet.outputs.gpio |= u8::from(value.clamp(0.0, 1.0) >= 0.5) << i;
        }
        packet.outputs.normalize();
        debug_assert!(packet.outputs.is_valid());
        packet.write_bytes(bytes);
    }

    fn validate_operating_roundtrip(&self, bytes: &[u8]) -> bool {
        bytes.len() == OperatingSnapshot::BYTE_LEN
            && OperatingSnapshot::read_bytes(bytes).is_valid()
    }

    fn parse_operating_roundtrip(&self, bytes: &[u8], outputs: &mut [f64]) -> OperatingMetrics {
        let packet = OperatingSnapshot::read_bytes(bytes);
        outputs[0] = packet.sample_time_ns as f64;
        let mut index = 1;
        for value in [
            packet.module_bus_current_a,
            packet.module_bus_voltage_v,
            packet.board_temperature_k,
        ] {
            outputs[index] = value as f64;
            index += 1;
        }
        for value in packet.current_4_20_a {
            outputs[index] = value as f64;
            index += 1;
        }
        for value in packet.rtd_resistance_ohm {
            outputs[index] = value as f64;
            index += 1;
        }
        for value in packet.thermocouple_temperature_k {
            outputs[index] = value as f64;
            index += 1;
        }
        for value in packet.voltage_v {
            outputs[index] = value as f64;
            index += 1;
        }
        outputs[index] = packet.encoder as f64;
        outputs[index + 1] = packet.pulse_counter as f64;
        outputs[index + 2] = packet.frequency_meas[0] as f64;
        outputs[index + 3] = packet.frequency_meas[1] as f64;
        outputs[index + 4] = (packet.gpio & 1) as f64;
        outputs[index + 5] = ((packet.gpio >> 1) & 1) as f64;
        OperatingMetrics {
            id: packet.metrics.id,
            sent_time_ns: packet.metrics.sent_time_ns,
            last_input_id: packet.metrics.last_input_id,
            last_input_received_time_ns: packet.metrics.last_input_received_time_ns,
            cycle_time_margin_ns: packet.metrics.cycle_time_margin_ns,
            ..OperatingMetrics::default()
        }
    }

    fn configuring_input_size(&self) -> usize {
        ConfiguringInput::BYTE_LEN
    }

    fn configuring_output_size(&self) -> usize {
        ConfiguringOutput::BYTE_LEN
    }

    fn validate_configuring(&self, base: BaseConfiguringInput) -> Result<(), String> {
        let config = ConfiguringInput::from_base(base);
        if config.is_valid() {
            return Ok(());
        }

        match config.validation_acknowledgement() {
            Some(AcknowledgeConfiguration::NakDtTooSmall) => Err(format!(
                "dt_ns={} is shorter than the supported minimum {} ns (maximum cycle rate {} Hz)",
                config.dt_ns, DEIMOS_MIN_CYCLE_PERIOD_NS, DEIMOS_MAX_CYCLE_RATE_HZ,
            )),
            Some(AcknowledgeConfiguration::NakDtTooLarge) => Err(format!(
                "dt_ns={} exceeds the supported maximum {} ns (minimum cycle rate {} Hz)",
                config.dt_ns, DEIMOS_MAX_CYCLE_PERIOD_NS, MIN_CYCLE_RATE_HZ,
            )),
            Some(response) => Err(format!(
                "Configuration was rejected with unexpected response {response:?}",
            )),
            None => Err("Configuration has an invalid packet marker".to_owned()),
        }
    }

    fn emit_configuring(&self, base: BaseConfiguringInput, bytes: &mut [u8]) {
        ConfiguringInput::from_base(base).write_bytes(bytes);
    }

    fn parse_configuring(&self, bytes: &[u8]) -> Result<Option<bool>, String> {
        if bytes.len() != ConfiguringOutput::BYTE_LEN {
            return Err("Invalid configuring response length".to_owned());
        }
        let response = ConfiguringOutput::read_bytes(bytes);
        if !response.is_valid() {
            return Err("Invalid configuring response magic or calibration flag".to_owned());
        }
        match response.acknowledge {
            AcknowledgeConfiguration::Ack => Ok(Some(response.firmware_calibrated != 0)),
            response => Err(format!("{response:?}")),
        }
    }

    /// Firmware publishes all engineering conversions except external RTD temperature.
    fn standard_calcs(&self, name: &str) -> BTreeMap<String, Box<dyn Calc>> {
        let mut calcs: BTreeMap<String, Box<dyn Calc>> = BTreeMap::new();
        for i in 0..RTD_CHANNEL_COUNT {
            calcs.insert(
                format!("{name}_rtd_{i}"),
                RtdPt100::new(format!("{name}.res_{i}_ohm"), true),
            );
        }
        calcs
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn firmware_calibration_conversion_rejects_nonfinite_data() {
        let mut record = CalRecord::default();
        record.voltage_cals[0].slope = f64::NAN;
        assert!(record.firmware_calibration(true).is_err());
    }

    #[test]
    fn configuring_validation_reports_supported_limits_before_transmission() {
        let peripheral = DeimosDaqRev7::default();
        let mut config = BaseConfiguringInput {
            dt_ns: DEIMOS_MIN_CYCLE_PERIOD_NS,
            ..BaseConfiguringInput::default()
        };
        assert!(peripheral.validate_configuring(config).is_ok());

        config.dt_ns = DEIMOS_MIN_CYCLE_PERIOD_NS - 1;
        let error = peripheral.validate_configuring(config).unwrap_err();
        assert!(error.contains("shorter than the supported minimum"));
        assert!(error.contains(&DEIMOS_MIN_CYCLE_PERIOD_NS.to_string()));

        config.dt_ns = DEIMOS_MAX_CYCLE_PERIOD_NS + 1;
        let error = peripheral.validate_configuring(config).unwrap_err();
        assert!(error.contains("exceeds the supported maximum"));
        assert!(error.contains(&DEIMOS_MAX_CYCLE_PERIOD_NS.to_string()));
    }

    #[test]
    fn packet_parser_populates_the_named_sample_timestamp() {
        let peripheral = DeimosDaqRev7::default();
        let packet = OperatingSnapshot {
            sample_time_ns: 0x0012_3456_789a_bcde,
            ..OperatingSnapshot::default()
        };
        let mut bytes = vec![0; OperatingSnapshot::BYTE_LEN];
        packet.write_bytes(&mut bytes);
        let output_names = peripheral.output_names();
        let mut outputs = vec![0.0; output_names.len()];
        peripheral.parse_operating_roundtrip(&bytes, &mut outputs);
        let sample_time_index = output_names
            .iter()
            .position(|name| name == "sample_time_ns")
            .expect("sample timestamp output");
        assert_eq!(outputs[sample_time_index], packet.sample_time_ns as f64);
    }

    #[test]
    fn operating_outputs_clamp_overshoot_and_safe_state_nan() {
        let peripheral = DeimosDaqRev7::default();
        let inputs = [
            f64::NEG_INFINITY,
            f64::INFINITY,
            f64::NAN,
            0.5,
            0.0,
            f64::INFINITY,
            1_000.0,
            f64::NAN,
            f64::NAN,
            f64::INFINITY,
            f64::NAN,
            f64::INFINITY,
            -0.1,
            0.6,
        ];
        let mut bytes = vec![0; OperatingRoundtripInput::BYTE_LEN];
        peripheral.emit_operating_roundtrip(1, 0, 0, &inputs, &mut bytes);
        let packet = OperatingRoundtripInput::read_bytes(&bytes);

        assert!(packet.is_valid());
        assert_eq!(packet.outputs.pwm_duty_frac, [0.0, 1.0, 0.0, 0.5]);
        assert_eq!(packet.outputs.pwm_freq_hz, [1, u32::MAX, 1_000, 1_000_000]);
        assert_eq!(packet.outputs.dac_v, [0.0, VREF]);
        assert_eq!(packet.outputs.gpio, 0b1010);
    }
}
