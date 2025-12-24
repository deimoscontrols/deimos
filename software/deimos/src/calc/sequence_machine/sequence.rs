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

    /// Get sorted, unique time points across all lookups.
    pub fn get_times(&self) -> Vec<f64> {
        let mut time_points: Vec<f64> = self
            .data
            .values()
            .flat_map(|lookup| lookup.time_s.iter().copied())
            .collect();
        time_points.sort_by(|a, b| a.partial_cmp(b).unwrap());
        time_points.dedup_by(|a, b| a == b);
        time_points
    }

    /// Check for misconfiguration
    pub fn validate(&self) -> Result<(), String> {
        let mut start_time = None;
        for lookup in self.data.values() {
            lookup.validate()?;

            let time = lookup
                .time_s
                .first()
                .ok_or_else(|| "Sequence lookup has empty time values".to_string())?;
            if let Some(expected) = start_time {
                if *time != expected {
                    return Err("Sequence outputs must share the same start time".to_string());
                }
            } else {
                start_time = Some(*time);
            }
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
        let mut rdr = csv::ReaderBuilder::new()
            .has_headers(false)
            .from_reader(data_csv.as_bytes());
        let mut records = rdr.records();

        let header = next_nonempty_record(&mut records, "Empty csv")?;
        let output_names: Vec<String> = header.iter().skip(1).map(|s| s.to_owned()).collect();
        if output_names.is_empty() {
            return Err("CSV missing output columns".to_string());
        }

        let method_record = next_nonempty_record(&mut records, "Empty csv")?;
        let expected_len = output_names.len() + 1;
        if method_record.len() < expected_len {
            return Err("CSV missing method column".to_string());
        }
        if method_record.len() > expected_len {
            return Err("CSV has an extra method column".to_string());
        }

        let mut methods: Vec<InterpMethod> = Vec::with_capacity(output_names.len());
        for s in method_record.iter().skip(1) {
            methods.push(InterpMethod::try_parse(s.trim())?);
        }

        let mut vals = vec![vec![]; methods.len()];
        let mut time_s = vec![vec![]; methods.len()];
        for (i, result) in records.enumerate() {
            let record = result.map_err(|e| format!("CSV read error on line {i}: {e}"))?;
            if record.len() == 0 {
                continue;
            }
            if record.len() < expected_len {
                return Err(format!("CSV missing column on line {i}"));
            }
            if record.len() > expected_len {
                return Err(format!("CSV has an extra column on line {i}"));
            }

            let time_entry = record
                .get(0)
                .ok_or_else(|| format!("CSV read error on line {i}, empty line"))?;
            let time = time_entry
                .trim()
                .parse::<f64>()
                .map_err(|e| format!("Error parsing time value in CSV on line {i}: {e}"))?;

            for (j, entry) in record.iter().skip(1).enumerate() {
                if entry.trim().is_empty() {
                    continue;
                }
                let v = entry.trim().parse::<f64>().map_err(|e| {
                    format!("CSV parse error on line {i} column {j} with entry {entry}: {e:?}")
                })?;
                time_s[j].push(time);
                vals[j].push(v);
            }
        }

        let mut data = BTreeMap::new();
        for ((name, method), (times, vals)) in output_names
            .into_iter()
            .zip(methods)
            .zip(time_s.into_iter().zip(vals))
        {
            let lookup = SequenceLookup::new(method, times, vals)?;
            data.insert(name, lookup);
        }

        Ok(Self { data })
    }

    /// Load sequence from a CSV file
    pub fn from_csv_file(path: &dyn AsRef<Path>) -> Result<Self, String> {
        let csv_str =
            std::fs::read_to_string(path).map_err(|e| format!("CSV file read error: {e}"))?;
        Self::from_csv_str(&csv_str)
    }

    /// Save sequence to a CSV file
    pub fn save_csv(&self, path: &dyn AsRef<Path>) -> Result<(), String> {
        // Start a writer for arbitrary data.
        // We have two header rows, so we'll write our headers manually.
        let mut writer = csv::WriterBuilder::new()
            .has_headers(false)
            .from_path(path)
            .map_err(|e| format!("CSV file write error: {e}"))?;

        // Get references to column data
        let output_names = self.data.keys();
        let lookups: Vec<&SequenceLookup> = self.data.values().collect();

        // Build first header row
        let mut header: Vec<&str> = vec!["time_s"];
        output_names.for_each(|n| header.push(n));
        writer
            .write_record(header)
            .map_err(|e| format!("CSV write error: {e}"))?;

        // Build second header row
        let mut method_row = Vec::with_capacity(lookups.len() + 1);
        method_row.push("method".to_string());
        for lookup in lookups.iter() {
            method_row.push(lookup.method.to_str().to_string());
        }
        writer
            .write_record(method_row)
            .map_err(|e| format!("CSV write error: {e}"))?;

        // Get unique time points across all columns
        let time_points: Vec<f64> = self.get_times();

        // Write data values at each time, leaving null if no data is present.
        let mut indices = vec![0usize; lookups.len()];
        for time in time_points {
            let mut record = Vec::with_capacity(lookups.len() + 1);
            record.push(time.to_string());
            for (idx, lookup) in lookups.iter().enumerate() {
                let pos = indices[idx];
                if pos < lookup.time_s.len() && lookup.time_s[pos] == time {
                    record.push(lookup.vals[pos].to_string());
                    indices[idx] += 1;
                } else {
                    record.push(String::new());
                }
            }
            writer
                .write_record(record)
                .map_err(|e| format!("CSV write error: {e}"))?;
        }

        // Push to file
        writer
            .flush()
            .map_err(|e| format!("CSV write error: {e}"))?;
        Ok(())
    }
}

/// Find the next non-empty CSV record, returning a consistent error on EOF.
fn next_nonempty_record<R: std::io::Read>(
    records: &mut csv::StringRecordsIter<R>,
    empty_error: &str,
) -> Result<csv::StringRecord, String> {
    for result in records {
        let record = result.map_err(|e| e.to_string())?;
        if record.len() == 0 {
            continue;
        }
        return Ok(record);
    }
    Err(empty_error.to_string())
}
