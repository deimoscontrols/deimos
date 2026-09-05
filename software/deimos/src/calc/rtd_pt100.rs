//! Pt100 calc wrapper around the allocation-free shared IEC 60751 implementation.
//!
//! # References
//!
//! \[1\] IEC 60751, *Industrial platinum resistance thermometers and platinum
//!     temperature sensors*.

#[cfg(feature = "python")]
use pyo3::prelude::*;

use super::*;
use crate::{calc_names, py_json_methods};

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
}

impl RtdPt100 {
    /// Builds an uninitialized Pt100 calc node.
    ///
    /// Args:
    ///   resistance_name: Calc-graph field containing resistance in `ohm`.
    /// Returns:
    ///   Boxed calc configuration.
    pub fn new(resistance_name: String) -> Box<Self> {
        Box::new(Self { resistance_name })
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
    fn init(&self, _: ControllerCtx) -> Result<CalcFn, String> {
        Ok(Box::new(|inputs, outputs| {
            outputs[0] = pt100_temp_k(inputs[0]);
            Ok(())
        }))
    }

    fn input_map(&self) -> BTreeMap<CalcInputName, FieldName> {
        BTreeMap::from([("resistance_ohm".to_owned(), self.resistance_name.clone())])
    }

    calc_names!((resistance_ohm), (temperature_K));
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
