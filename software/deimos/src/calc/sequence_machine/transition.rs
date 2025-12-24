use serde::{Deserialize, Serialize};

use super::{CalcInputName, FieldName, SequenceLookup, StateName};

/// Choice of behavior when a given sequence reaches the end of its lookup table
#[derive(Debug, Serialize, Deserialize)]
#[non_exhaustive]
pub enum Timeout {
    /// Transition to the next sequence
    Transition(StateName),

    /// Start over from the beginning of the table
    Loop,

    /// Raise an error with a message
    Error(String),
}

impl Default for Timeout {
    fn default() -> Self {
        Self::Loop
    }
}

/// A logical operator used to evaluate whether a transition should occur.
#[derive(Serialize, Deserialize, Debug, Clone)]
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
#[derive(Serialize, Deserialize, Debug)]
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
