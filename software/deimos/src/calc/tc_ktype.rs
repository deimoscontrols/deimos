//! Type-K thermocouple calc wrapper around the shared `f32` spline implementation.
//!
//! # References
//!
//! \[1\] G. W. Burns, M. G. Scroger, G. F. Strouse, M. C. Croarkin, and
//!     W. F. Guthrie, *Temperature-Electromotive Force Reference Functions and
//!     Tables for the Letter-Designated Thermocouple Types Based on the ITS-90*,
//!     NIST Monograph 175, 1993, doi: 10.6028/NIST.MONO.175.

#[cfg(feature = "python")]
use pyo3::prelude::*;

use super::*;
use crate::{calc_input_names, calc_output_names, calc_save_outputs, py_json_methods};

pub use deimos_shared::calcs::{
    ktype_corrected_temperature_k_f32, ktype_temperature_k_f32, ktype_voltage_v_f32,
};

/// Applies type-K cold-junction compensation through the shared `f32` evaluator.
///
/// Args:
///   sensed_voltage_v: Measured hot-to-cold-junction voltage scalar in `V`,
///     rounded to `f32` before evaluation.
///   cold_junction_temperature_k: Absolute connector temperature scalar in `K`,
///     rounded to `f32` before evaluation.
///
/// Returns:
///   Compensated absolute hot-junction temperature scalar in `K`, exactly
///   upcast from the shared `f32` result.
#[inline]
pub fn ktype_corrected_temp_k(sensed_voltage_v: f64, cold_junction_temperature_k: f64) -> f64 {
    ktype_corrected_temperature_k_f32(sensed_voltage_v as f32, cold_junction_temperature_k as f32)
        as f64
}

/// Converts type-K temperature to NIST-equivalent voltage through the shared evaluator.
///
/// Args:
///   temperature_k: Absolute junction temperature scalar in `K`, rounded to
///     `f32` before evaluation.
///
/// Returns:
///   Type-K equivalent electromotive-force scalar in `V`, exactly upcast from
///   the shared `f32` result.
#[inline]
pub fn ktype_voltage_v(temperature_k: f64) -> f64 {
    ktype_voltage_v_f32(temperature_k as f32) as f64
}

/// Converts type-K equivalent voltage to temperature through the shared evaluator.
///
/// Args:
///   voltage_v: Type-K equivalent electromotive-force scalar in `V`, rounded
///     to `f32` before evaluation.
///
/// Returns:
///   Absolute junction temperature scalar in `K`, exactly upcast from the
///   shared `f32` result.
#[inline]
pub fn ktype_temp_k(voltage_v: f64) -> f64 {
    ktype_temperature_k_f32(voltage_v as f32) as f64
}

#[cfg_attr(feature = "python", pyclass)]
#[derive(Serialize, Deserialize, Default, Debug)]
/// Calc-graph node that applies type-K cold-junction compensation.
pub struct TcKtype {
    voltage_name: String,
    cold_junction_temperature_name: String,
    save_outputs: bool,
    #[serde(skip)]
    input_indices: Vec<usize>,
    #[serde(skip)]
    output_index: usize,
}

impl TcKtype {
    /// Builds an uninitialized type-K calc node.
    ///
    /// Args:
    ///   voltage_name: Calc-graph field containing sensed voltage in `V`.
    ///   cold_junction_temperature_name: Calc-graph field containing absolute
    ///     cold-junction temperature in `K`.
    ///   save_outputs: Whether to retain the calculated temperature in recorded
    ///     controller output.
    ///
    /// Returns:
    ///   Boxed calc node; graph indices are assigned during `Calc::init`.
    pub fn new(
        voltage_name: String,
        cold_junction_temperature_name: String,
        save_outputs: bool,
    ) -> Box<Self> {
        Box::new(Self {
            voltage_name,
            cold_junction_temperature_name,
            save_outputs,
            input_indices: Vec::new(),
            output_index: usize::MAX,
        })
    }
}

py_json_methods!(
    TcKtype,
    Calc,
    #[new]
    fn py_new(
        voltage_name: String,
        cold_junction_temperature_name: String,
        save_outputs: bool,
    ) -> Self {
        *Self::new(voltage_name, cold_junction_temperature_name, save_outputs)
    }
);

#[typetag::serde]
impl Calc for TcKtype {
    fn init(
        &mut self,
        _: ControllerCtx,
        input_indices: Vec<usize>,
        output_range: Range<usize>,
    ) -> Result<(), String> {
        self.input_indices = input_indices;
        self.output_index = output_range
            .into_iter()
            .next()
            .ok_or_else(|| "TcKtype requires one output".to_owned())?;
        Ok(())
    }

    fn terminate(&mut self) -> Result<(), String> {
        self.input_indices.clear();
        self.output_index = usize::MAX;
        Ok(())
    }

    fn eval(&mut self, tape: &mut [f64]) -> Result<(), String> {
        tape[self.output_index] =
            ktype_corrected_temp_k(tape[self.input_indices[0]], tape[self.input_indices[1]]);
        Ok(())
    }

    fn get_input_map(&self) -> BTreeMap<CalcInputName, FieldName> {
        BTreeMap::from([
            ("voltage_V".to_owned(), self.voltage_name.clone()),
            (
                "cold_junction_temperature_K".to_owned(),
                self.cold_junction_temperature_name.clone(),
            ),
        ])
    }

    fn update_input_map(&mut self, field: &str, source: &str) -> Result<(), String> {
        match field {
            "voltage_V" => self.voltage_name = source.to_owned(),
            "cold_junction_temperature_K" => {
                self.cold_junction_temperature_name = source.to_owned()
            }
            _ => return Err(format!("Unrecognized field {field}")),
        }
        Ok(())
    }

    fn get_output_units(&self) -> Vec<Option<String>> {
        vec![Some("K".to_owned())]
    }

    calc_save_outputs!();
    calc_input_names!(voltage_V, cold_junction_temperature_K);
    calc_output_names!(temperature_K);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn software_wrappers_are_exact_upcasts() {
        for temperature_c in (-210..=1370).step_by(10) {
            let temperature_k = temperature_c as f64 + 273.15;
            let voltage = ktype_voltage_v(temperature_k);
            assert_eq!(voltage, ktype_voltage_v_f32(temperature_k as f32) as f64);
            assert_eq!(
                ktype_temp_k(voltage),
                ktype_temperature_k_f32(voltage as f32) as f64
            );
        }
    }

    #[test]
    fn cold_junction_correction_roundtrips() {
        let hot = 373.15;
        let cold = 293.15;
        let sensed = ktype_voltage_v(hot) - ktype_voltage_v(cold);
        assert!((ktype_corrected_temp_k(sensed, cold) - hot).abs() < 0.02);
    }
}
