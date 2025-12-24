//! A lookup-table sequence machine that follows a set procedure during
//! each sequence, and transitions between sequences based on set criteria.

use core::f64;
use std::{collections::HashSet, path::Path};

use serde::{Deserialize, Serialize};
use serde_json;

pub type StateName = String;

#[cfg(feature = "python")]
use pyo3::prelude::*;

use super::*;

mod lookup;
mod sequence;
mod transition;

pub use lookup::{InterpMethod, SequenceLookup};
pub use sequence::Sequence;
pub use transition::{ThreshOp, Timeout, Transition};

#[derive(Default, Debug)]
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
#[derive(Default, Debug, Serialize, Deserialize)]
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
#[derive(Serialize, Deserialize, Debug)]
#[cfg_attr(feature = "python", pyclass)]
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
    #[serde(skip)]
    execution_state: ExecutionState,
}

impl Default for SequenceMachine {
    fn default() -> Self {
        Self {
            cfg: MachineCfg {
                entry: "Placeholder".into(),
                ..Default::default()
            },
            sequences: BTreeMap::from([("Placeholder".into(), Sequence::default())]),
            execution_state: ExecutionState::default(),
        }
    }
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
                    // info!("Transition `{target_sequence}` due to timeout");
                    self.transition(target_sequence.clone());
                    Ok(())
                }
                Timeout::Loop => {
                    // info!("Looping sequence `{sequence_name}` due to timeout");
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
                    // info!("Transition `{target_sequence}` due to {criterion:?}");
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
            let name = fp
                .file_stem()
                .ok_or_else(|| "Filename missing".to_string())?
                .to_str()
                .ok_or_else(|| "Filename is not valid unicode".to_string())?
                .to_owned();
            let seq: Sequence = Sequence::from_csv_file(&fp)?;
            sequences.insert(name, seq);
        }

        Self::new(cfg, sequences)
    }
}

#[typetag::serde]
impl Calc for SequenceMachine {
    /// Reset internal sequence and register calc tape indices
    fn init(
        &mut self,
        ctx: ControllerCtx,
        input_indices: Vec<usize>,
        output_range: Range<usize>,
    ) -> Result<(), String> {
        // Reload from folder, if linked
        if let Some(rel_path) = &self.cfg.link_folder {
            let folder = ctx.op_dir.join(rel_path);
            *self = Self::load_folder(&folder)
                .map_err(|e| format!("Failed to load sequence machine from linked folder: {e}"))?;
        }

        // Reset execution sequence
        self.terminate()?;

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
        self.validate()
    }

    fn terminate(&mut self) -> Result<(), String> {
        self.execution_state.input_indices.clear();
        self.execution_state.output_range = usize::MAX..usize::MAX;
        let start_time = self
            .sequences
            .get(&self.cfg.entry)
            .ok_or_else(|| "Missing sequence".to_string())?
            .get_start_time_s();
        self.execution_state.sequence_time_s = start_time;
        self.execution_state.current_sequence = self.cfg.entry.clone();
        Ok(())
    }

    fn eval(&mut self, tape: &mut [f64]) -> Result<(), String> {
        // Increment sequence time
        self.execution_state.sequence_time_s += self.execution_state.dt_s;
        // Transition to the next sequence if needed, which may reset sequence time
        self.check_transitions(self.execution_state.sequence_time_s, tape)?;

        // Update output values based on the current sequence
        self.current_sequence().eval(
            self.execution_state.sequence_time_s,
            self.execution_state.output_range.clone(),
            tape,
        );
        Ok(())
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

#[cfg(feature = "python")]
#[pymethods]
impl SequenceMachine {
    #[new]
    fn py_new() -> Self {
        let mut cfg = MachineCfg::default();
        cfg.save_outputs = true;

        Self {
            cfg,
            sequences: BTreeMap::new(),
            execution_state: ExecutionState::default()
        }
    }

    /// Serialize to typetagged JSON so Python can pass into trait handoff
    fn to_json(&self) -> PyResult<String> {
        let payload: &dyn Calc = self;
        serde_json::to_string(payload)
            .map_err(|e| pyo3::exceptions::PyValueError::new_err(e.to_string()))
    }

    /// Deserialize from typetagged JSON
    #[classmethod]
    fn from_json(_cls: &Bound<'_, pyo3::types::PyType>, s: &str) -> PyResult<Self> {
        serde_json::from_str::<Self>(s)
            .map_err(|e| pyo3::exceptions::PyValueError::new_err(e.to_string()))
    }
}