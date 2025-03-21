//! A lookup-table state machine that follows a set procedure during
//! each state, and transitions between states based on set criteria.
//!
//! Unlike most calcs, the names of the inputs and outputs of this calc
//! are not known at compile-time, and are assembled from inputs instead.

use core::f64;
use std::collections::HashSet;

use interpn::one_dim::{Interp1D, RectilinearGrid1D};

use super::*;

/// Choice of behavior when a given state reaches the end of its lookup table
#[derive(Default)]
#[cfg_attr(feature = "ser", derive(Serialize, Deserialize))]
#[non_exhaustive]
pub enum Timeout {
    /// Transition to the next state
    Transition(String),

    /// Start over from the beginning of the table
    #[default]
    Loop,

    /// Raise an error with a message
    Error(String),
}

#[derive(Default)]
#[cfg_attr(feature = "ser", derive(Serialize, Deserialize))]
pub enum ThreshOp {
    /// Greater than
    #[default]
    Gt,

    /// Less than
    Lt,

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

impl Transition {
    pub fn get_input_names(&self) -> Vec<FieldName> {
        let mut names = Vec::new();
        match self {
            Self::Thresh(name, _, _) => names.push(name.clone()),
        };

        names
    }
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
    transitions: BTreeMap<FieldName, Transition>,
    timeout: Timeout,
}

impl State {
    fn get_end_time_s(&self) -> f64 {
        return *self.time_s.last().unwrap();
    }

    fn get_start_time_s(&self) -> f64 {
        return self.time_s[0];
    }

    fn get_timeout(&self) -> &Timeout {
        &self.timeout
    }

    fn get_input_names(&self) -> Vec<CalcInputName> {
        let mut names = HashSet::new();
        for t in self.transitions.values() {
            match t {
                Transition::Thresh(n, _, _) => {
                    names.insert(n.clone());
                }
            }
        }

        names.iter().cloned().collect()
    }

    /// Shuffle internal ordering of outputs to match the provided order
    fn permute(&mut self, output_names: &Vec<String>) {
        let ind_map =
            BTreeMap::from_iter(self.output_names.iter().enumerate().map(|(i, x)| (x, i)));

        let mut new_vals = Vec::new();

        for n in output_names.iter() {
            let ind = ind_map[n];
            new_vals.push(self.vals.remove(ind));
        }

        self.vals = new_vals;
        self.output_names = output_names.clone();
    }

    /// Run the interpolators for this timestep
    fn eval(&self, sequence_time_s: f64, output_range: Range<usize>, tape: &mut [f64]) {
        for (i, (method, v)) in output_range.zip(self.vals.iter()) {
            let grid = RectilinearGrid1D::new(&self.time_s, &v).unwrap();
            let v = match method {
                Method::Linear => interpn::Linear1D::new(grid)
                    .eval_one(sequence_time_s)
                    .unwrap(),
                Method::Left => interpn::Left1D::new(grid)
                    .eval_one(sequence_time_s)
                    .unwrap(),
                Method::Right => interpn::Right1D::new(grid)
                    .eval_one(sequence_time_s)
                    .unwrap(),
                Method::Nearest => interpn::Nearest1D::new(grid)
                    .eval_one(sequence_time_s)
                    .unwrap(),
            };

            tape[i] = v;
        }
    }
}

#[derive(Default)]
struct ExecutionState {
    /// Time in current state's sequence.
    /// Starts at the first time in the state's lookup table.
    pub sequence_time_s: f64,
    pub current_state: String,
}

#[derive(Default)]
#[cfg_attr(feature = "ser", derive(Serialize, Deserialize))]
pub struct MachineCfg {
    // User inputs
    save_outputs: bool,

    /// State which is the entrypoint for the machine
    entry: String,
}

/// A lookup-table state machine that follows a set procedure during
/// each state, and transitions between states based on set criteria.
///
/// Unlike most calcs, the names of the inputs and outputs of this calc
/// are not known at compile-time, and are assembled from inputs instead.
#[derive(Default)]
#[cfg_attr(feature = "ser", derive(Serialize, Deserialize))]
pub struct Machine {
    cfg: MachineCfg,

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
    /// Lookup map for channel names to indices, required for evaluating
    /// transition criteria
    #[cfg_attr(feature = "ser", serde(skip))]
    input_map: BTreeMap<String, usize>,

    /// Timestep
    #[cfg_attr(feature = "ser", serde(skip))]
    dt_s: f64,

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

    fn current_state(&self) -> &State {
        self.states
            .get(&self.execution_state.current_state)
            .unwrap()
    }

    fn transition(&mut self, target_state: String) {
        self.execution_state.current_state = target_state;
        self.execution_state.sequence_time_s = self.current_state().get_start_time_s();
    }

    /// Check each state transition criterion and set the next state if needed.
    /// If multiple transition criteria are met at the same time, the first
    /// one in the list will be prioritized.
    fn check_transitions(&mut self, sequence_time_s: f64, tape: &[f64]) -> Result<(), String> {
        // Check for timeout
        if sequence_time_s > self.current_state().get_end_time_s() {
            return match self.current_state().get_timeout() {
                Timeout::Transition(target_state) => {
                    self.transition(target_state.clone());
                    Ok(())
                }
                Timeout::Loop => {
                    self.transition(self.execution_state.current_state.clone());
                    Ok(())
                }
                Timeout::Error(msg) => Err(msg.clone()),
            };
        }

        // Check other criteria
        for (target_state, criterion) in self.current_state().transitions.iter() {
            // Check whether this criterion has been met
            let should_transition = match criterion {
                Transition::Thresh(channel, op, thresh) => {
                    let i = self.input_map[channel];
                    let v = tape[i];

                    match op {
                        ThreshOp::Gt => v > *thresh,
                        ThreshOp::Lt => v < *thresh,
                        ThreshOp::Approx { rtol, atol } => {
                            let drel = rtol * thresh.abs();
                            let dtot = drel + atol;
                            (v - thresh).abs() < dtot
                        }
                    }
                }
            };

            // If a state transition has been triggered, update the execution state
            // to the start of the next state.
            if should_transition {
                self.transition(target_state.clone());
                return Ok(());
            }
        }

        // No transition criteria were met; stay the course
        Ok(())
    }
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

        // Permute order of each state's lookups to match the entrypoint
        let entry_order = &self.current_state().output_names.clone();
        for s in self.states.values_mut() {
            s.permute(entry_order);
        }

        // TODO: check that all outputs are consistent
        // TODO: set up eval order for transitions for each state
        // TODO: check that entrypoint is a real state that exists
    }

    fn terminate(&mut self) {
        self.input_indices.clear();
        self.output_range = usize::MAX..usize::MAX;
        let start_time = self.states.get(&self.cfg.entry).unwrap().get_start_time_s();
        self.execution_state = ExecutionState {
            sequence_time_s: start_time,
            current_state: self.cfg.entry.clone(),
        };
    }

    /// Run calcs for a cycle
    fn eval(&mut self, tape: &mut [f64]) {
        // Increment sequence time
        self.execution_state.sequence_time_s += self.dt_s;
        // Transition to the next state if needed, which may reset sequence time
        self.check_transitions(self.execution_state.sequence_time_s, &tape)
            .unwrap();
        // Update output values based on the current state
        self.current_state().eval(
            self.execution_state.sequence_time_s,
            self.output_range.clone(),
            tape,
        );
    }

    /// Map from input field names (like `v`, without prefix) to the state name
    /// that the input should draw from (like `peripheral_0.output_1`, with prefix)
    fn get_input_map(&self) -> BTreeMap<CalcInputName, FieldName> {
        let mut map = BTreeMap::new();

        for state in self.states.values() {
            for c in state.transitions.values() {
                for name in c.get_input_names() {
                    map.insert(name.clone(), name);
                }
            }
        }

        map
    }

    /// Change a value in the input map
    fn update_input_map(&mut self, _field: &str, _source: &str) -> Result<(), String> {
        return Err(format!(
            "Machine input map is derived from state transition criterion dependencies"
        ));
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
        self.current_state().output_names.clone()
    }

    /// Get flag for whether to save outputs
    fn get_save_outputs(&self) -> bool {
        self.cfg.save_outputs
    }

    /// Set flag for whether to save outputs
    fn set_save_outputs(&mut self, save_outputs: bool) {
        self.cfg.save_outputs = save_outputs;
    }

    /// Get config field values
    fn get_config(&self) -> BTreeMap<String, f64> {
        #[allow(unused_mut)]
        let mut cfg = BTreeMap::<String, f64>::new();

        cfg
    }

    /// Apply config field values
    #[allow(unused)]
    fn set_config(&mut self, cfg: &BTreeMap<String, f64>) -> Result<(), String> {
        Err("No settable config fields".to_string())
    }
}
