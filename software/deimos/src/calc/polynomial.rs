//! Evaluate an Nth order polynomial calibration curve.

#[cfg(feature = "python")]
use pyo3::prelude::*;

use super::*;
use crate::{
    calc_config, calc_input_names, calc_output_names,
    math::{polyfit, polyval},
    py_json_methods,
};

/// Polynomial calibration: y = c0 + c1*x + c2*x^2 + ...
/// with an attached note that should include traceability info
/// like a sensor serial number.
/// Coefficients ordered by increasing polynomial order.
#[cfg_attr(feature = "python", pyclass)]
#[derive(Serialize, Deserialize, Debug, Default)]
pub struct Polynomial {
    // User inputs
    input_name: String,
    coefficients: Vec<f64>,
    note: String,
    save_outputs: bool,

    // Values provided by calc orchestrator during init
    #[serde(skip)]
    input_index: usize,

    #[serde(skip)]
    output_index: usize,
}

impl Polynomial {
    pub fn new(
        input_name: String,
        coefficients: Vec<f64>,
        note: String,
        save_outputs: bool,
    ) -> Self {
        Self {
            input_name,
            coefficients,
            note,
            save_outputs,
            input_index: usize::MAX,
            output_index: usize::MAX,
        }
    }

    pub fn fit_from_points(
        input_name: &str,
        points: &[(f64, f64)],
        order: usize,
        note: &str,
        save_outputs: bool,
    ) -> Result<Self, String> {
        let coefficients = polyfit(points, order)?;

        Ok(Self::new(
            input_name.into(),
            coefficients,
            note.into(),
            save_outputs,
        ))
    }
}

py_json_methods!(
    Polynomial,
    Calc,
    #[new]
    fn py_new(
        input_name: String,
        coefficients: Vec<f64>,
        note: String,
        save_outputs: bool,
    ) -> Self {
        Self::new(input_name, coefficients, note, save_outputs)
    }
);

#[typetag::serde]
impl Calc for Polynomial {
    fn init(
        &mut self,
        _: ControllerCtx,
        input_indices: Vec<usize>,
        output_range: Range<usize>,
    ) -> Result<(), String> {
        if self.coefficients.is_empty() {
            return Err("Polynomial coefficients cannot be empty".to_string());
        }
        self.input_index = input_indices
            .first()
            .copied()
            .ok_or_else(|| "Polynomial calc missing input index".to_string())?;
        self.output_index = output_range
            .clone()
            .next()
            .ok_or_else(|| "Polynomial calc missing output index".to_string())?;
        Ok(())
    }

    fn terminate(&mut self) -> Result<(), String> {
        self.input_index = usize::MAX;
        self.output_index = usize::MAX;
        Ok(())
    }

    fn eval(&mut self, tape: &mut [f64]) -> Result<(), String> {
        let x = tape[self.input_index];
        let y = polyval(x, &self.coefficients);
        tape[self.output_index] = y;
        Ok(())
    }

    fn get_input_map(&self) -> BTreeMap<CalcInputName, FieldName> {
        let mut map = BTreeMap::new();
        map.insert("x".to_owned(), self.input_name.clone());
        map
    }

    fn update_input_map(&mut self, field: &str, source: &str) -> Result<(), String> {
        if field == "x" {
            self.input_name = source.to_owned();
            Ok(())
        } else {
            Err(format!("Unrecognized field {field}"))
        }
    }

    calc_config!();
    calc_input_names!(x);
    calc_output_names!(y);
}
