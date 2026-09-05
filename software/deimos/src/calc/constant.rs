//! A calc that produces a constant value

use super::*;
use crate::{calc_names, py_json_methods};

#[cfg(feature = "python")]
use pyo3::prelude::*;

/// Simplest calc that does anything at all
#[derive(Serialize, Deserialize, Default, Debug)]
#[cfg_attr(feature = "python", pyo3::prelude::pyclass)]
pub struct Constant {
    y: f64,
}

impl Constant {
    pub fn new(y: f64) -> Box<Self> {
        Box::new(Self { y })
    }
}

py_json_methods!(
    Constant,
    Calc,
    #[new]
    fn py_new(y: f64) -> Self {
        *Self::new(y)
    }
);

#[typetag::serde]
impl Calc for Constant {
    fn init(&self, _: ControllerCtx) -> Result<CalcFn, String> {
        let y = self.y;
        Ok(Box::new(move |_, outputs| {
            outputs[0] = y;
            Ok(())
        }))
    }

    /// Map from input field names (like `v`, without prefix) to the state name
    /// that the input should draw from (like `peripheral_0.output_1`, with prefix)
    fn input_map(&self) -> BTreeMap<CalcInputName, FieldName> {
        BTreeMap::new()
    }

    calc_names!((), (y));
}
