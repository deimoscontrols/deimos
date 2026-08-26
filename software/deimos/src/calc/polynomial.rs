//! Evaluate an Nth order polynomial calibration curve.

#[cfg(feature = "python")]
use pyo3::prelude::*;

use super::*;
use crate::{
    calc_names,
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
}

impl Polynomial {
    pub fn new(input_name: String, coefficients: Vec<f64>, note: String) -> Box<Self> {
        Box::new(Self {
            input_name,
            coefficients,
            note,
        })
    }

    pub fn fit_from_points(
        input_name: &str,
        points: &[(f64, f64)],
        order: usize,
        note: &str,
    ) -> Result<Box<Self>, String> {
        let coefficients = polyfit(points, order)?;

        Ok(Self::new(input_name.into(), coefficients, note.into()))
    }
}

py_json_methods!(
    Polynomial,
    Calc,
    #[new]
    fn py_new(input_name: String, coefficients: Vec<f64>, note: String) -> Self {
        *Self::new(input_name, coefficients, note)
    }
);

#[typetag::serde]
impl Calc for Polynomial {
    fn init(&self, _: ControllerCtx) -> Result<CalcFn, String> {
        if self.coefficients.is_empty() {
            return Err("Polynomial coefficients cannot be empty".to_string());
        }
        let coefficients = self.coefficients.clone();
        Ok(Box::new(move |inputs, outputs| {
            outputs[0] = polyval(inputs[0], &coefficients);
            Ok(())
        }))
    }

    fn input_map(&self) -> BTreeMap<CalcInputName, FieldName> {
        BTreeMap::from([("x".to_owned(), self.input_name.clone())])
    }

    calc_names!((x), (y));
}
