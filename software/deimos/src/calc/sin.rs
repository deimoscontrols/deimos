//! Derive input voltage from linear amplifier reading

use core::f64;

use super::*;
use crate::{calc_config, calc_input_names, calc_output_names};

/// Sin wave between `low` and `high` with a period of `period_s` and phase offset of `offset_s`
#[derive(Serialize, Deserialize, Default, Debug)]
pub struct Sin {
    // User inputs
    period_s: f64,
    offset_s: f64,
    low: f64,
    high: f64,
    save_outputs: bool,

    // Values provided by calc orchestrator during init
    #[serde(skip)]
    output_index: usize,

    #[serde(skip)]
    rad_per_cycle: f64,

    #[serde(skip)]
    angle_rad: f64,

    #[serde(skip)]
    scale: f64,
}

impl Sin {
    pub fn new(period_s: f64, offset_s: f64, low: f64, high: f64, save_outputs: bool) -> Self {
        // These will be set during init.
        // Use default indices that will cause an error on the first call if not initialized properly
        let output_index = usize::MAX;
        let rad_per_cycle = 0.0;
        let angle_rad = offset_s * 2.0 * f64::consts::PI / period_s; // Apply offset once to save cycles
        let (high, low) = (high.max(low), low.min(high));
        let scale = (high - low) / 2.0;

        Self {
            period_s,
            offset_s,
            low,
            high,
            save_outputs,

            output_index,
            rad_per_cycle,
            angle_rad,
            scale,
        }
    }
}

#[typetag::serde]
impl Calc for Sin {
    /// Reset internal state and register calc tape indices
    fn init(&mut self, ctx: ControllerCtx, _input_indices: Vec<usize>, output_range: Range<usize>) {
        self.output_index = output_range.clone().next().unwrap();
        self.rad_per_cycle = (ctx.dt_ns as f64 / 1e9) * 2.0 * f64::consts::PI / self.period_s;
    }

    fn terminate(&mut self) {
        self.output_index = usize::MAX;
        self.rad_per_cycle = 0.0;
        self.angle_rad = 0.0;
        self.scale = 0.0;
    }

    /// Run calcs for a cycle
    fn eval(&mut self, tape: &mut [f64]) {
        self.angle_rad += self.rad_per_cycle;
        let y = (self.angle_rad.sin() + 1.0) * self.scale + self.low;

        tape[self.output_index] = y;
    }

    /// Map from input field names (like `v`, without prefix) to the state name
    /// that the input should draw from (like `peripheral_0.output_1`, with prefix)
    fn get_input_map(&self) -> BTreeMap<CalcInputName, FieldName> {
        BTreeMap::new()
    }

    /// Change a value in the input map
    fn update_input_map(&mut self, _field: &str, _source: &str) -> Result<(), String> {
        Ok(())
    }

    calc_config!(period_s, offset_s, low, high);
    calc_input_names!();
    calc_output_names!(y);
}
