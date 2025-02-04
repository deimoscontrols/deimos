//! Bypass nominal control program structure to deliver settings
//! and communication that are not explicitly supported.
//!
//! Demonstrated here:
//!     * Running a control program with no peripherals, only calcs
//!     * Using `user_ctx` and `user_channels` for sideloading
//!     * Defining custom calcs

// For definining calcs
use std::{collections::BTreeMap, ops::{Deref, Range}};
use deimos::{calcs::*, controller::channel::{Channel, Endpoint}};
use serde::{Deserialize, Serialize};

// For using the controller
use controller::context::ControllerCtx;
use deimos::*;

fn main() {
    // Set control rate
    let rate_hz = 10.0;
    let dt_ns = (1e9_f64 / rate_hz).ceil() as u32;

    // Define idle controller
    let mut ctx = ControllerCtx::default();
    ctx.dt_ns = dt_ns;
    let mut controller = Controller::new(ctx);
}

/// A dummy calc that calls out the time on a channel each cycle
#[derive(Serialize, Deserialize, Default)]
pub struct Speaker {
    // User inputs
    channel_name: String,
    save_outputs: bool,
    endpoint: Endpoint,

    // Values provided by calc orchestrator during init
    #[serde(skip)]
    output_index: usize,
}

impl Speaker {
    pub fn new(channel_name: &str) -> Self {
        // These will be set during init.
        // Use default indices that will cause an error on the first call if not initialized properly
        let output_index = usize::MAX;

        Self {
            channel_name: channel_name.to_owned(),
            save_outputs: false,
            endpoint: Endpoint::default(),
            output_index,
        }
    }
}

#[typetag::serde]
impl Calc for Speaker {
    /// Reset internal state and register calc tape indices
    fn init(&mut self, ctx: ControllerCtx, _input_indices: Vec<usize>, output_range: Range<usize>) {
        self.output_index = output_range.clone().next().unwrap();
        self.endpoint = ctx.source_endpoint(&self.channel_name);
    }

    /// Run calcs for a cycle
    fn eval(&mut self, tape: &mut [f64]) {
        // Send time on user channel


        // We could write a dummy value here, but we don't need to
        // let y = 0.0;
        // tape[self.output_index] = y;
    }

    /// Map from input field names (like `v`, without prefix) to the state name
    /// that the input should draw from (like `peripheral_0.output_1`, with prefix)
    fn get_input_map(&self) -> BTreeMap<CalcInputName, FieldName> {
        BTreeMap::new()
    }

    /// Change a value in the input map
    fn update_input_map(&mut self, field: &str, _source: &str) -> Result<(), String> {
        // there aren't any input fields
        return Err(format!("Unrecognized field {field}"));
    }

    calc_config!();
    calc_input_names!();
    calc_output_names!(y);
}
