//! A calc that produces a constant value

use super::*;
use crate::{calc_config, calc_input_names, calc_output_names};

#[cfg(feature = "python")]
use pyo3::prelude::*;

/// Simplest calc that does anything at all
#[derive(Serialize, Deserialize, Default, Debug)]
#[cfg_attr(feature = "python", pyo3::prelude::pyclass)]
pub struct Constant {
    // User inputs
    y: f64,
    save_outputs: bool,

    // Values provided by calc orchestrator during init
    #[serde(skip)]
    output_index: usize,
}

impl Constant {
    pub fn new(y: f64, save_outputs: bool) -> Self {
        // Use default indices that will cause an error on the first call if not initialized properly
        let output_index = usize::MAX;

        Self {
            y,
            save_outputs,
            output_index,
        }
    }
}

#[cfg(feature = "python")]
#[pymethods]
impl Constant {
    #[new]
    fn py_new(y: f64, save_outputs: bool) -> PyResult<Self> {
        Ok(Self::new(y, save_outputs))
    }

    /// Serialize to a JSON string
    fn to_json(&self) -> PyResult<String> {
        // Serialize as Trait object to capture type tag,
        // otherwise the type gets erased
        let calc: &dyn Calc = self;
        serde_json::to_string_pretty(calc).map_err(|e| {
            crate::python::BackendErr::InvalidCalcErr {
                msg: format!("Unable to serialize Calc object: {e}"),
            }
            .into()
        })
    }

    /// Deserialize from JSON string
    #[classmethod]
    fn from_json(_cls: &Bound<'_, pyo3::types::PyType>, s: &str) -> PyResult<Self> {
        serde_json::from_str::<Self>(s).map_err(|e| {
            crate::python::BackendErr::InvalidCalcErr {
                msg: format!("Unable to parse Calc object: {e}"),
            }
            .into()
        })
    }
}

#[typetag::serde]
impl Calc for Constant {
    /// Reset internal state and register calc tape indices
    fn init(
        &mut self,
        _: ControllerCtx,
        _: Vec<usize>,
        output_range: Range<usize>,
    ) -> Result<(), String> {
        self.output_index = output_range.clone().next().unwrap();
        Ok(())
    }

    fn terminate(&mut self) -> Result<(), String> {
        self.output_index = usize::MAX;
        Ok(())
    }

    /// Run calcs for a cycle
    fn eval(&mut self, tape: &mut [f64]) -> Result<(), String> {
        tape[self.output_index] = self.y;
        Ok(())
    }

    /// Map from input field names (like `v`, without prefix) to the state name
    /// that the input should draw from (like `peripheral_0.output_1`, with prefix)
    fn get_input_map(&self) -> BTreeMap<CalcInputName, FieldName> {
        BTreeMap::new()
    }

    /// Change a value in the input map
    fn update_input_map(&mut self, field: &str, _: &str) -> Result<(), String> {
        Err(format!("Unrecognized field {field}"))
    }

    calc_config!(y);
    calc_input_names!();
    calc_output_names!(y);
}
