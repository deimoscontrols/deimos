use super::Peripheral;
use crate::calcs::{Affine, Calc, InverseAffine, RtdPt100, TcKtype};
use deimos_shared::peripherals::{analog_i_rev_4::*, model_numbers, PeripheralId};
use deimos_shared::OperatingMetrics;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Serialize, Deserialize, Debug)]
pub struct AnalogIRev4 {
    pub serial_number: u64,
}

#[typetag::serde]
impl Peripheral for AnalogIRev4 {
    fn id(&self) -> PeripheralId {
        PeripheralId {
            model_number: model_numbers::ANALOG_I_REV_4_MODEL_NUMBER,
            serial_number: self.serial_number,
        }
    }

    fn input_names(&self) -> Vec<String> {
        let mut names = Vec::new();

        for i in 0..4 {
            names.push(format!("pwm{i}_duty").to_owned())
        }

        for i in 0..4 {
            names.push(format!("pwm{i}_freq").to_owned())
        }

        names
    }

    fn output_names(&self) -> Vec<String> {
        let mut names = Vec::new();

        for i in 0..20 {
            names.push(format!("ain{i}").to_owned())
        }
        names.push("encoder".to_owned());
        names.push("counter".to_owned());
        names.push("freq0".to_owned());
        names.push("freq1".to_owned());
        names.push("duty0".to_owned());

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
        let mut pwm_duty_frac = [0_f32; 4];
        let mut pwm_freq_hz = [0_u32; 4];

        for i in 0..4 {
            pwm_duty_frac[i] = (inputs[i] as f32).clamp(0.0, 1.0);
            pwm_freq_hz[i] = inputs[i + 4].clamp(1.0, u32::MAX as f64) as u32;
        }

        OperatingRoundtripInput {
            id,
            period_delta_ns,
            phase_delta_ns,
            pwm_duty_frac,
            pwm_freq_hz,
        }
        .write_bytes(bytes);
    }

    fn parse_operating_roundtrip(&self, bytes: &[u8], outputs: &mut [f64]) -> OperatingMetrics {
        let n = self.operating_roundtrip_output_size();
        let out = OperatingRoundtripOutput::read_bytes(&bytes[..n]);
        for i in 0..20 {
            outputs[i] = out.adc_voltages[i] as f64;
        }
        outputs[20] = out.encoder as f64;
        outputs[21] = out.pulse_counter as f64;
        outputs[22] = out.frequency_meas[0] as f64;
        outputs[23] = out.frequency_meas[1] as f64;
        outputs[24] = out.duty_cycle_meas as f64;

        out.metrics
    }

    /// Get a standard set of calcs that convert the raw outputs
    /// into a useable format.
    fn standard_calcs(&self, name: String) -> BTreeMap<String, Box<dyn Calc>> {
        let mut calcs: BTreeMap<String, Box<dyn Calc>> = BTreeMap::new();

        {
            // Bus current measured on shunt resistors with G=50
            let module_bus_current = Affine::new(format!("{name}.ain0"), 4.0 / 1.5, 0.0, true);
            calcs.insert(
                format!("{name}_bus_current_A"),
                Box::new(module_bus_current),
            );

            // Bus voltage measured with sub-unity gain
            let module_bus_voltage = Affine::new(format!("{name}.ain1"), 21.5 / 1.5, 0.0, true);
            calcs.insert(
                format!("{name}_bus_voltage_V"),
                Box::new(module_bus_voltage),
            );
        }

        // Cold junction RTD is also board temp
        {
            let i = 2;
            let input_name = format!("{name}.ain{i}");
            let resistance_calc_name = format!("{name}_board_temp_resistance_ohm");
            let temperature_calc_name: String = format!("{name}_board_temp");
            // v_sensed = 250e-6 amps * r_sensed * 25.7
            // => r_sensed = v_sensed / (250e-6 * 25.7)
            let slope = 250e-6 * 25.7;
            let resistance_calc = InverseAffine::new(input_name, slope, 0.0, true);
            let temperature_calc = RtdPt100::new(format!("{resistance_calc_name}.y"), true);
            calcs.insert(resistance_calc_name, Box::new(resistance_calc));
            calcs.insert(temperature_calc_name.clone(), Box::new(temperature_calc));
        }

        // The sensor analog frontends occupy contiguous blocks of channels
        let milliamp_4_20_range = 3..=7;
        let rtd_range = 8..=12;
        let tc_range = 13..=16;

        // 4-20mA channels use a 75 ohm reference resistor and G=1 amp
        {
            for i in milliamp_4_20_range {
                let n = i - 2;
                let input_name = format!("{name}.ain{i}");
                let calc_name = format!("{name}_4_20_mA_{n}_A");
                let slope = 75.0; // [V/A] due to 75 ohm resistor
                calcs.insert(
                    calc_name,
                    Box::new(InverseAffine::new(input_name, slope, 0.0, true)),
                );
            }
        }

        // RTDs use a 250uA reference current and gain of 25.7
        {
            for i in rtd_range {
                let n = i - 7;
                let input_name = format!("{name}.ain{i}");
                let resistance_calc_name = format!("{name}_rtd_{n}_resistance_ohm");
                let temperature_calc_name = format!("{name}_rtd_{n}");
                // v_sensed = 250e-6 amps * r_sensed * 25.7
                // => r_sensed = v_sensed / (250e-6 * 25.7)
                let slope = 250e-6 * 25.7;
                let resistance_calc = InverseAffine::new(input_name, slope, 0.0, true);
                let temperature_calc = RtdPt100::new(format!("{resistance_calc_name}.y"), true);
                calcs.insert(resistance_calc_name, Box::new(resistance_calc));
                calcs.insert(temperature_calc_name, Box::new(temperature_calc));
            }
        }

        // TCs use a gain of 25.7 as well, and an output offset
        // to allow measuring temperatures below 0C
        {
            for i in tc_range {
                let n = i - 12;
                let slope = 25.7;
                let offset = 1.024;

                let input_name = format!("{name}.ain{i}");
                let voltage_calc_name = format!("{name}_tc_{n}_voltage_V");
                let temperature_calc_name = format!("{name}_tc_{n}_temp_K");

                let voltage_calc = InverseAffine::new(input_name, slope, offset, true);
                let temperature_calc = TcKtype::new(
                    format!("{voltage_calc_name}.y"),
                    format!("{name}_board_temp.temperature_K"),
                    true,
                );
                calcs.insert(voltage_calc_name, Box::new(voltage_calc));
                calcs.insert(temperature_calc_name, Box::new(temperature_calc));
            }
        }
        calcs
    }
}
