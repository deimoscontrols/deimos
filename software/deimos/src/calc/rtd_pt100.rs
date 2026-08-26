//! Pt100 calc wrapper around the allocation-free shared IEC 60751 implementation.
//!
//! # References
//!
//! \[1\] IEC 60751, *Industrial platinum resistance thermometers and platinum
//!     temperature sensors*.

#[cfg(feature = "python")]
use pyo3::prelude::*;

use super::*;
use crate::{calc_input_names, calc_output_names, py_json_methods};

pub use deimos_shared::calcs::{pt100_resistance_ohm_f32, pt100_temperature_k_f32};

/// Converts Pt100 resistance to temperature through the shared `f32` evaluator.
///
/// Args:
///   resistance_ohm: Pt100 resistance scalar in `ohm`, rounded to `f32` before
///     evaluation.
///
/// Returns:
///   Absolute temperature scalar in `K`, exactly upcast from the shared `f32`
///   result.
#[inline]
pub fn pt100_temp_k(resistance_ohm: f64) -> f64 {
    pt100_temperature_k_f32(resistance_ohm as f32) as f64
}

/// Converts Pt100 temperature to resistance through the shared `f32` evaluator.
///
/// Args:
///   temperature_k: Absolute temperature scalar in `K`, rounded to `f32` before
///     evaluation.
///
/// Returns:
///   Pt100 resistance scalar in `ohm`, exactly upcast from the shared `f32`
///   result.
#[inline]
pub fn pt100_resistance_ohm(temperature_k: f64) -> f64 {
    pt100_resistance_ohm_f32(temperature_k as f32) as f64
}

#[cfg_attr(feature = "python", pyclass)]
#[derive(Serialize, Deserialize, Default, Debug)]
/// Calc-graph node that maps one Pt100 resistance input to temperature.
pub struct RtdPt100 {
    resistance_name: String,
    #[serde(skip)]
    input_index: usize,
    #[serde(skip)]
    output_index: usize,
}

impl RtdPt100 {
    /// Builds an uninitialized Pt100 calc node.
    ///
    /// Args:
    ///   resistance_name: Calc-graph field containing resistance in `ohm`.
    /// Returns:
    ///   Boxed calc node; graph indices are assigned during `Calc::init`.
    pub fn new(resistance_name: String) -> Box<Self> {
        Box::new(Self {
            resistance_name,
            input_index: usize::MAX,
            output_index: usize::MAX,
        })
    }
}

py_json_methods!(
    RtdPt100,
    Calc,
    #[new]
    fn py_new(resistance_name: String) -> Self {
        *Self::new(resistance_name)
    }
);

#[typetag::serde]
impl Calc for RtdPt100 {
    fn init(
        &mut self,
        _: ControllerCtx,
        input_indices: Vec<usize>,
        output_range: Range<usize>,
    ) -> Result<(), String> {
        self.input_index = input_indices[0];
        self.output_index = output_range
            .into_iter()
            .next()
            .ok_or_else(|| "RtdPt100 requires one output".to_owned())?;
        Ok(())
    }

    fn terminate(&mut self) -> Result<(), String> {
        self.input_index = usize::MAX;
        self.output_index = usize::MAX;
        Ok(())
    }

    fn eval(&mut self, tape: &mut [f64]) -> Result<(), String> {
        tape[self.output_index] = pt100_temp_k(tape[self.input_index]);
        Ok(())
    }

    fn get_input_map(&self) -> BTreeMap<CalcInputName, FieldName> {
        BTreeMap::from([("resistance_ohm".to_owned(), self.resistance_name.clone())])
    }

    fn update_input_map(&mut self, field: &str, source: &str) -> Result<(), String> {
        if field != "resistance_ohm" {
            return Err(format!("Unrecognized field {field}"));
        }
        self.resistance_name = source.to_owned();
        Ok(())
    }

    fn get_output_units(&self) -> Vec<Option<String>> {
        vec![Some("K".to_owned())]
    }

    calc_input_names!(resistance_ohm);
    calc_output_names!(temperature_K);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn software_wrappers_are_exact_upcasts_of_shared_results() {
        for temperature_c in (-200..=850).step_by(5) {
            let temperature_k = temperature_c as f64 + 273.15;
            let resistance = pt100_resistance_ohm(temperature_k);
            assert_eq!(
                resistance,
                pt100_resistance_ohm_f32(temperature_k as f32) as f64
            );
            assert_eq!(
                pt100_temp_k(resistance),
                pt100_temperature_k_f32(resistance as f32) as f64
            );
        }
    }
}
