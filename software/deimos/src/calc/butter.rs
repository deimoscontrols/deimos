//! A second-order Butterworth low-pass filter

#[cfg(feature = "python")]
use pyo3::prelude::*;

use super::*;
use crate::{calc_config, calc_input_names, calc_output_names, py_json_methods};
use deimos_numerics::{
    control::lti::butter,
    embedded::fixed::lti::{DeltaSos as FixedDeltaSos, DeltaSosState as FixedDeltaSosState},
};

const MAX_CUTOFF_RATIO: f64 = 0.4;

type Butter2Filter = FixedDeltaSos<f64, 1, 1>;
type Butter2FilterState = FixedDeltaSosState<f64, 1, 1>;

/// Single-input, single-output Butterworth low-pass filter.
#[cfg_attr(feature = "python", pyclass)]
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
    filt: Option<Butter2Filter>,

    #[serde(skip)]
    filt_state: Butter2FilterState,

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
    pub fn new(input_name: String, cutoff_hz: f64, save_outputs: bool) -> Box<Self> {
        let input_index = usize::MAX;
        let output_index = usize::MAX;

        Box::new(Self {
            input_name,
            cutoff_hz,
            save_outputs,
            input_index,
            output_index,
            filt: None,
            filt_state: Butter2FilterState::default(),
            initialized: false,
        })
    }
}

py_json_methods!(
    Butter2,
    Calc,
    #[new]
    fn py_new(input_name: String, cutoff_hz: f64, save_outputs: bool) -> Self {
        *Self::new(input_name, cutoff_hz, save_outputs)
    }
);

#[typetag::serde]
impl Calc for Butter2 {
    fn init(
        &mut self,
        ctx: ControllerCtx,
        input_indices: Vec<usize>,
        output_range: Range<usize>,
    ) -> Result<(), String> {
        assert!(
            ctx.dt_ns > 0,
            "dt_ns value of {} provided. dt_ns must be > 0",
            ctx.dt_ns
        );

        self.input_index = input_indices[0];
        self.output_index = output_range.clone().next().unwrap();

        let sample_rate_hz = 1e9f64 / f64::from(ctx.dt_ns);
        let cutoff_ratio = (self.cutoff_hz / sample_rate_hz).min(MAX_CUTOFF_RATIO);

        let filter = Butter2Filter::try_from(
            &butter::<2>(cutoff_ratio)
                .map_err(|err| format!("Failed to construct butter2 filter: {err}"))?,
        )
        .map_err(|err| format!("Failed to convert butter2 filter to fixed delta SOS: {err}"))?;

        self.filt_state = filter.reset_state();
        self.filt = Some(filter);
        self.initialized = false;
        Ok(())
    }

    fn terminate(&mut self) -> Result<(), String> {
        self.input_index = usize::MAX;
        self.output_index = usize::MAX;
        self.filt = None;
        self.filt_state = Butter2FilterState::default();
        self.initialized = false;
        Ok(())
    }

    fn eval(&mut self, tape: &mut [f64]) -> Result<(), String> {
        let x = tape[self.input_index];
        let filt = self
            .filt
            .as_ref()
            .ok_or_else(|| "Butter2 must be initialized before eval".to_string())?;
        let y = if branches::unlikely(!self.initialized) {
            // Pass through the first value to avoid excessive timing
            // on first cycle due to initialization
            filt.set_steady_state(&mut self.filt_state, [x]);
            self.initialized = true;
            x
        } else {
            filt.step(&mut self.filt_state, [x])[0]
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

    // FUTURE: passthrough — a filtered voltage is still a voltage. Resolving to the input
    // channel's unit requires `CalcOrchestrator` to pass channel units into `init`.
    fn get_output_units(&self) -> Vec<Option<String>> {
        vec![None]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::controller::context::ControllerCtx;

    /// Running `terminate()` then `init()` must reset `Butter2` state to the same baseline,
    /// so that two back-to-back sessions fed the same input sequence produce identical output.
    #[test]
    fn butter2_state_resets_across_terminate_init() {
        let ctx = ControllerCtx {
            dt_ns: 50_000_000, // 20 Hz sample rate
            ..Default::default()
        };

        let mut calc = Butter2::new("ignored".to_owned(), 5.0, true);

        // Input sits at tape[0], output at tape[1].
        let inputs: [f64; 8] = [1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0];
        let mut tape = [0.0f64; 2];

        let mut run = || -> Vec<f64> {
            calc.init(ctx.clone(), vec![0], 1..2).unwrap();
            let mut out = Vec::with_capacity(inputs.len());
            for &x in &inputs {
                tape[0] = x;
                calc.eval(&mut tape).unwrap();
                out.push(tape[1]);
            }
            calc.terminate().unwrap();
            out
        };

        let run1 = run();
        let run2 = run();

        assert_eq!(
            run1, run2,
            "Butter2 output must match bit-for-bit across terminate+init; \
             run1={run1:?} run2={run2:?}"
        );

        // Sanity check: the filter must actually do work (beyond just passing through the
        // first sample), otherwise the equality above is trivial.
        assert!(
            run1.iter().any(|&y| y != inputs[0]),
            "Butter2 output never deviated from the first input — filter appears inert"
        );
    }
}
