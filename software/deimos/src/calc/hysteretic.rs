//! A hysteretic bang-bang controller.

#[cfg(feature = "python")]
use pyo3::prelude::*;

use super::*;
use crate::{calc_input_names, calc_output_names, py_json_methods};

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

    fn reset(
        &mut self,
        low_thresh: f64,
        high_thresh: f64,
        persistence: u32,
        value_when_low: f64,
        value_when_high: f64,
    ) {
        *self = Self::new(
            low_thresh,
            high_thresh,
            persistence,
            value_when_low,
            value_when_high,
        );
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
    save_outputs: bool,

    // Internal state
    #[serde(skip)]
    machine: Machine,

    // Values provided by the calc orchestrator during init
    #[serde(skip)]
    input_index: usize,

    #[serde(skip)]
    output_index: usize,
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
            save_outputs: false,
            machine: Machine::default(),
            input_index: usize::MAX,
            output_index: usize::MAX,
        }
    }
}

impl Hysteretic {
    pub fn new(
        input_name: String,
        low_thresh: f64,
        high_thresh: f64,
        persistence: u32,
        save_outputs: bool,
    ) -> Box<Self> {
        Self::new_with_values(
            input_name,
            low_thresh,
            high_thresh,
            persistence,
            1.0,
            0.0,
            save_outputs,
        )
    }

    pub fn new_with_values(
        input_name: String,
        low_thresh: f64,
        high_thresh: f64,
        persistence: u32,
        value_when_low: f64,
        value_when_high: f64,
        save_outputs: bool,
    ) -> Box<Self> {
        Box::new(Self {
            input_name,
            low_thresh,
            high_thresh,
            persistence,
            value_when_low,
            value_when_high,
            save_outputs,
            machine: Machine::new(
                low_thresh,
                high_thresh,
                persistence,
                value_when_low,
                value_when_high,
            ),
            input_index: usize::MAX,
            output_index: usize::MAX,
        })
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
    fn py_new(
        input_name: String,
        low_thresh: f64,
        high_thresh: f64,
        persistence: u32,
        save_outputs: bool,
    ) -> Self {
        *Self::new(
            input_name,
            low_thresh,
            high_thresh,
            persistence,
            save_outputs,
        )
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
        save_outputs: bool,
    ) -> Self {
        *Self::new_with_values(
            input_name,
            low_thresh,
            high_thresh,
            persistence,
            value_when_low,
            value_when_high,
            save_outputs,
        )
    }
);

#[typetag::serde]
impl Calc for Hysteretic {
    fn init(
        &mut self,
        _: ControllerCtx,
        input_indices: Vec<usize>,
        output_range: Range<usize>,
    ) -> Result<(), String> {
        Self::validate_config(self.low_thresh, self.high_thresh)?;
        self.input_index = input_indices[0];
        self.output_index = output_range.clone().next().unwrap();
        self.machine.reset(
            self.low_thresh,
            self.high_thresh,
            self.persistence,
            self.value_when_low,
            self.value_when_high,
        );
        Ok(())
    }

    fn terminate(&mut self) -> Result<(), String> {
        self.machine.reset(
            self.low_thresh,
            self.high_thresh,
            self.persistence,
            self.value_when_low,
            self.value_when_high,
        );
        self.input_index = usize::MAX;
        self.output_index = usize::MAX;
        Ok(())
    }

    fn eval(&mut self, tape: &mut [f64]) -> Result<(), String> {
        let v = tape[self.input_index];
        tape[self.output_index] = self.machine.step(v);
        Ok(())
    }

    fn get_input_map(&self) -> BTreeMap<CalcInputName, FieldName> {
        BTreeMap::from([("v".to_owned(), self.input_name.clone())])
    }

    fn update_input_map(&mut self, field: &str, source: &str) -> Result<(), String> {
        if field == "v" {
            self.input_name = source.to_owned();
            Ok(())
        } else {
            Err(format!("Unrecognized field {field}"))
        }
    }

    fn get_save_outputs(&self) -> bool {
        self.save_outputs
    }

    fn set_save_outputs(&mut self, save_outputs: bool) {
        self.save_outputs = save_outputs;
    }

    calc_input_names!(v);
    calc_output_names!(y);
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
        let mut calc = Hysteretic::new("source".to_owned(), 2.0, 8.0, 0, false);

        assert_eq!(calc.persistence, 0);

        assert_eq!(calc.machine.step(1.0), 1.0);
        assert_eq!(calc.machine.step(9.0), 0.0);
    }

    #[test]
    fn supports_custom_and_inverted_output_values() {
        let mut calc =
            Hysteretic::new_with_values("source".to_owned(), 2.0, 8.0, 0, -4.0, 12.5, false);

        assert_eq!(calc.machine.step(5.0), 12.5);
        assert_eq!(calc.machine.step(1.0), -4.0);
        assert_eq!(calc.machine.step(5.0), -4.0);
        assert_eq!(calc.machine.step(9.0), 12.5);
    }

    #[test]
    fn calc_lifecycle_resets_the_machine() {
        let mut calc = Hysteretic::new("source".to_owned(), 2.0, 8.0, 2, false);
        let mut tape = [0.0; 2];

        calc.init(ControllerCtx::default(), vec![0], 1..2).unwrap();
        tape[0] = 1.0;
        calc.eval(&mut tape).unwrap();
        assert_eq!(tape[1], 0.0);
        calc.eval(&mut tape).unwrap();
        assert_eq!(tape[1], 0.0);
        calc.eval(&mut tape).unwrap();
        assert_eq!(tape[1], 1.0);

        calc.terminate().unwrap();
        calc.init(ControllerCtx::default(), vec![0], 1..2).unwrap();
        calc.eval(&mut tape).unwrap();
        assert_eq!(tape[1], 0.0);
    }
}
