//! Derive input voltage from linear amplifier reading

use super::*;
use crate::{calc_config, calc_input_names, calc_output_names};

/// Derive input voltage from linear amplifier reading
///
/// First subtracts the output offset, then divides by the slope.
#[cfg_attr(feature = "ser", derive(Serialize, Deserialize))]
#[derive(Default, Debug)]
pub struct InverseAffine {
    // User inputs
    input_name: String,
    slope: f64,
    offset: f64,
    save_outputs: bool,

    // Values provided by calc orchestrator during init
    #[cfg_attr(feature = "ser", serde(skip))]
    input_index: usize,

    #[cfg_attr(feature = "ser", serde(skip))]
    output_index: usize,
}

impl InverseAffine {
    pub fn new(input_name: String, slope: f64, offset: f64, save_outputs: bool) -> Self {
        // These will be set during init.
        // Use default indices that will cause an error on the first call if not initialized properly
        let input_index = usize::MAX;
        let output_index = usize::MAX;

        Self {
            input_name,
            slope,
            offset,
            save_outputs,

            input_index,
            output_index,
        }
    }
}

#[cfg_attr(feature = "ser", typetag::serde)]
impl Calc for InverseAffine {
    /// Reset internal state and register calc tape indices
    fn init(&mut self, _: ControllerCtx, input_indices: Vec<usize>, output_range: Range<usize>) {
        self.input_index = input_indices[0];
        self.output_index = output_range.clone().next().unwrap();
    }

    fn terminate(&mut self) {
        self.input_index = usize::MAX;
        self.output_index = usize::MAX;
    }

    /// Run calcs for a cycle
    fn eval(&mut self, tape: &mut [f64]) {
        let x = tape[self.input_index];
        let y = (x - self.offset) / self.slope;

        tape[self.output_index] = y;
    }

    /// Map from input field names (like `v`, without prefix) to the state name
    /// that the input should draw from (like `peripheral_0.output_1`, with prefix)
    fn get_input_map(&self) -> BTreeMap<CalcInputName, FieldName> {
        let mut map = BTreeMap::new();
        map.insert("x".to_owned(), self.input_name.clone());
        map
    }

    /// Change a value in the input map
    fn update_input_map(&mut self, field: &str, source: &str) -> Result<(), String> {
        if field == "x" {
            self.input_name = source.to_owned();
        } else {
            return Err(format!("Unrecognized field {field}"));
        }

        Ok(())
    }

    calc_config!(slope, offset);
    calc_input_names!(x);
    calc_output_names!(y);
}
