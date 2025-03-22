//! A lookup-table state machine that follows a set procedure during
//! each state, and transitions between states based on set criteria.
//!
//! Unlike most calcs, the names of the inputs and outputs of this calc
//! are not known at compile-time, and are assembled from inputs instead.

use core::f64;
use std::path::Path;

#[cfg(feature = "ser")]
use serde_json;

#[cfg(feature = "ser")]
use serde::{Deserialize, Serialize};

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
    // /// Transition if a value of some input exceeds a threshold value
    // /// that is interpolated from a lookup table
    // /// based on some choice of comparison operation and interpolation method.
    // LookupThresh(String, ThreshOp, SequenceLookup),
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
#[derive(Default, Debug)]
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

impl Method {
    /// Attempt to parse a string into an interpolation method.
    /// Case-insensitive and discards whitespace.
    pub fn from_str(s: &str) -> Result<Self, String> {
        let lower = s.to_lowercase();
        let normalized = lower.trim();
        match normalized {
            x if x == "linear" => Ok(Self::Linear),
            x if x == "left" => Ok(Self::Left),
            x if x == "right" => Ok(Self::Right),
            x if x == "nearest" => Ok(Self::Nearest),
            _ => Err(format!("Unable to process method: `{s}`")),
        }
    }
}

// #[derive(Default)]
// #[cfg_attr(feature = "ser", derive(Serialize, Deserialize))]
// pub struct StateCfg {
//     /// Transition criteria to evaluate at the beginning of each timestep
//     transitions: BTreeMap<FieldName, Transition>,

//     /// What to do at the end of the sequence
//     timeout: Timeout,
// }

#[cfg_attr(feature = "ser", derive(Serialize, Deserialize))]
pub struct SequenceLookup {
    /// Interpolation method
    method: Method,

    /// Grid to interpolate on. Must be the same length as `vals`.
    time_s: Vec<f64>,

    /// Values to interpolate. Must be the same length as `time_s`.
    vals: Vec<f64>,
}

impl SequenceLookup {
    /// Sample the lookup at a point in time
    pub fn eval(&self, sequence_time_s: f64) -> f64 {
        let grid = RectilinearGrid1D::new(&self.time_s, &self.vals).unwrap();
        let v = match self.method {
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

        v
    }
}

#[derive(Default)]
#[cfg_attr(feature = "ser", derive(Serialize, Deserialize))]
pub struct SequenceState {
    /// Sequence interpolation data
    data: BTreeMap<String, SequenceLookup>,
}

impl SequenceState {
    fn get_end_time_s(&self) -> f64 {
        let mut t = f64::NEG_INFINITY;
        for d in self.data.values() {
            t = t.max(*d.time_s.last().unwrap());
        }
        t
    }

    fn get_start_time_s(&self) -> f64 {
        let mut t = f64::INFINITY;
        for d in self.data.values() {
            t = t.min(d.time_s[0]);
        }
        t
    }

    /// Shuffle internal ordering of outputs to match the provided order
    fn permute(&mut self, output_names: &Vec<String>) {
        let mut new_map = BTreeMap::new();
        for n in output_names.iter() {
            new_map.insert(n.clone(), self.data.remove(n).unwrap());
        }
        assert!(
            self.data.is_empty(),
            "State eval order permutation was missing entres: {:?}",
            self.data.keys()
        );
        self.data = new_map;
    }

    /// Run the interpolators for this timestep
    fn eval(&self, sequence_time_s: f64, output_range: Range<usize>, tape: &mut [f64]) {
        for (i, d) in output_range.zip(self.data.values()) {
            tape[i] = d.eval(sequence_time_s);
        }
    }

    /// Load sequence from a CSV string
    #[cfg(feature = "ser")]
    pub fn from_csv_str(data_csv: &str) -> Result<Self, String> {
        let mut lines = data_csv.lines();

        // Parse header row, discarding time column name
        let output_names: Vec<String> = lines
            .next()
            .ok_or("Empty csv".to_string())?
            .split(",")
            .skip(1)
            .map(|s| s.to_owned())
            .collect();

        // Parse interpolation methods
        let mut methods: Vec<Method> = Vec::new();
        for s in lines
            .next()
            .ok_or("Empty csv".to_string())?
            .split(",")
            .skip(1)
        {
            let m = Method::from_str(s.trim())?;
            methods.push(m);
        }

        // Parse values from remaining lines
        let mut vals = vec![vec![]; methods.len()];
        let mut time_s = vec![vec![]; methods.len()];
        for (i, line) in lines.enumerate() {
            let mut entries = line.split(",");
            let ne = line.chars().filter(|c| *c == ',').count() + 1; // Size hint is not exact for split
            let ne_expected = methods.len() + 1; // Expected number of entries per line

            // If the line is empty, skip to the next line.
            if ne == 0 {
                continue;
            }

            // If the line is not empty but doesn't contain exactly one entry for each column,
            // we can't be sure that we are interpreting the user's intent correctly
            if ne < ne_expected {
                return Err(format!("CSV missing column on line {i}"));
            }
            if ne > ne_expected {
                return Err(format!("CSV has an extra column on line {i}"));
            }

            // Leftmost column is the time index
            let time = entries
                .next()
                .ok_or(format!("CSV read error on line {i}, empty line"))?
                .trim()
                .parse::<f64>()
                .map_err(|e| format!("Error parsing time value in CSV on line {i}: {e}"))?;

            // Parse column values
            for (j, entry) in entries.enumerate() {
                // Whitespace-only entries are null
                if entry.trim() == "" {
                    continue;
                }

                // Entries that are not empty or whitespace should be valid numbers
                let v = entry.parse::<f64>().map_err(|e| {
                    format!("CSV parse error on line {i} column {j} with entry {entry}: {e:?}")
                })?;

                time_s[j].push(time);
                vals[j].push(v);
            }
        }

        // Make sure all the columns have the same start time
        let start_time = time_s[0][0];
        for i in 0..methods.len() {
            let n = &output_names[i];
            if time_s[i][0] != start_time {
                return Err(format!(
                    "Value at start time missing for column {i} (`{n}`)"
                ));
            }
        }

        // Make sure time is sorted
        for (i, t) in time_s.iter().enumerate() {
            let n = &output_names[i];
            if !t.is_sorted() {
                return Err(format!("Out-of-order sequence for column {i} (`{n}`)"));
            }
        }

        // Pack parsed values
        let mut data = BTreeMap::new();
        methods.reverse();
        time_s.reverse();
        vals.reverse();
        for i in 0..methods.len() {
            data.insert(
                output_names[i].clone(),
                SequenceLookup {
                    method: methods.pop().unwrap(),
                    time_s: time_s.pop().unwrap(),
                    vals: vals.pop().unwrap(),
                },
            );
        }

        Ok(Self { data })
    }

    /// Load sequence from a CSV file
    pub fn from_csv_file(path: &dyn AsRef<Path>) -> Result<Self, String> {
        let csv_str =
            std::fs::read_to_string(path).map_err(|e| format!("CSV file read error: {e}"))?;
        Self::from_csv_str(&csv_str)
    }
}

#[derive(Default)]
struct ExecutionState {
    /// Time in current state's sequence.
    /// Starts at the first time in the state's lookup table.
    pub sequence_time_s: f64,

    /// Name of the current operating sequence
    pub current_state: String,
}

#[derive(Default)]
#[cfg_attr(feature = "ser", derive(Serialize, Deserialize))]
pub struct MachineCfg {
    // User inputs
    /// Whether to dispatch outputs
    pub save_outputs: bool,

    /// Name of SequenceState which is the entrypoint for the machine
    pub entry: String,

    /// Whether to reload from a folder at this relative path from the op dir during init
    pub link_folder: Option<String>,

    /// Timeout behavior for each state
    pub timeouts: BTreeMap<String, Timeout>,

    /// Early transition criteria for each state
    pub transitions: BTreeMap<String, BTreeMap<String, Vec<Transition>>>,
}

/// A lookup-table state machine that follows a set procedure during
/// each state, and transitions between states based on set criteria.
///
/// Unlike most calcs, the names of the inputs and outputs of this calc
/// are not known at compile-time, and are assembled from inputs instead.
#[derive(Default)]
#[cfg_attr(feature = "ser", derive(Serialize, Deserialize))]
pub struct Machine {
    /// State transition criteria and other configuration
    cfg: MachineCfg,

    /// All the lookup sequence states of the machine, including their
    /// transition criteria.
    ///
    /// All states must have the same outputs so that no values
    /// are ever left dangling, and all values must be defined
    /// at the first timestep of each sequence.
    ///
    /// The inputs to the machine are the sum of all the inputs
    /// required by each state.
    states: BTreeMap<String, SequenceState>,

    // Current execution state
    #[cfg_attr(feature = "ser", serde(skip))]
    execution_state: ExecutionState,

    // Values provided by calc orchestrator during init
    /// Lookup map for channel names to indices, required for evaluating
    /// transition criteria
    #[cfg_attr(feature = "ser", serde(skip))]
    input_index_map: BTreeMap<String, usize>,

    /// Timestep
    #[cfg_attr(feature = "ser", serde(skip))]
    dt_s: f64,

    #[cfg_attr(feature = "ser", serde(skip))]
    input_indices: Vec<usize>,

    #[cfg_attr(feature = "ser", serde(skip))]
    output_range: Range<usize>,
}

impl Machine {
    pub fn new(cfg: MachineCfg, states: BTreeMap<String, SequenceState>) -> Result<Self, String> {
        // These will be set during init.
        // Use default indices that will cause an error on the first call if not initialized properly
        let input_indices = Vec::new();
        let output_range = usize::MAX..usize::MAX;

        let machine = Self {
            cfg,
            states,
            execution_state: ExecutionState::default(),
            input_index_map: BTreeMap::new(),

            dt_s: f64::NAN,
            input_indices,
            output_range,
        };

        machine.validate()?;

        Ok(machine)
    }

    fn validate(&self) -> Result<(), String> {
        Ok(())
    }

    /// Get a reference to the state indicated in execution_state.current_state
    fn current_state(&self) -> &SequenceState {
        &self.states[&self.execution_state.current_state]
    }

    /// Get a reference to the entrypoint state
    fn entry_state(&self) -> &SequenceState {
        &self.states[&self.cfg.entry]
    }

    /// Set next target state and reset sequence time to the initial time for that state.
    fn transition(&mut self, target_state: String) {
        self.execution_state.current_state = target_state;
        self.execution_state.sequence_time_s = self.current_state().get_start_time_s();
    }

    /// Check each state transition criterion and set the next state if needed.
    /// If multiple transition criteria are met at the same time, the first
    /// one in the list will be prioritized.
    fn check_transitions(&mut self, sequence_time_s: f64, tape: &[f64]) -> Result<(), String> {
        let state_name = &self.execution_state.current_state;

        // Check for timeout
        if sequence_time_s > self.current_state().get_end_time_s() {
            return match &self.cfg.timeouts[state_name] {
                Timeout::Transition(target_state) => {
                    self.transition(target_state.clone());
                    Ok(())
                }
                Timeout::Loop => {
                    self.transition(state_name.clone());
                    Ok(())
                }
                Timeout::Error(msg) => Err(msg.clone()),
            };
        }

        // Check other criteria
        for (target_state, criteria) in self.cfg.transitions[state_name].iter() {
            // Check whether this each criterion has been met
            for criterion in criteria {
                let should_transition = match criterion {
                    Transition::Thresh(channel, op, thresh) => {
                        let i = self.input_index_map[channel];
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
        }

        // No transition criteria were met; stay the course
        Ok(())
    }

    /// Read a configuration json and sequence CSV files from a folder.
    /// The folder must contain one json representing a [MachineCfg] and
    /// some number of CSV files each representing a [SequenceState].
    pub fn load_folder(path: &dyn AsRef<Path>) -> Result<Self, String> {
        let dir = std::fs::read_dir(path)
            .map_err(|e| format!("Unable to read items in folder {:?}: {e}", path.as_ref()))?;

        let mut csv_files = Vec::new();
        let mut json_files = Vec::new();
        for entry in dir {
            match entry {
                Ok(e) => {
                    let path = e.path();
                    if path.is_file() {
                        match path.extension() {
                            Some(ext) if ext.to_ascii_lowercase().to_str() == Some("csv") => {
                                csv_files.push(path)
                            }
                            Some(ext) if ext.to_ascii_lowercase().to_str() == Some("json") => {
                                json_files.push(path)
                            }
                            _ => {}
                        }
                    }
                }
                _ => {}
            }
        }

        // Make sure there is exactly one json file
        if json_files.len() == 0 {
            return Err("Did not find configuration json file".to_string());
        }

        if json_files.len() > 1 {
            return Err(format!("Found multiple config json files: {json_files:?}"));
        }

        // Load config
        let json_file = &json_files[0];
        let json_str = std::fs::read_to_string(json_file)
            .map_err(|e| format!("Failed to read config json: {e}"))?;
        let cfg: MachineCfg = serde_json::from_str(&json_str)
            .map_err(|e| format!("Failed to parse config json: {e}"))?;

        // Load sequences
        let mut states = BTreeMap::new();
        for fp in csv_files {
            // unwrap: ok because we already checked that this is a file with an extension
            let name = fp.file_stem().unwrap().to_str().unwrap().to_owned();
            let seq: SequenceState = SequenceState::from_csv_file(&fp)?;
            states.insert(name, seq);
        }

        Self::new(cfg, states)
    }
}

#[cfg_attr(feature = "ser", typetag::serde)]
impl Calc for Machine {
    /// Reset internal state and register calc tape indices
    fn init(&mut self, ctx: ControllerCtx, input_indices: Vec<usize>, output_range: Range<usize>) {
        // Reload from folder, if linked
        if let Some(rel_path) = &self.cfg.link_folder {
            let folder = ctx.op_dir.join(rel_path);
            *self = Self::load_folder(&folder).unwrap();
        }

        // Reset execution state
        self.terminate();

        // Set per-run config
        self.input_indices = input_indices;
        self.output_range = output_range;
        self.dt_s = ctx.dt_ns as f64 / 1e9;

        // Permute order of each state's lookups to match the entrypoint
        let entry_order = &self.current_state().data.keys().cloned().collect();
        for s in self.states.values_mut() {
            s.permute(entry_order);
        }

        // Set up map from input names to tape indices to support
        // transition checks
        self.input_index_map = BTreeMap::new();
        for (i, name) in self
            .input_indices
            .iter()
            .cloned()
            .zip(self.get_input_names().iter())
        {
            self.input_index_map.insert(name.clone(), i);
        }

        // TODO: check that all outputs are consistent
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

        for transitions in self.cfg.transitions.values() {
            for criteria in transitions.values() {
                for criterion in criteria {
                    let names = criterion.get_input_names();
                    for name in names {
                        map.insert(name.clone(), name);
                    }
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
        self.get_input_map().keys().cloned().collect()
    }

    /// All states have the same outputs
    fn get_output_names(&self) -> Vec<CalcOutputName> {
        self.entry_state().data.keys().cloned().collect()
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
        BTreeMap::<String, f64>::new()
    }

    /// Apply config field values
    #[allow(unused)]
    fn set_config(&mut self, cfg: &BTreeMap<String, f64>) -> Result<(), String> {
        Err("No settable config fields".to_string())
    }
}
