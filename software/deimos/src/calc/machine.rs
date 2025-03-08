//! A lookup-table state machine that follows a set procedure during
//! each state, and transitions between states based on set criteria.
//!
//! Unlike most calcs, the names of the inputs and outputs of this calc
//! are not known at compile-time, and are assembled from inputs instead.

use core::f64;

use interpn::one_dim::{Interp1D, RectilinearGrid1D};

use super::*;
use crate::{calc_config, calc_input_names, calc_output_names};

/// Choice of behavior when a given state reaches the end of its lookup table
#[derive(Default)]
#[cfg_attr(feature = "ser", derive(Serialize, Deserialize))]
#[non_exhaustive]
pub enum Timeout {
    /// Transition to the next state
    Transition(String),

    /// Start over from the beginning of the table
    Loop,

    /// Raise an error
    #[default]
    Error,
}

#[derive(Default)]
#[cfg_attr(feature = "ser", derive(Serialize, Deserialize))]
pub enum ThreshOp {
    /// Greater than
    #[default]
    Gt,

    /// Less than
    Lt,

    /// Greater than or equal
    Ge,

    /// Less than or equal
    Le,

    /// Equal
    Eq,

    /// Approximately equal
    Approx { rtol: f64, atol: f64 },
}

#[cfg_attr(feature = "ser", derive(Serialize, Deserialize))]
#[non_exhaustive]
pub enum Transition {
    /// Transition if a value of some input exceeds a threshold value
    /// based on some choice of comparison operation.
    /// 
    /// This may be used, for example, to exit when overheating is detected,
    /// or to wait until a controlled parameter has converged to a value
    /// before proceeding into the next part of an operation.
    Thresh(String, ThreshOp, f64),

    /// Time out to a given state if sequence time (including previous timeout loops)
    /// exceeds some value.
    Timeout(String, f64),
}

impl Default for Transition {
    fn default() -> Self {
        Self::Timeout("".to_owned(), f64::INFINITY)
    }
}

/// Interpolation method
#[derive(Default)]
#[cfg_attr(feature = "ser", derive(Serialize, Deserialize))]
#[non_exhaustive]
pub enum Method {
    Linear,
    #[default]
    Left,
    Right,
    Nearest
}

#[derive(Default)]
#[cfg_attr(feature = "ser", derive(Serialize, Deserialize))]
pub struct State {
    name: String,
    input_names: Vec<String>,
    output_names: Vec<String>,

    // Interpolation
    /// Grid to interpolate on
    time_s: Vec<f64>,
    /// Values to interpolate
    vals: Vec<(Method, Vec<f64>)>,

    // Transition criteria

    timeout: Timeout,
    transitions: BTreeMap<String, Transition>,
}

/// A lookup-table state machine that follows a set procedure during
/// each state, and transitions between states based on set criteria.
///
/// Unlike most calcs, the names of the inputs and outputs of this calc
/// are not known at compile-time, and are assembled from inputs instead.
#[derive(Default)]
#[cfg_attr(feature = "ser", derive(Serialize, Deserialize))]
pub struct Machine {
    // User inputs
    save_outputs: bool,

    /// State which is the entrypoint for the machine
    entry: String,

    /// All the lookup states of the machine, including their
    /// transition criteria.
    /// 
    /// All states must have the same outputs so that no values
    /// are ever left dangling.
    /// 
    /// The inputs to the machine are the sum of all the inputs
    /// required by each state.
    states: Vec<State>,

    // Values provided by calc orchestrator during init
    #[cfg_attr(feature = "ser", serde(skip))]
    input_indices: Vec<usize>,

    #[cfg_attr(feature = "ser", serde(skip))]
    output_range: Range<usize>,
}

impl Machine {
    // pub fn new(save_outputs: bool) -> Self {
    //     // These will be set during init.
    //     // Use default indices that will cause an error on the first call if not initialized properly
    //     let input_indices = Vec::new();
    //     let output_indices = Vec::new();

    //     Self {
    //         input_name,
    //         slope,
    //         offset,
    //         save_outputs,

    //         input_index,
    //         output_index,
    //     }
    // }
}

#[cfg_attr(feature = "ser", typetag::serde)]
impl Calc for Machine {
    /// Reset internal state and register calc tape indices
    fn init(&mut self, _: ControllerCtx, input_indices: Vec<usize>, output_range: Range<usize>) {
        self.input_indices = input_indices;
        self.output_range = output_range;
    }

    fn terminate(&mut self) {
        self.input_indices.clear();
        self.output_range = usize::MAX..usize::MAX;
    }

    /// Run calcs for a cycle
    fn eval(&mut self, tape: &mut [f64]) {
    }

    /// Map from input field names (like `v`, without prefix) to the state name
    /// that the input should draw from (like `peripheral_0.output_1`, with prefix)
    fn get_input_map(&self) -> BTreeMap<CalcInputName, FieldName> {
        let mut map = BTreeMap::new();
        
        map
    }

    /// Change a value in the input map
    fn update_input_map(&mut self, _field: &str, _source: &str) -> Result<(), String> {
        
        return Err(format!("Machine input map does not support direct updates"));
    }

    fn get_input_names(&self) -> Vec<CalcInputName> {
        
    }

    fn get_output_names(&self) -> Vec<CalcOutputName> {
        
    }

    calc_config!();
}
