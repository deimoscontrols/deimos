//! A second-order Butterworth low-pass filter

#[cfg(feature = "python")]
use pyo3::prelude::*;

use super::*;
use crate::{calc_names, py_json_methods};
use deimos_numerics::{control::lti::butter, embedded::fixed::lti::DeltaSos as FixedDeltaSos};

const MAX_CUTOFF_RATIO: f64 = 0.4;

type Butter2Filter = FixedDeltaSos<f64, 1, 1>;

/// Single-input, single-output Butterworth low-pass filter.
#[cfg_attr(feature = "python", pyclass)]
#[derive(Default, Serialize, Deserialize)]
pub struct Butter2 {
    input_name: String,
    cutoff_hz: f64,
}

impl core::fmt::Debug for Butter2 {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Butter")
            .field("input_name", &self.input_name)
            .field("cutoff_hz", &self.cutoff_hz)
            .finish()
    }
}

impl Butter2 {
    pub fn new(input_name: String, cutoff_hz: f64) -> Box<Self> {
        Box::new(Self {
            input_name,
            cutoff_hz,
        })
    }
}

py_json_methods!(
    Butter2,
    Calc,
    #[new]
    fn py_new(input_name: String, cutoff_hz: f64) -> Self {
        *Self::new(input_name, cutoff_hz)
    }
);

#[typetag::serde]
impl Calc for Butter2 {
    fn init(&self, ctx: ControllerCtx) -> Result<CalcFn, String> {
        if ctx.dt_ns == 0 {
            return Err("Butter2 requires dt_ns to be greater than zero".to_owned());
        }

        let sample_rate_hz = 1e9f64 / f64::from(ctx.dt_ns);
        let cutoff_ratio = (self.cutoff_hz / sample_rate_hz).min(MAX_CUTOFF_RATIO);

        let filter = Butter2Filter::try_from(
            &butter::<2>(cutoff_ratio)
                .map_err(|err| format!("Failed to construct butter2 filter: {err}"))?,
        )
        .map_err(|err| format!("Failed to convert butter2 filter to fixed delta SOS: {err}"))?;

        let mut filt_state = filter.reset_state();
        let mut initialized = false;
        Ok(Box::new(move |inputs, outputs| {
            let x = inputs[0];
            outputs[0] = if branches::unlikely(!initialized) {
                filter.set_steady_state(&mut filt_state, [x]);
                initialized = true;
                x
            } else {
                filter.step(&mut filt_state, [x])[0]
            };
            Ok(())
        }))
    }

    fn input_map(&self) -> BTreeMap<CalcInputName, FieldName> {
        BTreeMap::from([("x".to_owned(), self.input_name.clone())])
    }

    calc_names!((x), (y));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::controller::context::ControllerCtx;

    /// Each evaluator must begin with fresh filter state.
    #[test]
    fn butter2_evaluators_have_independent_state() {
        let ctx = ControllerCtx {
            dt_ns: 50_000_000, // 20 Hz sample rate
            ..Default::default()
        };

        let calc = Butter2::new("ignored".to_owned(), 5.0);

        let inputs: [f64; 8] = [1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0];

        let run = || -> Vec<f64> {
            let mut evaluator = calc.init(ctx.clone()).unwrap();
            let mut out = Vec::with_capacity(inputs.len());
            for &x in &inputs {
                let mut output = [0.0];
                evaluator(&[x], &mut output).unwrap();
                out.push(output[0]);
            }
            out
        };

        let run1 = run();
        let run2 = run();

        assert_eq!(
            run1, run2,
            "Butter2 output must match bit-for-bit across fresh evaluators; \
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
