/// Glue for passing Deimos trait objects between Python and Rust via json.
///
/// These are never used in-the-loop, so they do not need to be exceptionally
/// performant, only consistent and ergonomic.
use pyo3::prelude::*;

use crate::{calc::Calc, dispatcher::Dispatcher, peripheral::Peripheral, python::BackendErr};

macro_rules! impl_from_pyobject_json {
    ($trait:path, $err_variant:ident, $type_name:expr) => {
        /// Glue for passing dyn Trait objects from Python to Rust via json
        /// to handle the fact that they are concrete types on the Python side,
        /// but dyn Trait objects on the Rust side.
        impl<'a, 'py> FromPyObject<'a, 'py> for Box<dyn $trait> {
            type Error = PyErr;

            fn extract(obj: Borrowed<'a, 'py, PyAny>) -> Result<Self, Self::Error> {
                // Require a to_json() method on the Python instance
                let to_json = obj.getattr("to_json").map_err(|e| BackendErr::$err_variant {
                    msg: format!("{} object missing to_json(): {e}", $type_name),
                })?;
                if !to_json.is_callable() {
                    return Err(
                        BackendErr::$err_variant {
                            msg: format!("{}.to_json is not callable", $type_name),
                        }
                        .into(),
                    );
                }

                let json_any = to_json.call0().map_err(|e| BackendErr::$err_variant {
                    msg: format!("{}.to_json() failed: {e}", $type_name),
                })?;
                let json_str: String = json_any.extract().map_err(|e| BackendErr::$err_variant {
                    msg: format!("{}.to_json() did not return a string: {e}", $type_name),
                })?;

                serde_json::from_str(&json_str).map_err(|e| {
                    BackendErr::$err_variant {
                        msg: format!("Unable to process {} object: {e}", $type_name),
                    }
                    .into()
                })
            }
        }
    };
}

impl_from_pyobject_json!(Calc, InvalidCalcErr, "Calc");
impl_from_pyobject_json!(Dispatcher, InvalidDispatcherErr, "Dispatcher");
impl_from_pyobject_json!(Peripheral, InvalidPeripheralErr, "Peripheral");
