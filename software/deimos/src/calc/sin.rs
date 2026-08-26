//! Derive input voltage from linear amplifier reading

use core::f64;

#[cfg(feature = "python")]
use pyo3::prelude::*;

use super::*;
use crate::{calc_names, py_json_methods};

/// Sin wave between `low` and `high` with a period of `period_s` and phase offset of `offset_s`
#[cfg_attr(feature = "python", pyclass)]
#[derive(Serialize, Deserialize, Default, Debug)]
pub struct Sin {
    // User inputs
    period_s: f64,
    offset_s: f64,
    low: f64,
    high: f64,
}

impl Sin {
    pub fn new(period_s: f64, offset_s: f64, low: f64, high: f64) -> Box<Self> {
        let (high, low) = (high.max(low), low.min(high));

        Box::new(Self {
            period_s,
            offset_s,
            low,
            high,
        })
    }
}

py_json_methods!(
    Sin,
    Calc,
    #[new]
    fn py_new(period_s: f64, offset_s: f64, low: f64, high: f64) -> Self {
        *Self::new(period_s, offset_s, low, high)
    }
);

#[typetag::serde]
impl Calc for Sin {
    fn init(&self, ctx: ControllerCtx) -> Result<CalcFn, String> {
        let rad_per_cycle = (ctx.dt_ns as f64 / 1e9) * 2.0 * f64::consts::PI / self.period_s;
        let mut angle_rad = self.offset_s * 2.0 * f64::consts::PI / self.period_s;
        let low = self.low.min(self.high);
        let scale = (self.high.max(self.low) - low) / 2.0;
        Ok(Box::new(move |_, outputs| {
            angle_rad += rad_per_cycle;
            outputs[0] = (angle_rad.sin() + 1.0) * scale + low;
            Ok(())
        }))
    }

    /// Map from input field names (like `v`, without prefix) to the state name
    /// that the input should draw from (like `peripheral_0.output_1`, with prefix)
    fn input_map(&self) -> BTreeMap<CalcInputName, FieldName> {
        BTreeMap::new()
    }

    calc_names!((), (y));
}
