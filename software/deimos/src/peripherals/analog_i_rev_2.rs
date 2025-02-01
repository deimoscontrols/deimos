use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use super::Peripheral;
use super::PeripheralId;
use crate::calcs::{Affine, Calc, Constant, InverseAffine, RtdPt100, TcKtype};
use deimos_shared::states::OperatingMetrics;

use deimos_shared::peripherals::{analog_i_rev_2::*, model_numbers};

#[derive(Serialize, Deserialize, Debug)]
pub struct AnalogIRev2 {
    pub serial_number: u64,
}

#[typetag::serde]
impl Peripheral for AnalogIRev2 {
    fn id(&self) -> PeripheralId {
        PeripheralId {
            model_number: model_numbers::ANALOG_I_REV_2_MODEL_NUMBER,
            serial_number: self.serial_number,
        }
    }

    fn input_names(&self) -> Vec<String> {
        let mut names = Vec::new();

        for i in 0..8 {
            names.push(format!("pwm{i}_duty").to_owned())
        }

        for i in 0..8 {
            names.push(format!("pwm{i}_freq").to_owned())
        }

        names
    }

    fn output_names(&self) -> Vec<String> {
        let mut names = Vec::new();

        for i in 0..20 {
            names.push(format!("ain{i}").to_owned())
        }

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
        let mut pwm_duty_frac = [0_f32; 8];
        let mut pwm_freq_hz = [0_u32; 8];

        for i in 0..8 {
            pwm_duty_frac[i] = (inputs[i] as f32).clamp(0.0, 1.0);
            pwm_freq_hz[i] = inputs[i + 8].clamp(1.0, u32::MAX as f64) as u32;
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

        let nout = out.adc_voltages.len();
        for i in 0..nout {
            outputs[i] = out.adc_voltages[i] as f64;
        }

        out.metrics
    }

    /// Get a standard set of calcs that convert the raw outputs
    /// into a useable format.
    fn standard_calcs(&self, name: String) -> BTreeMap<String, Box<dyn Calc>> {
        let mut calcs: BTreeMap<String, Box<dyn Calc>> = BTreeMap::new();

        // Board temperature is on ain0, but doesn't function on this design
        // Until that's fixed, we can baseline 24C (75F) room temp
        let board_temp = Constant::new(0.0 + 273.15, true);
        calcs.insert(format!("{name}_board_temp_K"), Box::new(board_temp));

        // Bus voltage on the pluggable module is on ain1 with 1/3 scale
        let module_bus_voltage = Affine::new(format!("{name}.ain1"), 3.0, 0.0, true);
        calcs.insert(
            format!("{name}_module_bus_voltage_V"),
            Box::new(module_bus_voltage),
        );

        // The sensor analog frontends occupy contiguous blocks of channels
        let milliamp_4_20_range = 2..=7;
        let rtd_range = 8..=12;
        let tc_range = 13..=17;

        // 4-20mA channels use a 100 ohm reference resistor and G=1 amp
        for i in milliamp_4_20_range {
            let n = i - 1;
            let input_name = format!("{name}.ain{i}");
            let calc_name = format!("{name}_4_20_mA_{n}_A");
            let slope = 100.0; // [V/A] due to 100 ohm resistor
            calcs.insert(
                calc_name,
                Box::new(InverseAffine::new(input_name, slope, 0.0, true)),
            );
        }

        // RTDs use a 250uA reference current and gain of 25.7
        for i in rtd_range {
            let n = i - 7;
            let input_name = format!("{name}.ain{i}");
            let resistance_calc_name = format!("{name}_rtd_{n}_resistance_ohm");
            let temperature_calc_name = format!("{name}_rtd_{n}_temp_K");
            // v_sensed = 250e-6 amps * r_sensed * 25.7
            // => r_sensed = v_sensed / (250e-6 * 25.7)
            let slope = 250e-6 * 25.7;
            let resistance_calc = InverseAffine::new(input_name, slope, 0.0, true);
            let temperature_calc = RtdPt100::new(format!("{resistance_calc_name}.y"), true);
            calcs.insert(resistance_calc_name, Box::new(resistance_calc));
            calcs.insert(temperature_calc_name, Box::new(temperature_calc));
        }

        //   Cold junction RTD
        let i = 0;
        let input_name = format!("{name}.ain{i}");
        let resistance_calc_name = format!("{name}_rtd_cj_resistance_ohm");
        let temperature_calc_name = format!("{name}_rtd_cj_temp_K");
        // v_sensed = 250e-6 amps * r_sensed * 25.7
        // => r_sensed = v_sensed / (250e-6 * 25.7)
        let slope = 250e-6 * 25.7;
        let resistance_calc = InverseAffine::new(input_name, slope, 0.0, true);
        let temperature_calc = RtdPt100::new(format!("{resistance_calc_name}.y"), true);
        calcs.insert(resistance_calc_name, Box::new(resistance_calc));
        calcs.insert(temperature_calc_name.clone(), Box::new(temperature_calc));

        let board_temp_name = temperature_calc_name;

        // TCs use a gain of 25.7 as well, and an output offset
        // (implemented as the amplifier's reference voltage)
        // of 2.5 * 2 / 12 in order to handle negative TC probe voltages.
        //
        // Ideally, we'd use a real cold-junction correction here, but on this particular
        // design, the on-board temperature does not work.
        // However, the constant value can be swapped out for an RTD measurement or such if needed.
        //
        // Similarly, the offset voltage was intended to be 0.4166667 V, but the amps
        // draw too much current, and pull the offset reference rail down to 0.262V
        // at the test-point.
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
                format!("{board_temp_name}.temperature_K"),
                true,
            );
            calcs.insert(voltage_calc_name, Box::new(voltage_calc));
            calcs.insert(temperature_calc_name, Box::new(temperature_calc));
        }

        calcs
    }
}
