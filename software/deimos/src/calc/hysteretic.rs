//! A hysteretic bang-bang controller.

#[cfg(feature = "python")]
use pyo3::prelude::*;

use super::*;
use crate::{calc_names, py_json_methods};

/// The state machine output, together with the number of consecutive cycles
/// spent beyond the threshold that would change that output.
#[derive(Clone, Copy, Debug)]
enum State {
    InputHigh(u32),
    InputLow(u32),
}

impl Default for State {
    fn default() -> Self {
        Self::InputHigh(0)
    }
}

#[derive(Debug)]
struct Machine {
    low_thresh: f64,
    high_thresh: f64,
    persistence: u32,
    value_when_low: f64,
    value_when_high: f64,
    state: State,
}

impl Default for Machine {
    fn default() -> Self {
        Self::new(0.0, 0.0, 0, 1.0, 0.0)
    }
}

impl Machine {
    fn new(
        low_thresh: f64,
        high_thresh: f64,
        persistence: u32,
        value_when_low: f64,
        value_when_high: f64,
    ) -> Self {
        Self {
            low_thresh,
            high_thresh,
            persistence,
            value_when_low,
            value_when_high,
            state: State::default(),
        }
    }

    /// Advance the controller by one cycle using the latest input value.
    fn step(&mut self, v: f64) -> f64 {
        self.state = match self.state {
            State::InputHigh(count) if v < self.low_thresh => {
                let count = count.saturating_add(1);
                if count > self.persistence {
                    State::InputLow(0)
                } else {
                    State::InputHigh(count)
                }
            }
            State::InputHigh(_) => State::InputHigh(0),
            State::InputLow(count) if v > self.high_thresh => {
                let count = count.saturating_add(1);
                if count > self.persistence {
                    State::InputHigh(0)
                } else {
                    State::InputLow(count)
                }
            }
            State::InputLow(_) => State::InputLow(0),
        };

        match self.state {
            State::InputHigh(_) => self.value_when_high,
            State::InputLow(_) => self.value_when_low,
        }
    }
}

/// A bang-bang controller with separate on and off thresholds.
///
/// The output starts at `value_when_high`. It tolerates `persistence`
/// consecutive cycles below `low_thresh`, then changes to `value_when_low` on
/// the next such cycle. Likewise, it returns to `value_when_high` when the
/// consecutive cycles above `high_thresh` exceed `persistence`.
#[cfg_attr(feature = "python", pyclass)]
#[derive(Serialize, Deserialize, Debug)]
pub struct Hysteretic {
    // User inputs
    input_name: String,
    low_thresh: f64,
    high_thresh: f64,
    persistence: u32,
    value_when_low: f64,
    value_when_high: f64,
}

impl Default for Hysteretic {
    fn default() -> Self {
        Self {
            input_name: String::new(),
            low_thresh: 0.0,
            high_thresh: 0.0,
            persistence: 0,
            value_when_low: 1.0,
            value_when_high: 0.0,
        }
    }
}

impl Hysteretic {
    pub fn new(
        input_name: String,
        low_thresh: f64,
        high_thresh: f64,
        persistence: u32,
    ) -> Box<Self> {
        Box::new(Self {
            input_name,
            low_thresh,
            high_thresh,
            persistence,
            value_when_low: 1.0,
            value_when_high: 0.0,
        })
    }

    /// Construct a controller with custom low-state and high-state outputs.
    ///
    /// Returns an error if either output value is `NaN`.
    pub fn new_with_values(
        input_name: String,
        low_thresh: f64,
        high_thresh: f64,
        persistence: u32,
        value_when_low: f64,
        value_when_high: f64,
    ) -> Result<Box<Self>, String> {
        if value_when_low.is_nan() || value_when_high.is_nan() {
            return Err("Hysteretic output values must not be NaN".to_owned());
        }

        Ok(Box::new(Self {
            input_name,
            low_thresh,
            high_thresh,
            persistence,
            value_when_low,
            value_when_high,
        }))
    }

    fn validate_config(low_thresh: f64, high_thresh: f64) -> Result<(), String> {
        if !high_thresh.is_finite() || !low_thresh.is_finite() {
            return Err("Hysteretic thresholds must be finite".to_owned());
        }
        if low_thresh > high_thresh {
            return Err(format!(
                "Hysteretic low threshold ({low_thresh}) must not exceed high threshold ({high_thresh})"
            ));
        }
        Ok(())
    }
}

py_json_methods!(
    Hysteretic,
    Calc,
    #[new]
    fn py_new(input_name: String, low_thresh: f64, high_thresh: f64, persistence: u32) -> Self {
        *Self::new(input_name, low_thresh, high_thresh, persistence)
    },
    #[staticmethod]
    #[pyo3(name = "new_with_values")]
    fn py_new_with_values(
        input_name: String,
        low_thresh: f64,
        high_thresh: f64,
        persistence: u32,
        value_when_low: f64,
        value_when_high: f64,
    ) -> PyResult<Self> {
        Self::new_with_values(
            input_name,
            low_thresh,
            high_thresh,
            persistence,
            value_when_low,
            value_when_high,
        )
        .map(|calc| *calc)
        .map_err(pyo3::exceptions::PyValueError::new_err)
    }
);

#[typetag::serde]
impl Calc for Hysteretic {
    fn init(&self, _: ControllerCtx) -> Result<CalcFn, String> {
        Self::validate_config(self.low_thresh, self.high_thresh)?;
        let mut machine = Machine::new(
            self.low_thresh,
            self.high_thresh,
            self.persistence,
            self.value_when_low,
            self.value_when_high,
        );
        Ok(Box::new(move |inputs, outputs| {
            outputs[0] = machine.step(inputs[0]);
            Ok(())
        }))
    }

    fn input_map(&self) -> BTreeMap<CalcInputName, FieldName> {
        BTreeMap::from([("v".to_owned(), self.input_name.clone())])
    }

    calc_names!((v), (y));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::controller::context::ControllerCtx;

    #[test]
    fn switches_only_after_consecutive_persistent_samples() {
        let mut machine = Machine::new(2.0, 8.0, 3, 1.0, 0.0);

        assert_eq!(machine.step(1.0), 0.0);
        assert_eq!(machine.step(1.0), 0.0);
        assert_eq!(machine.step(3.0), 0.0); // Leaving the low region resets the count.
        assert_eq!(machine.step(1.0), 0.0);
        assert_eq!(machine.step(1.0), 0.0);
        assert_eq!(machine.step(1.0), 0.0);
        assert_eq!(machine.step(1.0), 1.0);

        assert_eq!(machine.step(9.0), 1.0);
        assert_eq!(machine.step(9.0), 1.0);
        assert_eq!(machine.step(7.0), 1.0); // Leaving the high region resets the count.
        assert_eq!(machine.step(9.0), 1.0);
        assert_eq!(machine.step(9.0), 1.0);
        assert_eq!(machine.step(9.0), 1.0);
        assert_eq!(machine.step(9.0), 0.0);
    }

    #[test]
    fn thresholds_are_strict_and_deadband_holds_output() {
        let mut machine = Machine::new(2.0, 8.0, 0, 1.0, 0.0);

        assert_eq!(machine.step(2.0), 0.0);
        assert_eq!(machine.step(1.0), 1.0);
        assert_eq!(machine.step(2.0), 1.0);
        assert_eq!(machine.step(5.0), 1.0);
        assert_eq!(machine.step(8.0), 1.0);
        assert_eq!(machine.step(9.0), 0.0);
    }

    #[test]
    fn zero_persistence_switches_immediately() {
        let calc = Hysteretic::new("source".to_owned(), 2.0, 8.0, 0);

        assert_eq!(calc.persistence, 0);
        let mut evaluator = calc.init(ControllerCtx::default()).unwrap();
        let mut outputs = [0.0];
        evaluator(&[1.0], &mut outputs).unwrap();
        assert_eq!(outputs[0], 1.0);
        evaluator(&[9.0], &mut outputs).unwrap();
        assert_eq!(outputs[0], 0.0);
    }

    #[test]
    fn supports_custom_and_inverted_output_values() {
        let calc =
            Hysteretic::new_with_values("source".to_owned(), 2.0, 8.0, 0, -4.0, 12.5).unwrap();
        let mut evaluator = calc.init(ControllerCtx::default()).unwrap();
        let mut outputs = [0.0];
        for (input, expected) in [(5.0, 12.5), (1.0, -4.0), (5.0, -4.0), (9.0, 12.5)] {
            evaluator(&[input], &mut outputs).unwrap();
            assert_eq!(outputs[0], expected);
        }
    }

    #[test]
    fn rejects_nan_output_values_during_construction() {
        for (value_when_low, value_when_high) in [(f64::NAN, 0.0), (1.0, f64::NAN)] {
            let error = Hysteretic::new_with_values(
                "source".to_owned(),
                2.0,
                8.0,
                0,
                value_when_low,
                value_when_high,
            )
            .err()
            .unwrap();

            assert_eq!(error, "Hysteretic output values must not be NaN");
        }
    }

    #[test]
    fn each_evaluator_has_fresh_state() {
        let calc = Hysteretic::new("source".to_owned(), 2.0, 8.0, 2);
        let mut outputs = [0.0];
        let mut first = calc.init(ControllerCtx::default()).unwrap();
        for expected in [0.0, 0.0, 1.0] {
            first(&[1.0], &mut outputs).unwrap();
            assert_eq!(outputs[0], expected);
        }

        let mut second = calc.init(ControllerCtx::default()).unwrap();
        second(&[1.0], &mut outputs).unwrap();
        assert_eq!(outputs[0], 0.0);
    }
}
