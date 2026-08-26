//! A PID controller with simple saturation for anti-windup

#[cfg(feature = "python")]
use pyo3::prelude::*;

use super::*;
use crate::{calc_names, py_json_methods};

/// A PID controller with simple saturation for anti-windup
#[cfg_attr(feature = "python", pyclass)]
#[derive(Serialize, Deserialize, Default, Debug)]
pub struct Pid {
    // User inputs
    measurement_name: String,
    setpoint_name: String,
    kp: f64,
    ki: f64,
    kd: f64,
    max_integral: f64,
}

impl Pid {
    pub fn new(
        measurement_name: String,
        setpoint_name: String,
        kp: f64,
        ki: f64,
        kd: f64,
        max_integral: f64,
    ) -> Box<Self> {
        Box::new(Self {
            measurement_name,
            setpoint_name,
            kp,
            ki,
            kd,
            max_integral,
        })
    }
}

py_json_methods!(
    Pid,
    Calc,
    #[new]
    fn py_new(
        measurement_name: String,
        setpoint_name: String,
        kp: f64,
        ki: f64,
        kd: f64,
        max_integral: f64,
    ) -> Self {
        *Self::new(measurement_name, setpoint_name, kp, ki, kd, max_integral)
    }
);

#[typetag::serde]
impl Calc for Pid {
    fn init(&self, ctx: ControllerCtx) -> Result<CalcFn, String> {
        if ctx.dt_ns == 0 {
            return Err("Pid requires dt_ns to be greater than zero".to_owned());
        }
        let dt_s = f64::from(ctx.dt_ns) / 1e9;
        let (kp, ki, kd, max_integral) = (self.kp, self.ki, self.kd, self.max_integral);
        let mut err = 0.0;
        let mut integral = 0.0;
        Ok(Box::new(move |inputs, outputs| {
            let new_error = inputs[0] - inputs[1];
            let derivative = (new_error - err) / dt_s;
            err = new_error;
            integral += err * dt_s;
            integral = integral.min(max_integral).max(-max_integral);
            outputs[0] = kp * err + ki * integral + kd * derivative;
            Ok(())
        }))
    }

    /// Map from input field names (like `v`, without prefix) to the state name
    /// that the input should draw from (like `peripheral_0.output_1`, with prefix)
    fn input_map(&self) -> BTreeMap<CalcInputName, FieldName> {
        BTreeMap::from([
            ("measurement".to_owned(), self.measurement_name.clone()),
            ("setpoint".to_owned(), self.setpoint_name.clone()),
        ])
    }

    calc_names!((measurement, setpoint), (y));
}
