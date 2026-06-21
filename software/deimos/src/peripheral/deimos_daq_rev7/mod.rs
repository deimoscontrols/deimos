use super::{Peripheral, calibration::CalRecordCore};
use crate::calc::{Affine, Butter2, Calc, InverseAffine, RtdPt100, TcKtype};
use deimos_shared::OperatingMetrics;
use deimos_shared::peripherals::{PeripheralId, deimos_daq_rev7::*};
use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

#[cfg(feature = "python")]
use pyo3::prelude::*;

use crate::py_peripheral_methods;

pub mod calibration_7_0_0;

#[derive(Serialize, Deserialize, Debug)]
pub struct LinearCal {
    slope: f64,
    offset: f64,
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
pub struct CalRecord {
    pub core: CalRecordCore,
    pub voltage_cals: [LinearCal; 18],
}

const CURRENT_REFERENCE_RESISTOR_OHM: f64 = 75.0;
const RTD_REFERENCE_CURRENT_A: f64 = 250e-6;
const RTD_FRONTEND_GAIN: f64 = 25.7;
const TC_FRONTEND_GAIN: f64 = 25.7;
const TC_FRONTEND_OFFSET_V: f64 = 1.024;

fn adc_cal_index_for_ain(ain_index: usize) -> Result<usize, String> {
    match ain_index {
        0..=12 => Ok(ain_index),
        15..=19 => Ok(ain_index - 2),
        _ => Err(format!(
            "Analog input ain{ain_index} does not have a rev7 calibration slot"
        )),
    }
}

fn voltage_cal_for_ain(cal_record: &CalRecord, ain_index: usize) -> Result<&LinearCal, String> {
    let cal_index = adc_cal_index_for_ain(ain_index)?;
    cal_record
        .voltage_cals
        .get(cal_index)
        .ok_or_else(|| format!("Calibration slot {cal_index} missing for ain{ain_index}"))
}

fn calibrated_sense_voltage(
    input_name: String,
    cal: &LinearCal,
    save_outputs: bool,
) -> Box<dyn Calc> {
    Affine::new(input_name, cal.slope, cal.offset, save_outputs)
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
            names.push(format!("pwm{i}_duty").to_owned())
        }

        for i in 0..PWM_CHANNEL_COUNT {
            names.push(format!("pwm{i}_freq").to_owned())
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
        let mut names = Vec::new();

        for i in 0..=12 {
            names.push(format!("ain{i}").to_owned())
        }

        for i in 15..=19 {
            names.push(format!("ain{i}").to_owned())
        }

        names.push("encoder".to_owned());
        names.push("counter".to_owned());
        names.push("freq0".to_owned());
        names.push("freq1".to_owned());

        names.push("di0".to_owned());
        names.push("di1".to_owned());

        names
    }

    fn operating_roundtrip_input_size(&self) -> usize {
        OperatingRoundtripInput::BYTE_LEN
    }

    fn operating_roundtrip_output_size(&self) -> usize {
        OperatingRoundtripOutput::BYTE_LEN
    }

    fn emit_operating_roundtrip(
        &self,
        id: u64,
        period_delta_ns: i64,
        phase_delta_ns: i64,
        inputs: &[f64],
        bytes: &mut [u8],
    ) {
        let mut pwm_duty_frac = [0_f32; PWM_CHANNEL_COUNT];
        let mut pwm_freq_hz = [0_u32; PWM_CHANNEL_COUNT];

        for i in 0..PWM_CHANNEL_COUNT {
            pwm_duty_frac[i] = (inputs[i] as f32).clamp(0.0, 1.0);
            pwm_freq_hz[i] = inputs[i + PWM_CHANNEL_COUNT].clamp(1.0, u32::MAX as f64) as u32;
        }

        let dac_start = PWM_CHANNEL_COUNT * 2;
        let dac_v = [
            (inputs[dac_start] as f32).clamp(0.0, VREF),
            (inputs[dac_start + 1] as f32).clamp(0.0, VREF),
        ];

        let mut gpio = 0_u8;
        let digital_output_start = dac_start + DAC_CHANNEL_COUNT;

        for i in 0..DIGITAL_OUTPUT_COUNT {
            if inputs[digital_output_start + i] != 0.0 {
                gpio |= 1 << i;
            }
        }

        OperatingRoundtripInput {
            id,
            period_delta_ns,
            phase_delta_ns,
            pwm_duty_frac,
            pwm_freq_hz,
            dac_v,
            gpio,
        }
        .write_bytes(bytes);
    }

    fn parse_operating_roundtrip(&self, bytes: &[u8], outputs: &mut [f64]) -> OperatingMetrics {
        let n = self.operating_roundtrip_output_size();
        let out = OperatingRoundtripOutput::read_bytes(&bytes[..n]);
        for i in 0..ADC_CHANNEL_COUNT {
            outputs[i] = out.adc_voltages[i] as f64;
        }
        let counter_start = ADC_CHANNEL_COUNT;
        let frequency_start = counter_start + COUNTER_CHANNEL_COUNT;
        let digital_input_start = frequency_start + FREQUENCY_CHANNEL_COUNT;

        outputs[counter_start] = out.encoder as f64;
        outputs[counter_start + 1] = out.pulse_counter as f64;
        outputs[frequency_start] = out.frequency_meas[0] as f64;
        outputs[frequency_start + 1] = out.frequency_meas[1] as f64;

        outputs[digital_input_start] = (out.gpio & 0b01) as f64;
        outputs[digital_input_start + 1] = ((out.gpio >> 1) & 0b01) as f64;

        out.metrics
    }

    /// Get a standard set of calcs that convert the raw outputs
    /// into a useable format.
    fn standard_calcs(
        &self,
        name: &str,
        cals: &str,
    ) -> Result<BTreeMap<String, Box<dyn Calc>>, String> {
        let cal_record = if cals.trim().is_empty() {
            CalRecord::default()
        } else {
            serde_json::from_str::<CalRecord>(cals)
                .map_err(|e| format!("Failed to parse {} calibration data: {e}", self.kind()))?
        };

        let mut calcs: BTreeMap<String, Box<dyn Calc>> = BTreeMap::new();

        {
            // Bus current measured on 6mohm shunt with G=50
            let module_bus_current =
                Affine::new(format!("{name}.ain0"), 1.0 / (0.006 * 50.0), 0.0, true);
            calcs.insert(format!("{name}_bus_current_A"), module_bus_current);

            // Bus voltage measured with sub-unity gain
            let module_bus_voltage = Affine::new(format!("{name}.ain1"), 21.5 / 1.5, 0.0, true);
            calcs.insert(format!("{name}_bus_voltage_V"), module_bus_voltage);
        }

        // Cold junction RTD is also board temp
        {
            let i = 2;
            let raw_sense_voltage_calc_name = format!("{name}_board_rtd_sense_V_raw");
            let sense_voltage_calc_name = format!("{name}_board_rtd_sense_V");
            let resistance_calc_name = format!("{name}_board_rtd_ohm");
            let temperature_calc_name: String = format!("{name}_board_rtd");
            let raw_sense_voltage_calc =
                InverseAffine::new(format!("{name}.ain{i}"), RTD_FRONTEND_GAIN, 0.0, false);
            let sense_voltage_calc = calibrated_sense_voltage(
                format!("{raw_sense_voltage_calc_name}.y"),
                voltage_cal_for_ain(&cal_record, i)?,
                false,
            );
            let resistance_calc = InverseAffine::new(
                format!("{sense_voltage_calc_name}.y"),
                RTD_REFERENCE_CURRENT_A,
                0.0,
                true,
            );
            let temperature_calc = RtdPt100::new(format!("{resistance_calc_name}.y"), true);
            calcs.insert(raw_sense_voltage_calc_name, raw_sense_voltage_calc);
            calcs.insert(sense_voltage_calc_name, sense_voltage_calc);
            calcs.insert(resistance_calc_name, resistance_calc);
            calcs.insert(temperature_calc_name.clone(), temperature_calc);

            let filtered_calc_name = format!("{name}_board_rtd_filtered");
            let filtered_calc =
                Butter2::new(format!("{temperature_calc_name}.temperature_K"), 1.0, true);
            calcs.insert(filtered_calc_name, filtered_calc);
        }

        // The sensor analog frontends occupy contiguous blocks of channels
        let milliamp_4_20_range = 3..=6;
        let rtd_range = 7..=9;
        let tc_range = 10..=11;

        // 4-20mA channels use a 75 ohm reference resistor and G=1 amp
        {
            for (n, i) in milliamp_4_20_range.enumerate() {
                let sense_voltage_calc_name = format!("{name}_4_20_mA_{n}_sense_V");
                let calc_name = format!("{name}_4_20_mA_{n}_A");
                let sense_voltage_calc = calibrated_sense_voltage(
                    format!("{name}.ain{i}"),
                    voltage_cal_for_ain(&cal_record, i)?,
                    false,
                );
                calcs.insert(sense_voltage_calc_name.clone(), sense_voltage_calc);
                calcs.insert(
                    calc_name,
                    InverseAffine::new(
                        format!("{sense_voltage_calc_name}.y"),
                        CURRENT_REFERENCE_RESISTOR_OHM,
                        0.0,
                        true,
                    ),
                );
            }
        }

        // Resistance sensors use a 250uA reference current and gain of 25.7
        {
            for (n, i) in rtd_range.enumerate() {
                let raw_sense_voltage_calc_name = format!("{name}_rtd_{n}_sense_V_raw");
                let sense_voltage_calc_name = format!("{name}_rtd_{n}_sense_V");
                let resistance_calc_name = format!("{name}_ohm_{n}");
                let temperature_calc_name = format!("{name}_rtd_{n}");
                let raw_sense_voltage_calc =
                    InverseAffine::new(format!("{name}.ain{i}"), RTD_FRONTEND_GAIN, 0.0, false);
                let sense_voltage_calc = calibrated_sense_voltage(
                    format!("{raw_sense_voltage_calc_name}.y"),
                    voltage_cal_for_ain(&cal_record, i)?,
                    false,
                );
                let resistance_calc = InverseAffine::new(
                    format!("{sense_voltage_calc_name}.y"),
                    RTD_REFERENCE_CURRENT_A,
                    0.0,
                    true,
                );
                let temperature_calc = RtdPt100::new(format!("{resistance_calc_name}.y"), true);
                calcs.insert(raw_sense_voltage_calc_name, raw_sense_voltage_calc);
                calcs.insert(sense_voltage_calc_name, sense_voltage_calc);
                calcs.insert(resistance_calc_name, resistance_calc);
                calcs.insert(temperature_calc_name, temperature_calc);
            }
        }

        // TCs use a gain of 25.7 as well, and an output offset
        // to allow measuring temperatures below 0C
        {
            for (n, i) in tc_range.enumerate() {
                let raw_sense_voltage_calc_name = format!("{name}_tc_{n}_sense_V_raw");
                let sense_voltage_calc_name = format!("{name}_tc_{n}_sense_V");
                let temperature_calc_name = format!("{name}_tc_{n}");

                let raw_sense_voltage_calc = InverseAffine::new(
                    format!("{name}.ain{i}"),
                    TC_FRONTEND_GAIN,
                    TC_FRONTEND_OFFSET_V,
                    false,
                );
                let sense_voltage_calc = calibrated_sense_voltage(
                    format!("{raw_sense_voltage_calc_name}.y"),
                    voltage_cal_for_ain(&cal_record, i)?,
                    true,
                );
                let temperature_calc = TcKtype::new(
                    format!("{sense_voltage_calc_name}.y"),
                    format!("{name}_board_rtd_filtered.y"),
                    true,
                );
                calcs.insert(raw_sense_voltage_calc_name, raw_sense_voltage_calc);
                calcs.insert(sense_voltage_calc_name, sense_voltage_calc);
                calcs.insert(temperature_calc_name, temperature_calc);
            }
        }

        // Variety of raw voltages with different gains
        {
            let i = 12;
            let voltage_calc_name = format!("{name}_0_2V5_0_sense_V");
            let voltage_calc = calibrated_sense_voltage(
                format!("{name}.ain{i}"),
                voltage_cal_for_ain(&cal_record, i)?,
                true,
            );
            calcs.insert(voltage_calc_name, voltage_calc);
        }

        {
            let i = 15;
            let voltage_calc_name = format!("{name}_0_2V5_1_sense_V");
            let voltage_calc = calibrated_sense_voltage(
                format!("{name}.ain{i}"),
                voltage_cal_for_ain(&cal_record, i)?,
                true,
            );
            calcs.insert(voltage_calc_name, voltage_calc);
        }

        {
            let i = 16;
            let raw_sense_voltage_calc_name = format!("{name}_0_15V_0_sense_V_raw");
            let voltage_calc_name = format!("{name}_0_15V_0_sense_V");
            let raw_sense_voltage_calc = Affine::new(format!("{name}.ain{i}"), 6.0, 0.0, false);
            let voltage_calc = calibrated_sense_voltage(
                format!("{raw_sense_voltage_calc_name}.y"),
                voltage_cal_for_ain(&cal_record, i)?,
                true,
            );
            calcs.insert(raw_sense_voltage_calc_name, raw_sense_voltage_calc);
            calcs.insert(voltage_calc_name, voltage_calc);
        }

        {
            let i = 17;
            let raw_sense_voltage_calc_name = format!("{name}_0_15V_1_sense_V_raw");
            let voltage_calc_name = format!("{name}_0_15V_1_sense_V");
            let raw_sense_voltage_calc = Affine::new(format!("{name}.ain{i}"), 6.0, 0.0, false);
            let voltage_calc = calibrated_sense_voltage(
                format!("{raw_sense_voltage_calc_name}.y"),
                voltage_cal_for_ain(&cal_record, i)?,
                true,
            );
            calcs.insert(raw_sense_voltage_calc_name, raw_sense_voltage_calc);
            calcs.insert(voltage_calc_name, voltage_calc);
        }

        {
            let i = 18;
            let raw_sense_voltage_calc_name = format!("{name}_x26_0_sense_V_raw");
            let voltage_calc_name = format!("{name}_x26_0_sense_V");
            let raw_sense_voltage_calc = InverseAffine::new(
                format!("{name}.ain{i}"),
                TC_FRONTEND_GAIN,
                TC_FRONTEND_OFFSET_V,
                false,
            );
            let voltage_calc = calibrated_sense_voltage(
                format!("{raw_sense_voltage_calc_name}.y"),
                voltage_cal_for_ain(&cal_record, i)?,
                true,
            );
            calcs.insert(raw_sense_voltage_calc_name, raw_sense_voltage_calc);
            calcs.insert(voltage_calc_name, voltage_calc);
        }

        {
            let i = 19;
            let raw_sense_voltage_calc_name = format!("{name}_x26_1_sense_V_raw");
            let voltage_calc_name = format!("{name}_x26_1_sense_V");
            let raw_sense_voltage_calc = InverseAffine::new(
                format!("{name}.ain{i}"),
                TC_FRONTEND_GAIN,
                TC_FRONTEND_OFFSET_V,
                false,
            );
            let voltage_calc = calibrated_sense_voltage(
                format!("{raw_sense_voltage_calc_name}.y"),
                voltage_cal_for_ain(&cal_record, i)?,
                true,
            );
            calcs.insert(raw_sense_voltage_calc_name, raw_sense_voltage_calc);
            calcs.insert(voltage_calc_name, voltage_calc);
        }

        Ok(calcs)
    }

    fn default_cals(&self) -> Result<String, String> {
        serde_json::to_string(&CalRecord::default()).map_err(|e| {
            format!(
                "Failed to serialize default cals for {}: {}",
                self.kind(),
                e,
            )
        })
    }
}
