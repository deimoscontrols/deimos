//! A lookup-table sequence machine that follows a set procedure during
//! each sequence, and transitions between sequences based on set criteria.

use core::f64;
use std::{collections::HashSet, path::Path};

#[cfg(feature = "ser")]
use serde_json;

#[cfg(feature = "ser")]
use serde::{Deserialize, Serialize};

use interpn::one_dim::{Interp1D, RectilinearGrid1D};

pub type StateName = String;

use super::*;

/// Choice of behavior when a given sequence reaches the end of its lookup table
#[derive(Default)]
#[cfg_attr(feature = "ser", derive(Serialize, Deserialize))]
#[non_exhaustive]
pub enum Timeout {
    /// Transition to the next sequence
    Transition(StateName),

    /// Start over from the beginning of the table
    #[default]
    Loop,

    /// Raise an error with a message
    Error(String),
}

/// A logical operator used to evaluate whether a transition should occur.
#[cfg_attr(feature = "ser", derive(Serialize, Deserialize))]
pub enum ThreshOp {
    /// Greater than
    Gt { by: f64 },

    /// Less than
    Lt { by: f64 },

    /// Approximately equal
    Approx { rtol: f64, atol: f64 },
}

impl Default for ThreshOp {
    fn default() -> Self {
        Self::Gt { by: 0.0 }
    }
}

impl ThreshOp {
    /// Check whether a value meets a threshold based on this operation.
    pub fn eval(&self, v: f64, thresh: f64) -> bool {
        // Check for NaN
        assert!(
            !v.is_nan() && !thresh.is_nan(),
            "Unable to assess transition criteria involving NaN values."
        );

        // Evaluate whether a transition should occur
        match self {
            ThreshOp::Gt { by } => v > thresh + by,
            ThreshOp::Lt { by } => v < thresh - by,
            ThreshOp::Approx { rtol, atol } => {
                let drel = rtol * thresh.abs();
                let dtot = drel + atol;
                (v - thresh).abs() < dtot
            }
        }
    }
}

/// Methods for checking whether a sequence transition should occur
#[cfg_attr(feature = "ser", derive(Serialize, Deserialize))]
#[non_exhaustive]
pub enum Transition {
    /// Transition if a value of some input exceeds a threshold value
    /// based on some choice of comparison operation.
    ///
    /// This may be used, for example, to exit when overheating is detected,
    /// or to wait until a controlled parameter has converged to a value
    /// before proceeding into the next part of an operation.
    ConstantThresh(CalcInputName, ThreshOp, f64),

    /// Transition if a value of some input exceeds the value of another input
    /// based on some choice of comparison operation.
    ///
    /// This is an adaptable way to continue to the next sequence
    /// once a controller has converged (for example, waiting to preheat)
    /// by comparing the target state and measured state, without the need
    /// to update the threshold value every time the setpoint changes.
    ChannelThresh(CalcInputName, ThreshOp, CalcInputName),

    /// Transition if a value of some input exceeds a threshold value
    /// that is interpolated from a lookup table based on some choice
    /// of comparison operation and interpolation method.
    ///
    /// This type of threshold can help maintain guard rails around sensitive values
    /// during sensitive transient operations.
    LookupThresh(CalcInputName, ThreshOp, SequenceLookup),
}

impl Transition {
    /// Get a list of the names of inputs needed by this transition check
    pub fn get_input_names(&self) -> Vec<FieldName> {
        let mut names = Vec::new();
        match self {
            Self::ConstantThresh(name, _, _) => names.push(name.clone()),
            Self::ChannelThresh(first, _, second) => {
                names.extend_from_slice(&[first.clone(), second.clone()])
            }
            Self::LookupThresh(name, _, _) => names.push(name.clone()),
        };

        names
    }
}

/// Interpolation method for sequence lookups
#[derive(Default, Debug)]
#[cfg_attr(feature = "ser", derive(Serialize, Deserialize))]
#[non_exhaustive]
pub enum InterpMethod {
    /// Linear interpolation inside the grid. Outside the grid,
    /// the nearest value is held constant to prevent runaway extrapolation.
    Linear,

    /// Hold the value on the left side of the current cell.
    ///
    /// Hold-left is the default because intermediate values may not be
    /// valid values in some cases, while the values at the control points
    /// are valid to the extent that the user's intent is valid.
    #[default]
    Left,

    /// Hold the value on the right side of the current cell.
    Right,

    /// Hold the nearest value to the left or right of the current cell.
    Nearest,
}

impl InterpMethod {
    /// Attempt to parse a string into an interpolation method.
    /// Case-insensitive and discards whitespace.
    pub fn try_parse(s: &str) -> Result<Self, String> {
        let lower = s.to_lowercase();
        let normalized = lower.trim();
        match normalized {
            "linear" => Ok(Self::Linear),
            "left" => Ok(Self::Left),
            "right" => Ok(Self::Right),
            "nearest" => Ok(Self::Nearest),
            _ => Err(format!("Unable to process method: `{s}`")),
        }
    }
}

/// A lookup table defining one sequenced output from a Sequence
#[cfg_attr(feature = "ser", derive(Serialize, Deserialize))]
pub struct SequenceLookup {
    /// Interpolation method
    method: InterpMethod,

    /// Grid to interpolate on. Must be the same length as `vals`.
    time_s: Vec<f64>,

    /// Values to interpolate. Must be the same length as `time_s`.
    vals: Vec<f64>,
}

impl SequenceLookup {
    /// Validate and store lookup
    pub fn new(method: InterpMethod, time_s: Vec<f64>, vals: Vec<f64>) -> Result<Self, String> {
        let seq = Self {
            method,
            time_s,
            vals,
        };

        match seq.validate() {
            Ok(()) => Ok(seq),
            Err(x) => Err(x),
        }
    }

    /// Check that time is monotonic and lengths are correct
    pub fn validate(&self) -> Result<(), String> {
        if !self.time_s.is_sorted() {
            return Err("Sequence time entries must be monotonically increasing".to_owned());
        }

        match self.eval_checked(0.0) {
            Ok(_) => Ok(()),
            Err(x) => Err(x),
        }
    }

    /// Sample the lookup, propagating any errors encountered while assembling or evaluating the interpolator.
    pub fn eval_checked(&self, sequence_time_s: f64) -> Result<f64, String> {
        let grid = RectilinearGrid1D::new(&self.time_s, &self.vals).map_err(|e| e.to_owned())?;

        match self.method {
            InterpMethod::Linear => interpn::LinearHoldLast1D::new(grid)
                .eval_one(sequence_time_s)
                .map_err(|e| e.to_owned()),
            InterpMethod::Left => interpn::Left1D::new(grid)
                .eval_one(sequence_time_s)
                .map_err(|e| e.to_owned()),
            InterpMethod::Right => interpn::Right1D::new(grid)
                .eval_one(sequence_time_s)
                .map_err(|e| e.to_owned()),
            InterpMethod::Nearest => interpn::Nearest1D::new(grid)
                .eval_one(sequence_time_s)
                .map_err(|e| e.to_owned()),
        }
    }

    /// Sample the lookup at a point in time
    pub fn eval(&self, sequence_time_s: f64) -> f64 {
        self.eval_checked(sequence_time_s).unwrap()
    }
}

/// A state in a SequenceMachine, defined by a set of time-dependent
/// sequence lookups.
#[derive(Default)]
#[cfg_attr(feature = "ser", derive(Serialize, Deserialize))]
pub struct Sequence {
    /// Sequence interpolation data
    data: BTreeMap<CalcOutputName, SequenceLookup>,
}

impl Sequence {
    /// Get the final time point defined for any of the lookups in the sequence
    pub fn get_end_time_s(&self) -> f64 {
        let mut t = f64::NEG_INFINITY;
        for d in self.data.values() {
            t = t.max(*d.time_s.last().unwrap());
        }
        t
    }

    /// Get the first time point in the sequence
    pub fn get_start_time_s(&self) -> f64 {
        let mut t = f64::INFINITY;
        for d in self.data.values() {
            t = t.min(d.time_s[0]);
        }
        t
    }

    /// Check for misconfiguration
    pub fn validate(&self) -> Result<(), String> {
        for lookup in self.data.values() {
            lookup.validate()?;
        }

        Ok(())
    }

    /// Shuffle internal ordering of outputs to match the provided order
    pub fn permute(&mut self, output_names: &[String]) {
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
    pub fn eval(&self, sequence_time_s: f64, output_range: Range<usize>, tape: &mut [f64]) {
        // First entry is sequence time
        let time_ind = output_range.start;
        tape[time_ind] = sequence_time_s;

        // Later entries are lookup outputs
        for (i, d) in output_range.skip(1).zip(self.data.values()) {
            tape[i] = d.eval(sequence_time_s);
        }
    }

    /// Load sequence from a CSV string
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
        let mut methods: Vec<InterpMethod> = Vec::new();
        for s in lines
            .next()
            .ok_or("Empty csv".to_string())?
            .split(",")
            .skip(1)
        {
            let m = InterpMethod::try_parse(s.trim())?;
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
    /// Time in current sequence's sequence.
    /// Starts at the first time in the sequence's lookup table.
    pub sequence_time_s: f64,

    /// Name of the current operating sequence
    pub current_sequence: String,

    // Values provided by calc orchestrator during init
    /// Lookup map for channel names to indices, required for evaluating
    /// transition criteria
    pub input_index_map: BTreeMap<String, usize>,

    /// Timestep
    pub dt_s: f64,

    /// Indices of input calc/channel names
    pub input_indices: Vec<usize>,

    /// Where to write the calc outputs in the calc tape
    pub output_range: Range<usize>,
}

/// Sequence entrypoint and transition criteria for the SequenceMachine.
#[derive(Default)]
#[cfg_attr(feature = "ser", derive(Serialize, Deserialize))]
pub struct MachineCfg {
    // User inputs
    /// Whether to dispatch outputs
    pub save_outputs: bool,

    /// Name of Sequence which is the entrypoint for the machine
    pub entry: String,

    /// Whether to reload from a folder at this relative path from the op dir during init
    pub link_folder: Option<String>,

    /// Timeout behavior for each sequence
    pub timeouts: BTreeMap<String, Timeout>,

    /// Early transition criteria for each sequence
    pub transitions: BTreeMap<String, BTreeMap<String, Vec<Transition>>>,
}

/// A lookup-table sequence machine that follows a set procedure during
/// each sequence, and transitions between sequences based on set criteria.
///
/// Unlike most calcs, the names of the inputs and outputs of this calc
/// are not known at compile-time, and are assembled from inputs instead.
#[derive(Default)]
#[cfg_attr(feature = "ser", derive(Serialize, Deserialize))]
pub struct SequenceMachine {
    /// State transition criteria and other configuration
    cfg: MachineCfg,

    /// All the lookup sequence sequences of the machine, including their
    /// transition criteria.
    ///
    /// All sequences must have the same outputs so that no values
    /// are ever left dangling, and all values must be defined
    /// at the first timestep of each sequence.
    ///
    /// The inputs to the machine are the sum of all the inputs
    /// required by each sequence.
    sequences: BTreeMap<String, Sequence>,

    /// Current execution state of the SequenceMachine
    /// including sequence time and per-run configuration.
    #[cfg_attr(feature = "ser", serde(skip))]
    execution_state: ExecutionState,
}

impl SequenceMachine {
    /// Store and validate a new SequenceMachine
    pub fn new(cfg: MachineCfg, sequences: BTreeMap<String, Sequence>) -> Result<Self, String> {
        // These will be set during init.
        // Use default indices that will cause an error on the first call if not initialized properly
        let input_indices = Vec::new();
        let output_range = usize::MAX..usize::MAX;
        let entry = cfg.entry.to_owned();

        let machine = Self {
            cfg,
            sequences,
            execution_state: ExecutionState {
                sequence_time_s: f64::NAN,
                current_sequence: entry,
                input_index_map: BTreeMap::new(),
                dt_s: f64::NAN,
                input_indices,
                output_range,
            },
        };

        machine.validate()?;

        Ok(machine)
    }

    /// Check the validity of sequences, transitions, timeouts, etc
    fn validate(&self) -> Result<(), String> {
        // Validate individual sequences and lookups
        for seq in self.sequences.values() {
            seq.validate()?;
        }

        // Validate machine-level configuration
        let seq_names: HashSet<String> = self.sequences.keys().cloned().collect();

        // Make sure all the timeouts are present and there aren't any extras
        let timeout_seq_names: HashSet<String> = self.cfg.timeouts.keys().cloned().collect();
        if seq_names != timeout_seq_names {
            return Err(format!(
                "Timeouts do not match sequences. Sequence names that are present in sequences but not timeouts: `{:?}`. Sequence names that are present in timeouts but not sequences: {:?}",
                seq_names.difference(&timeout_seq_names),
                timeout_seq_names.difference(&seq_names)
            ));
        }

        // Make sure all the transitions are present and there aren't any extras
        let transition_seq_names: HashSet<String> = self.cfg.timeouts.keys().cloned().collect();
        if seq_names != transition_seq_names {
            return Err(format!(
                "Transitions do not match sequences. Sequence names that are present in sequences but not timeouts: `{:?}`. Sequence names that are present in transitions but not sequences: {:?}",
                seq_names.difference(&transition_seq_names),
                transition_seq_names.difference(&seq_names)
            ));
        }

        // Make sure all the transitions' target sequences refer to real targets
        for (seq, transitions) in self.cfg.transitions.iter() {
            for target_sequence in transitions.keys() {
                if !seq_names.contains(target_sequence) {
                    return Err(format!(
                        "Sequence `{seq}` has transition target sequence `{target_sequence}` which does not exist."
                    ));
                }
            }
        }

        Ok(())
    }

    /// Get a reference to the sequence indicated in execution_state.current_sequence
    fn current_sequence(&self) -> &Sequence {
        &self.sequences[&self.execution_state.current_sequence]
    }

    /// Get a reference to the entrypoint sequence
    fn entry_sequence(&self) -> &Sequence {
        &self.sequences[&self.cfg.entry]
    }

    /// Set next target sequence and reset sequence time to the initial time for that sequence.
    fn transition(&mut self, target_sequence: String) {
        self.execution_state.current_sequence = target_sequence;
        self.execution_state.sequence_time_s = self.current_sequence().get_start_time_s();
    }

    /// Check each sequence transition criterion and set the next sequence if needed.
    /// If multiple transition criteria are met at the same time, the first
    /// one in the list will be prioritized.
    fn check_transitions(&mut self, sequence_time_s: f64, tape: &[f64]) -> Result<(), String> {
        let sequence_name = &self.execution_state.current_sequence;

        // Check for timeout
        if sequence_time_s > self.current_sequence().get_end_time_s() {
            return match &self.cfg.timeouts[sequence_name] {
                Timeout::Transition(target_sequence) => {
                    self.transition(target_sequence.clone());
                    Ok(())
                }
                Timeout::Loop => {
                    self.transition(sequence_name.clone());
                    Ok(())
                }
                Timeout::Error(msg) => Err(msg.clone()),
            };
        }

        // Check other criteria
        for (target_sequence, criteria) in self.cfg.transitions[sequence_name].iter() {
            // Check whether this each criterion has been met
            for criterion in criteria {
                let should_transition = match criterion {
                    Transition::ConstantThresh(channel, op, thresh) => {
                        let i = self.execution_state.input_index_map[channel];
                        let v = tape[i];

                        op.eval(v, *thresh)
                    }
                    Transition::ChannelThresh(val_channel, op, thresh_channel) => {
                        let ival = self.execution_state.input_index_map[val_channel];
                        let ithresh = self.execution_state.input_index_map[thresh_channel];
                        let v = tape[ival];
                        let thresh = tape[ithresh];

                        op.eval(v, thresh)
                    }
                    Transition::LookupThresh(channel, op, lookup) => {
                        let i = self.execution_state.input_index_map[channel];
                        let v = tape[i];
                        let thresh = lookup.eval(sequence_time_s);

                        op.eval(v, thresh)
                    }
                };

                // If a sequence transition has been triggered, update the execution sequence
                // to the start of the next sequence.
                if should_transition {
                    self.transition(target_sequence.clone());
                    return Ok(());
                }
            }
        }

        // No transition criteria were met; stay the course
        Ok(())
    }

    /// Read a configuration json and sequence CSV files from a folder.
    /// The folder must contain one json representing a [MachineCfg] and
    /// some number of CSV files each representing a [Sequence].
    #[cfg(feature = "ser")]
    pub fn load_folder(path: &dyn AsRef<Path>) -> Result<Self, String> {
        let dir = std::fs::read_dir(path)
            .map_err(|e| format!("Unable to read items in folder {:?}: {e}", path.as_ref()))?;

        let mut csv_files = Vec::new();
        let mut json_files = Vec::new();
        for e in dir.flatten() {
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

        // Make sure there is exactly one json file
        if json_files.is_empty() {
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
        let mut sequences = BTreeMap::new();
        for fp in csv_files {
            // unwrap: ok because we already checked that this is a file with an extension
            let name = fp.file_stem().unwrap().to_str().unwrap().to_owned();
            let seq: Sequence = Sequence::from_csv_file(&fp)?;
            sequences.insert(name, seq);
        }

        Self::new(cfg, sequences)
    }
}

#[cfg_attr(feature = "ser", typetag::serde)]
impl Calc for SequenceMachine {
    /// Reset internal sequence and register calc tape indices
    fn init(&mut self, ctx: ControllerCtx, input_indices: Vec<usize>, output_range: Range<usize>) {
        // Reload from folder, if linked
        if let Some(rel_path) = &self.cfg.link_folder {
            let folder = ctx.op_dir.join(rel_path);
            *self = Self::load_folder(&folder).unwrap();
        }

        // Reset execution sequence
        self.terminate();

        // Set per-run config
        self.execution_state.input_indices = input_indices;
        self.execution_state.output_range = output_range;
        self.execution_state.dt_s = ctx.dt_ns as f64 / 1e9;

        // Permute order of each sequence's lookups to match the entrypoint
        let entry_order: Vec<String> = self.current_sequence().data.keys().cloned().collect();
        for s in self.sequences.values_mut() {
            s.permute(&entry_order);
        }

        // Set up map from input names to tape indices to support
        // transition checks
        self.execution_state.input_index_map = BTreeMap::new();
        for (i, name) in self
            .execution_state
            .input_indices
            .iter()
            .cloned()
            .zip(self.get_input_names().iter())
        {
            self.execution_state.input_index_map.insert(name.clone(), i);
        }

        // Make sure lookup tables are usable, transitions refer to real sequences, etc
        self.validate().unwrap();
    }

    fn terminate(&mut self) {
        self.execution_state.input_indices.clear();
        self.execution_state.output_range = usize::MAX..usize::MAX;
        let start_time = self
            .sequences
            .get(&self.cfg.entry)
            .unwrap()
            .get_start_time_s();
        self.execution_state.sequence_time_s = start_time;
        self.execution_state.current_sequence = self.cfg.entry.clone();
    }

    fn eval(&mut self, tape: &mut [f64]) {
        // Increment sequence time
        self.execution_state.sequence_time_s += self.execution_state.dt_s;
        // Transition to the next sequence if needed, which may reset sequence time
        self.check_transitions(self.execution_state.sequence_time_s, tape)
            .unwrap();

        // Update output values based on the current sequence
        self.current_sequence().eval(
            self.execution_state.sequence_time_s,
            self.execution_state.output_range.clone(),
            tape,
        );
    }

    /// Map from input field names (like `v`, without prefix) to the sequence name
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
        Err(
            "SequenceMachine input map is derived from sequence transition criterion dependencies"
                .to_string(),
        )
    }

    /// Inputs are the sum of all inputs required by any sequence
    fn get_input_names(&self) -> Vec<CalcInputName> {
        self.get_input_map().keys().cloned().collect()
    }

    /// All sequences have the same outputs
    fn get_output_names(&self) -> Vec<CalcOutputName> {
        let mut output_names = vec!["sequence_time_s".to_owned()];
        self.entry_sequence()
            .data
            .keys()
            .cloned()
            .for_each(|n| output_names.push(n));
        output_names
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
