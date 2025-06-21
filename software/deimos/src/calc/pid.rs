//! A PID controller with simple saturation for anti-windup

use super::*;
use crate::{calc_config, calc_input_names, calc_output_names};

/// A PID controller with simple saturation for anti-windup
#[derive(Serialize, Deserialize, Default, Debug)]
pub struct Pid {
    // User inputs
    measurement_name: String,
    setpoint_name: String,
    kp: f64,
    ki: f64,
    kd: f64,
    max_integral: f64,
    save_outputs: bool,

    // Internal state
    err: f64,
    integral: f64,

    // Values provided by calc orchestrator during init
    dt_s: f64,

    #[serde(skip)]
    input_indices: Vec<usize>,

    #[serde(skip)]
    output_index: usize,
}

impl Pid {
    pub fn new(
        measurement_name: String,
        setpoint_name: String,
        kp: f64,
        ki: f64,
        kd: f64,
        max_integral: f64,
        save_outputs: bool,
    ) -> Self {
        let err = 0.0;
        let integral = 0.0;

        // These will be set during init.
        // Use default indices that will cause an error on the first call if not initialized properly
        let dt_s = 1.0;
        let input_indices = vec![];
        let output_index = usize::MAX;

        Self {
            measurement_name,
            setpoint_name,
            kp,
            ki,
            kd,
            max_integral,
            save_outputs,

            err,
            integral,

            dt_s,
            input_indices,
            output_index,
        }
    }
}

#[typetag::serde]
impl Calc for Pid {
    /// Reset internal state and register calc tape indices
    fn init(&mut self, ctx: ControllerCtx, input_indices: Vec<usize>, output_range: Range<usize>) {
        assert!(
            ctx.dt_ns > 0,
            "dt_ns value of {} provided. dt_ns must be > 0",
            ctx.dt_ns
        );

        self.dt_s = (ctx.dt_ns as f64) / 1e9;
        self.input_indices = input_indices;
        self.output_index = output_range.clone().next().unwrap();
    }

    fn terminate(&mut self) {
        self.err = 0.0;
        self.dt_s = 1.0;
        self.integral = 0.0;
        self.input_indices.clear();
        self.output_index = usize::MAX;
    }

    /// Run calcs for a cycle
    fn eval(&mut self, tape: &mut [f64]) {
        // Consume latest error estimate
        let meas = tape[self.input_indices[0]];
        let setpoint = tape[self.input_indices[1]];
        let new_error = meas - setpoint;
        let derivative = (new_error - self.err) / self.dt_s;
        self.err = new_error;
        self.integral += self.err * self.dt_s;

        // Anti-windup saturation
        self.integral = self.integral.min(self.max_integral).max(-self.max_integral);

        // Set the new output
        let y = self.kp * self.err + self.ki * self.integral + self.kd * derivative;
        tape[self.output_index] = y;
    }

    /// Map from input field names (like `v`, without prefix) to the state name
    /// that the input should draw from (like `peripheral_0.output_1`, with prefix)
    fn get_input_map(&self) -> BTreeMap<CalcInputName, FieldName> {
        let mut map = BTreeMap::new();
        map.insert("measurement".to_owned(), self.measurement_name.clone());
        map.insert("setpoint".to_owned(), self.setpoint_name.clone());
        map
    }

    /// Change a value in the input map
    fn update_input_map(&mut self, field: &str, source: &str) -> Result<(), String> {
        match field {
            "measurement" => self.measurement_name = source.to_owned(),
            "setpoint" => self.setpoint_name = source.to_owned(),
            _ => return Err(format!("Unrecognized field {field}")),
        }

        Ok(())
    }

    calc_config!(kp, ki, kd, max_integral);
    calc_input_names!(measurement, setpoint);
    calc_output_names!(y);
}
