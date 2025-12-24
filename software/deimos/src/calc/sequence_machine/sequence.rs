use std::collections::BTreeMap;
use std::ops::Range;
use std::path::Path;

use serde::{Deserialize, Serialize};

use super::{CalcOutputName, InterpMethod, SequenceLookup};

/// A state in a SequenceMachine, defined by a set of time-dependent
/// sequence lookups.
#[derive(Default, Debug, Serialize, Deserialize)]
pub struct Sequence {
    /// Sequence interpolation data
    pub(super) data: BTreeMap<CalcOutputName, SequenceLookup>,
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
            .ok_or_else(|| "Empty csv".to_string())?
            .split(",")
            .skip(1)
            .map(|s| s.to_owned())
            .collect();

        // Parse interpolation methods
        let mut methods: Vec<InterpMethod> = Vec::new();
        for s in lines
            .next()
            .ok_or_else(|| "Empty csv".to_string())?
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
                .ok_or_else(|| format!("CSV read error on line {i}, empty line"))?
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
