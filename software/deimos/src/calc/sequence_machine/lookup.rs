use interpn::one_dim::{Interp1D, RectilinearGrid1D};

use serde::{Deserialize, Serialize};

/// Interpolation method for sequence lookups
#[derive(Default, Debug, Serialize, Deserialize)]
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

    /// Simple string representation compatible with [Self::try_parse]
    pub fn to_str(&self) -> &'static str {
        match self {
            InterpMethod::Linear => "linear",
            InterpMethod::Left => "left",
            InterpMethod::Right => "right",
            InterpMethod::Nearest => "nearest",
        }
    }
}

/// A lookup table defining one sequenced output from a Sequence
#[derive(Serialize, Deserialize, Debug)]
pub struct SequenceLookup {
    /// Interpolation method
    pub(super) method: InterpMethod,

    /// Grid to interpolate on. Must be the same length as `vals`.
    pub(super) time_s: Vec<f64>,

    /// Values to interpolate. Must be the same length as `time_s`.
    pub(super) vals: Vec<f64>,
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

        // Run the interpolator at the first and last times
        let first_time = self
            .time_s
            .first()
            .ok_or_else(|| "Empty sequence".to_string())?;
        match self.eval_checked(*first_time) {
            Ok(_) => Ok(()),
            Err(x) => Err(x),
        }?;

        let last_time = self
            .time_s
            .last()
            .ok_or_else(|| "Empty sequence".to_string())?;
        match self.eval_checked(*last_time) {
            Ok(_) => Ok(()),
            Err(x) => Err(x),
        }?;

        Ok(())
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
