use pyo3::prelude::*;

use crate::{calc::Calc, python::BackendErr};

/// Glue for passing Calc objects from Python to Rust via json
/// to handle the fact that they are concrete types on the Python side,
/// but dyn Trait objects on the Rust side.
impl<'a, 'py> FromPyObject<'a, 'py> for Box<dyn Calc> {
    type Error = PyErr;

    fn extract(obj: Borrowed<'a, 'py, PyAny>) -> Result<Self, Self::Error> {
        // Require a to_json() method on the Python calc instance
        let to_json = obj
            .getattr("to_json")
            .map_err(|e| BackendErr::InvalidCalcErr {
                msg: format!("Calc object missing to_json(): {e}"),
            })?;
        if !to_json.is_callable() {
            return Err(BackendErr::InvalidCalcErr {
                msg: "Calc.to_json is not callable".into(),
            }
            .into());
        }

        let json_any = to_json
            .call0()
            .map_err(|e| BackendErr::InvalidCalcErr {
                msg: format!("Calc.to_json() failed: {e}"),
            })?;
        let json_str: String = json_any
            .extract()
            .map_err(|e| BackendErr::InvalidCalcErr {
                msg: format!("Calc.to_json() did not return a string: {e}"),
            })?;

        serde_json::from_str(&json_str).map_err(|e| BackendErr::InvalidCalcErr {
            msg: format!("Unable to process Calc object: {e}"),
        }
        .into())
    }
}
