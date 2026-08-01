use super::{Peripheral, calibration::CalRecordCore};
use crate::calc::{Calc, RtdPt100};
use deimos_shared::{
    OperatingMetrics,
    peripherals::{PeripheralId, deimos_daq_rev7::*},
    states::{AcknowledgeConfiguration, ConfiguringInput},
};
use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

#[cfg(feature = "python")]
use pyo3::prelude::*;

use crate::py_peripheral_methods;
pub mod calibration_7_0_0;

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
/// Human-readable rev7 calibration artifact and its provenance.
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
    pub fn firmware_calibration(&self, calibrated: bool) -> Result<Rev7Calibration, String> {
        let voltage_cals = self.voltage_cals.map(|cal| LinearCalibration {
            slope: cal.slope as f32,
            offset: cal.offset as f32,
        });
        let calibration = Rev7Calibration {
            firmware_calibrated: u8::from(calibrated),
            voltage_cals,
        };
        if !calibration.is_valid() {
            return Err(
                "Rev7 calibration contains an invalid or non-finite coefficient".to_owned(),
            );
        }
        Ok(calibration)
    }
}

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
            "module_bus_current_A".to_owned(),
            "module_bus_voltage_V".to_owned(),
            "board_temperature_K".to_owned(),
        ];
        for i in 0..CURRENT_4_20_CHANNEL_COUNT {
            names.push(format!("current_4_20_{i}_A"));
        }
        for i in 0..RTD_CHANNEL_COUNT {
            names.push(format!("rtd_{i}_resistance_ohm"));
        }
        for i in 0..THERMOCOUPLE_CHANNEL_COUNT {
            names.push(format!("thermocouple_{i}_temperature_K"));
        }
        for name in [
            "voltage_0_2V5_0_V",
            "voltage_0_2V5_1_V",
            "voltage_0_15_0_V",
            "voltage_0_15_1_V",
            "voltage_x26_0_V",
            "voltage_x26_1_V",
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
        let mut packet = OperatingRoundtripInput::default();
        packet.id = id;
        packet.period_delta_ns = period_delta_ns;
        packet.phase_delta_ns = phase_delta_ns;
        for i in 0..PWM_CHANNEL_COUNT {
            packet.outputs.pwm_duty_frac[i] = (inputs[i] as f32).clamp(0.0, 1.0);
            packet.outputs.pwm_freq_hz[i] =
                inputs[i + PWM_CHANNEL_COUNT].clamp(1.0, u32::MAX as f64) as u32;
        }
        let dac_start = PWM_CHANNEL_COUNT * 2;
        packet.outputs.dac_v = [
            (inputs[dac_start] as f32).clamp(0.0, VREF),
            (inputs[dac_start + 1] as f32).clamp(0.0, VREF),
        ];
        let digital_output_start = dac_start + DAC_CHANNEL_COUNT;
        for i in 0..DIGITAL_OUTPUT_COUNT {
            packet.outputs.gpio |= u8::from(inputs[digital_output_start + i] != 0.0) << i;
        }
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
        Rev7ConfiguringInput::BYTE_LEN
    }

    fn configuring_output_size(&self) -> usize {
        Rev7ConfiguringOutput::BYTE_LEN
    }

    fn emit_configuring(&self, base: ConfiguringInput, bytes: &mut [u8]) {
        Rev7ConfiguringInput::from_base(base).write_bytes(bytes);
    }

    fn parse_configuring(&self, bytes: &[u8]) -> Result<Option<bool>, String> {
        if bytes.len() != Rev7ConfiguringOutput::BYTE_LEN {
            return Err("Invalid rev7 configuring response length".to_owned());
        }
        let response = Rev7ConfiguringOutput::read_bytes(bytes);
        if !response.is_valid() {
            return Err("Invalid rev7 configuring response magic or calibration flag".to_owned());
        }
        match response.acknowledge {
            AcknowledgeConfiguration::Ack => Ok(Some(response.firmware_calibrated != 0)),
            response => Err(format!("{response:?}")),
        }
    }

    fn requires_host_calibration_artifact(&self) -> bool {
        false
    }

    /// The firmware now publishes all engineering conversions except external RTD temperature.
    fn standard_calcs(
        &self,
        name: &str,
        _cals: &str,
    ) -> Result<BTreeMap<String, Box<dyn Calc>>, String> {
        let mut calcs: BTreeMap<String, Box<dyn Calc>> = BTreeMap::new();
        for i in 0..RTD_CHANNEL_COUNT {
            calcs.insert(
                format!("{name}_rtd_{i}"),
                RtdPt100::new(format!("{name}.rtd_{i}_resistance_ohm"), true),
            );
        }
        Ok(calcs)
    }

    fn default_cals(&self) -> Result<String, String> {
        serde_json::to_string(&CalRecord::default())
            .map_err(|e| format!("Failed to serialize default rev7 cals: {e}"))
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
    fn packet_output_count_matches_names() {
        let peripheral = DeimosDaqRev7::default();
        let packet = OperatingSnapshot {
            sample_time_ns: 0x0012_3456_789a_bcde,
            ..OperatingSnapshot::default()
        };
        let mut bytes = vec![0; OperatingSnapshot::BYTE_LEN];
        packet.write_bytes(&mut bytes);
        let mut outputs = vec![0.0; peripheral.output_names().len()];
        peripheral.parse_operating_roundtrip(&bytes, &mut outputs);
        assert_eq!(outputs.len(), 25);
        assert_eq!(outputs[0], packet.sample_time_ns as f64);
        assert_eq!(peripheral.output_names()[0], "sample_time_ns");
    }
}
