//! Minimum and maximum reductions over a configurable set of channels.
//!
//! The convenience constructors in this module produce serializable calcs with
//! one output named `y`. Their input count and channel mappings are determined
//! by configuration, then fixed when the controller initializes a run.

use super::*;

/// Build a calc that returns the minimum value across `channels`.
///
/// Channel names are copied into the calc configuration in the order supplied.
/// At least one channel is required; an empty list is rejected when the
/// controller initializes. `NaN` inputs are ignored, and evaluation fails if
/// every input is `NaN`.
pub fn min(channels: &[&str]) -> Box<dyn Calc> {
    Box::new(Min {
        channels: owned_channels(channels),
    })
}

/// Build a calc that returns the maximum value across `channels`.
///
/// Channel names are copied into the calc configuration in the order supplied.
/// At least one channel is required; an empty list is rejected when the
/// controller initializes. `NaN` inputs are ignored, and evaluation fails if
/// every input is `NaN`.
pub fn max(channels: &[&str]) -> Box<dyn Calc> {
    Box::new(Max {
        channels: owned_channels(channels),
    })
}

/// Serializable configuration for a minimum reduction.
#[derive(Serialize, Deserialize, Debug)]
struct Min {
    /// Source fields in calc input order.
    channels: Vec<String>,
}

/// Serializable configuration for a maximum reduction.
#[derive(Serialize, Deserialize, Debug)]
struct Max {
    /// Source fields in calc input order.
    channels: Vec<String>,
}

#[typetag::serde]
impl Calc for Min {
    fn init(&self, _: ControllerCtx) -> Result<CalcFn, String> {
        validate_channels("Min", &self.channels)?;
        let mut all_nan_error = "Min received no non-NaN input values".to_owned();

        // All configuration and routing work is complete before this closure
        // enters the control loop, so evaluation only scans the input slice.
        // The terminal error is also prepared here to avoid allocating it in
        // the real-time loop.
        Ok(Box::new(move |inputs, outputs| {
            let Some(value) = minimum(inputs) else {
                return Err(std::mem::take(&mut all_nan_error));
            };
            outputs[0] = value;
            Ok(())
        }))
    }

    fn input_map(&self) -> BTreeMap<CalcInputName, FieldName> {
        reduction_input_map(&self.channels)
    }

    fn names(&self) -> (Vec<CalcInputName>, Vec<CalcOutputName>) {
        reduction_names(self.channels.len())
    }
}

#[typetag::serde]
impl Calc for Max {
    fn init(&self, _: ControllerCtx) -> Result<CalcFn, String> {
        validate_channels("Max", &self.channels)?;
        let mut all_nan_error = "Max received no non-NaN input values".to_owned();

        // The error string is allocated before the run and moved out only when
        // the evaluator reports its terminal all-NaN condition.
        Ok(Box::new(move |inputs, outputs| {
            let Some(value) = maximum(inputs) else {
                return Err(std::mem::take(&mut all_nan_error));
            };
            outputs[0] = value;
            Ok(())
        }))
    }

    fn input_map(&self) -> BTreeMap<CalcInputName, FieldName> {
        reduction_input_map(&self.channels)
    }

    fn names(&self) -> (Vec<CalcInputName>, Vec<CalcOutputName>) {
        reduction_names(self.channels.len())
    }
}

// Everything below this point supports the public constructors and calc
// implementations; none of it is part of the user-facing API.

/// Copy borrowed channel names into serializable calc configuration.
fn owned_channels(channels: &[&str]) -> Vec<String> {
    channels
        .iter()
        .map(|channel| (*channel).to_owned())
        .collect()
}

/// Generate stable local input names for the dynamically shaped calc.
fn input_names(count: usize) -> Vec<CalcInputName> {
    (0..count).map(|index| format!("v{index}")).collect()
}

/// Map each generated local input to its configured source field.
fn reduction_input_map(channels: &[String]) -> BTreeMap<CalcInputName, FieldName> {
    input_names(channels.len())
        .into_iter()
        .zip(channels.iter().cloned())
        .collect()
}

/// Describe the dynamic inputs and single `y` output to the orchestrator.
fn reduction_names(count: usize) -> (Vec<CalcInputName>, Vec<CalcOutputName>) {
    (input_names(count), vec!["y".to_owned()])
}

/// Reject empty reductions during initialization, outside the real-time loop.
fn validate_channels(kind: &str, channels: &[String]) -> Result<(), String> {
    if channels.is_empty() {
        Err(format!("{kind} requires at least one input channel"))
    } else {
        Ok(())
    }
}

/// Evaluate a minimum without allocating, skipping unordered inputs.
fn minimum(inputs: &[f64]) -> Option<f64> {
    let mut result = f64::INFINITY;
    let mut found = false;
    for value in inputs.iter().copied() {
        if value.is_nan() {
            continue;
        }
        found = true;
        result = result.min(value);
    }
    found.then_some(result)
}

/// Evaluate a maximum without allocating, skipping unordered inputs.
fn maximum(inputs: &[f64]) -> Option<f64> {
    let mut result = f64::NEG_INFINITY;
    let mut found = false;
    for value in inputs.iter().copied() {
        if value.is_nan() {
            continue;
        }
        found = true;
        result = result.max(value);
    }
    found.then_some(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn evaluate(calc: Box<dyn Calc>, inputs: &[f64]) -> f64 {
        let mut evaluator = calc.init(ControllerCtx::default()).unwrap();
        let mut output = [0.0];
        evaluator(inputs, &mut output).unwrap();
        output[0]
    }

    #[test]
    fn reductions_evaluate_all_channels() {
        let channel_names = ["a", "b", "c", "d"];
        let values = [8.0, 2.0, 6.0, 4.0];

        assert_eq!(evaluate(min(&channel_names), &values), 2.0);
        assert_eq!(evaluate(max(&channel_names), &values), 8.0);
    }

    #[test]
    fn reduction_inputs_follow_channel_order() {
        let calc = min(&["p.tc_0_K", "p.tc_1_K"]);

        assert_eq!(
            calc.names(),
            (vec!["v0".to_owned(), "v1".to_owned()], vec!["y".to_owned()])
        );
        assert_eq!(
            calc.input_map(),
            BTreeMap::from([
                ("v0".to_owned(), "p.tc_0_K".to_owned()),
                ("v1".to_owned(), "p.tc_1_K".to_owned()),
            ])
        );
    }

    #[test]
    fn reductions_reject_empty_channel_lists() {
        for calc in [min(&[]), max(&[])] {
            assert!(calc.init(ControllerCtx::default()).is_err());
        }
    }

    #[test]
    fn reductions_ignore_nan() {
        let channels = ["a", "b", "c"];
        let values = [1.0, f64::NAN, 3.0];

        assert_eq!(evaluate(min(&channels), &values), 1.0);
        assert_eq!(evaluate(max(&channels), &values), 3.0);
    }

    #[test]
    fn reductions_error_when_all_inputs_are_nan() {
        let channels = ["a", "b"];
        let values = [f64::NAN, f64::NAN];

        for calc in [min(&channels), max(&channels)] {
            let mut evaluator = calc.init(ControllerCtx::default()).unwrap();
            let mut output = [0.0];
            assert!(evaluator(&values, &mut output).is_err());
        }
    }

    #[test]
    fn reduction_calcs_round_trip_through_typetag() {
        for calc in [min(&["a", "b"]), max(&["a", "b"])] {
            let json = serde_json::to_string(&calc).unwrap();
            let restored: Box<dyn Calc> = serde_json::from_str(&json).unwrap();

            assert_eq!(restored.input_map(), calc.input_map());
            assert_eq!(restored.names(), calc.names());
        }
    }
}
