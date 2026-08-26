//! A slope and offset, y = ax + b

#[cfg(feature = "python")]
use pyo3::prelude::*;

use super::*;
use crate::{calc_names, py_json_methods};

/// A slope and offset, y = ax + b
#[derive(Default, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "python", pyclass)]
pub struct Affine {
    // User inputs
    input_name: String,
    slope: f64,
    offset: f64,
}

impl Affine {
    pub fn new(input_name: String, slope: f64, offset: f64) -> Box<Self> {
        Box::new(Self {
            input_name,
            slope,
            offset,
        })
    }
}

py_json_methods!(
    Affine,
    Calc,
    #[new]
    fn py_new(input_name: String, slope: f64, offset: f64) -> Self {
        *Self::new(input_name, slope, offset)
    }
);

#[typetag::serde]
impl Calc for Affine {
    fn init(&self, _: ControllerCtx) -> Result<CalcFn, String> {
        let slope = self.slope;
        let offset = self.offset;
        Ok(Box::new(move |inputs, outputs| {
            outputs[0] = slope * inputs[0] + offset;
            Ok(())
        }))
    }

    /// Map from input field names (like `v`, without prefix) to the state name
    /// that the input should draw from (like `peripheral_0.output_1`, with prefix)
    fn input_map(&self) -> BTreeMap<CalcInputName, FieldName> {
        BTreeMap::from([("x".to_owned(), self.input_name.clone())])
    }

    calc_names!((x), (y));
}
