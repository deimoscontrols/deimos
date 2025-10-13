//! A second-order Butterworth low-pass filter

use super::*;
use crate::{calc_config, calc_input_names, calc_output_names};
use flaw::{
    SisoIirFilter, butter2,
    generated::butter::butter2::{MAX_CUTOFF_RATIO, MIN_CUTOFF_RATIO},
};

/// Single-input, single-output Butterworth low-pass filter implemented with `flaw::butter2`
#[derive(Default, Serialize, Deserialize)]
pub struct Butter2 {
    // User inputs
    input_name: String,
    cutoff_hz: f64,
    save_outputs: bool,

    // Values provided by calc orchestrator during init
    #[serde(skip)]
    input_index: usize,

    #[serde(skip)]
    output_index: usize,

    // Internal state
    #[serde(skip)]
    filt: SisoIirFilter<2>,

    #[serde(skip)]
    initialized: bool,
}

impl core::fmt::Debug for Butter2 {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Butter")
            .field("input_name", &self.input_name)
            .field("cutoff_hz", &self.cutoff_hz)
            .field("save_outputs", &self.save_outputs)
            .finish()
    }
}

impl Butter2 {
    pub fn new(input_name: String, cutoff_hz: f64, save_outputs: bool) -> Self {
        let input_index = usize::MAX;
        let output_index = usize::MAX;

        Self {
            input_name,
            cutoff_hz,
            save_outputs,
            input_index,
            output_index,
            filt: SisoIirFilter::default(),
            initialized: false,
        }
    }
}

#[typetag::serde]
impl Calc for Butter2 {
    fn init(
        &mut self,
        ctx: ControllerCtx,
        input_indices: Vec<usize>,
        output_range: Range<usize>,
    ) -> Result<(), &'static str> {
        assert!(
            ctx.dt_ns > 0,
            "dt_ns value of {} provided. dt_ns must be > 0",
            ctx.dt_ns
        );

        self.input_index = input_indices[0];
        self.output_index = output_range.clone().next().unwrap();

        let sample_rate_hz = 1e9f64 / f64::from(ctx.dt_ns);
        let cutoff_ratio = (self.cutoff_hz / sample_rate_hz)
            .max(MIN_CUTOFF_RATIO)
            .min(MAX_CUTOFF_RATIO);

        let filter = butter2(cutoff_ratio).unwrap_or_else(|err| {
            panic!("Failed to construct butter2 filter for ratio {cutoff_ratio}: {err}")
        });

        self.filt = filter;
        self.initialized = false;
        Ok(())
    }

    fn terminate(&mut self) -> Result<(), &'static str> {
        self.input_index = usize::MAX;
        self.output_index = usize::MAX;
        self.filt = SisoIirFilter::default();
        self.initialized = false;
        Ok(())
    }

    fn eval(&mut self, tape: &mut [f64]) -> Result<(), &'static str> {
        let x = tape[self.input_index];
        let y = if branches::unlikely(!self.initialized) {
            // Pass through the first value to avoid excessive timing
            // on first cycle due to initialization
            self.filt.initialize(x as f32);
            self.initialized = true;
            x
        } else {
            self.filt.update(x as f32) as f64
        };
        tape[self.output_index] = y;
        Ok(())
    }

    fn get_input_map(&self) -> BTreeMap<CalcInputName, FieldName> {
        let mut map = BTreeMap::new();
        map.insert("x".to_owned(), self.input_name.clone());
        map
    }

    fn update_input_map(&mut self, field: &str, source: &str) -> Result<(), String> {
        if field == "x" {
            self.input_name = source.to_owned();
            Ok(())
        } else {
            Err(format!("Unrecognized field {field}"))
        }
    }

    calc_config!(cutoff_hz);
    calc_input_names!(x);
    calc_output_names!(y);
}
