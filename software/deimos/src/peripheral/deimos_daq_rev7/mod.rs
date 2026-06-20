use super::Peripheral;
use crate::calc::{Affine, Butter2, Calc, InverseAffine, RtdPt100, TcKtype};
use deimos_shared::OperatingMetrics;
use deimos_shared::peripherals::{PeripheralId, deimos_daq_rev7::*};
use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

#[cfg(feature = "python")]
use pyo3::prelude::*;

use crate::py_peripheral_methods;

pub mod calibration_7_0_0;

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
        if !cals.is_empty() {
            return Err(format!("{} does not support calibration data", self.kind()));
        }

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
            let input_name = format!("{name}.ain{i}");
            let resistance_calc_name = format!("{name}_board_rtd_ohm");
            let temperature_calc_name: String = format!("{name}_board_rtd");
            // v_sensed = 250e-6 amps * r_sensed * 25.7
            // => r_sensed = v_sensed / (250e-6 * 25.7)
            let slope = 250e-6 * 25.7;
            let resistance_calc = InverseAffine::new(input_name, slope, 0.0, true);
            let temperature_calc = RtdPt100::new(format!("{resistance_calc_name}.y"), true);
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
                let input_name = format!("{name}.ain{i}");
                let calc_name = format!("{name}_4_20_mA_{n}_A");
                let slope = 75.0; // [V/A] due to 75 ohm resistor
                calcs.insert(calc_name, InverseAffine::new(input_name, slope, 0.0, true));
            }
        }

        // Resistance sensors use a 250uA reference current and gain of 25.7
        {
            for (n, i) in rtd_range.enumerate() {
                let input_name = format!("{name}.ain{i}");
                let resistance_calc_name = format!("{name}_ohm_{n}");
                let temperature_calc_name = format!("{name}_rtd_{n}");
                // v_sensed = 250e-6 amps * r_sensed * 25.7
                // => r_sensed = v_sensed / (250e-6 * 25.7)
                let slope = 250e-6 * 25.7;
                let resistance_calc = InverseAffine::new(input_name, slope, 0.0, true);
                let temperature_calc = RtdPt100::new(format!("{resistance_calc_name}.y"), true);
                calcs.insert(resistance_calc_name, resistance_calc);
                calcs.insert(temperature_calc_name, temperature_calc);
            }
        }

        // TCs use a gain of 25.7 as well, and an output offset
        // to allow measuring temperatures below 0C
        {
            for (n, i) in tc_range.enumerate() {
                let slope = 25.7;
                let offset = 1.024;

                let input_name = format!("{name}.ain{i}");
                let voltage_calc_name = format!("{name}_tc_{n}_V");
                let temperature_calc_name = format!("{name}_tc_{n}");

                let voltage_calc = InverseAffine::new(input_name, slope, offset, true);
                let temperature_calc = TcKtype::new(
                    format!("{voltage_calc_name}.y"),
                    format!("{name}_board_rtd_filtered.y"),
                    true,
                );
                calcs.insert(voltage_calc_name, voltage_calc);
                calcs.insert(temperature_calc_name, temperature_calc);
            }
        }

        // Variety of raw voltages with different gains
        {
            let input_name = format!("{name}.ain12");
            let voltage_calc_name = format!("{name}_0_2V5_0");
            let voltage_calc = Affine::new(input_name, 1.0, 0.0, true);
            calcs.insert(voltage_calc_name, voltage_calc);
        }

        {
            let input_name = format!("{name}.ain15");
            let voltage_calc_name = format!("{name}_0_2V5_1");
            let voltage_calc = Affine::new(input_name, 1.0, 0.0, true);
            calcs.insert(voltage_calc_name, voltage_calc);
        }

        {
            let input_name = format!("{name}.ain16");
            let voltage_calc_name = format!("{name}_0_15V_0");
            let voltage_calc = Affine::new(input_name, 6.0, 0.0, true);
            calcs.insert(voltage_calc_name, voltage_calc);
        }

        {
            let input_name = format!("{name}.ain17");
            let voltage_calc_name = format!("{name}_0_15V_1");
            let voltage_calc = Affine::new(input_name, 6.0, 0.0, true);
            calcs.insert(voltage_calc_name, voltage_calc);
        }

        {
            let input_name = format!("{name}.ain18");
            let voltage_calc_name = format!("{name}_x26_0");
            let voltage_calc = InverseAffine::new(input_name, 25.7, 1.024, true);
            calcs.insert(voltage_calc_name, voltage_calc);
        }

        {
            let input_name = format!("{name}.ain19");
            let voltage_calc_name = format!("{name}_x26_1");
            let voltage_calc = InverseAffine::new(input_name, 25.7, 1.024, true);
            calcs.insert(voltage_calc_name, voltage_calc);
        }

        Ok(calcs)
    }
}
