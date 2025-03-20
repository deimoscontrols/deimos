//! A lookup-table state machine that follows a set procedure during
//! each state, and transitions between states based on set criteria.
//!
//! Unlike most calcs, the names of the inputs and outputs of this calc
//! are not known at compile-time, and are assembled from inputs instead.

use core::f64;
use std::collections::HashSet;

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
}

/// Interpolation method
#[derive(Default)]
#[cfg_attr(feature = "ser", derive(Serialize, Deserialize))]
#[non_exhaustive]
pub enum Method {
    Linear,

    /// Hold-left is the default because intermediate values may not be
    /// valid values in some cases, while the values at the control points
    /// are valid to the extent that the user's intent is valid.
    #[default]
    Left,
    Right,
    Nearest,
}

#[derive(Default)]
#[cfg_attr(feature = "ser", derive(Serialize, Deserialize))]
pub struct State {
    // input_names: Vec<String>,
    output_names: Vec<String>,

    // Interpolation
    /// Grid to interpolate on
    time_s: Vec<f64>,

    /// Values to interpolate
    vals: Vec<(Method, Vec<f64>)>,

    // Transition criteria
    transitions: BTreeMap<String, Transition>,
    timeout: Timeout,
}

impl State {
    fn get_start_time_s(&self) -> f64 {
        return self.time_s[0]
    }

    fn get_input_names(&self) -> Vec<CalcInputName> {
        let mut names = HashSet::new();
        for t in self.transitions.values() {
            match t {
                Transition::Thresh(n, _, _) => {
                    names.insert(n.clone());
                }
                _ => {}
            }
        }

        names.iter().cloned().collect()
    }

    // fn permute(&mut self, output_names: Vec<String>) {}
    
    fn eval(&self, state_time_s: f64, output_indices: &[usize], tape: &mut [f64]) {
        for (i, (method, v)) in output_indices.iter().zip(self.vals.iter()) {
            let grid = RectilinearGrid1D::new(&self.time_s, &v).unwrap();
            let v = match method {
                Method::Linear => {
                    interpn::Linear1D::new(grid).eval_one(state_time_s).unwrap()
                },
                Method::Left => {
                    interpn::Left1D::new(grid).eval_one(state_time_s).unwrap()
                },
                Method::Right => {
                    interpn::Right1D::new(grid).eval_one(state_time_s).unwrap()
                },
                Method::Nearest => {
                    interpn::Nearest1D::new(grid).eval_one(state_time_s).unwrap()
                }
            };

            tape[*i] = v;
        }
        
    }
}

#[derive(Default)]
struct ExecutionState {
    pub state_time_s: f64,
    pub current_state: String,
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
    states: BTreeMap<String, State>,

    // Current execution state
    #[cfg_attr(feature = "ser", serde(skip))]
    execution_state: ExecutionState,

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
        // Reset execution state
        self.terminate();

        // Set per-run config
        self.input_indices = input_indices;
        self.output_range = output_range;

        // TODO: check that all outputs are consistent
        // TODO: set up eval order for transitions for each state
        // TODO: check that entrypoint is a real state that exists
    }

    fn terminate(&mut self) {
        self.input_indices.clear();
        self.output_range = usize::MAX..usize::MAX;
        let start_time = self.states.get(&self.entry).unwrap().get_start_time_s();
        self.execution_state = ExecutionState {
            state_time_s: start_time,
            current_state: self.entry.clone(),
        };
    }

    /// Run calcs for a cycle
    fn eval(&mut self, tape: &mut [f64]) {}

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

    /// Inputs are the sum of all inputs required by any state
    fn get_input_names(&self) -> Vec<CalcInputName> {
        let mut names = HashSet::new();
        for s in self.states.values() {
            for n in s.get_input_names() {
                names.insert(n);
            }
        }

        names.iter().cloned().collect()
    }

    /// All states have the same outputs
    fn get_output_names(&self) -> Vec<CalcOutputName> {
        self.states.get(&self.execution_state.current_state).unwrap().output_names.clone()
    }

    calc_config!();
}
