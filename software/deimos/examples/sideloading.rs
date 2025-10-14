//! Bypass nominal control program structure to deliver settings
//! and communication that are not explicitly supported.
//!
//! Demonstrated here:
//!   * Running a control program with no peripherals, only calcs
//!   * Using `user_ctx` and `user_channels` fields for sideloading
//!   * Defining custom calcs

// For definining calcs
use deimos::{
    calc::*,
    controller::channel::{Endpoint, Msg},
    dispatcher::fmt_time,
};

use serde::{Deserialize, Serialize};
use tracing::info;

use std::{
    collections::BTreeMap,
    ops::Range,
    time::{Duration, SystemTime},
};

// For using the controller
use controller::context::ControllerCtx;
use deimos::*;

fn main() {
    // Set control rate
    let rate_hz = 4.0;
    let dt_ns = (1e9_f64 / rate_hz).ceil() as u32;

    // Set termination criteria to end the control loop after a set duration from start of operating
    let termination_criteria = vec![Termination::Timeout(Duration::from_millis(500))];

    // Define idle controller
    let mut ctx = ControllerCtx::default();
    ctx.dt_ns = dt_ns;
    ctx.termination_criteria = termination_criteria;
    ctx.user_ctx
        .insert("speaker_prefix".to_owned(), "foobar".to_owned());
    let mut controller = Controller::new(ctx);

    // Clear default UDP socket, which we will not be using
    controller.clear_sockets();

    // Add calcs that use sideloading channel for comms.
    //
    // While they perform their communication on a channel, not via the calc tape,
    // they still have a dummy input-output relationship registered with the controller's
    // calc orchestrator in order to make sure that they are evaluated in the correct
    // order at each cycle.
    controller.add_calc("speaker", Box::new(Speaker::new("time channel")));
    controller.add_calc(
        "listener",
        Box::new(Listener::new("speaker.y", "time channel")),
    );

    // Serialize and deserialize the controller (for demonstration purposes)
    {
        let serialized_controller = serde_json::to_string_pretty(&controller).unwrap();
        let _: Controller = serde_json::from_str(&serialized_controller).unwrap();
    }

    // Run to planned termination
    controller.run(&None).unwrap();
}

/// A dummy calc that calls out the time on a channel each cycle
#[derive(Serialize, Deserialize, Default, Debug)]
pub struct Speaker {
    // User inputs
    channel_name: String,
    save_outputs: bool,

    #[serde(skip)]
    endpoint: Endpoint,

    prefix: String,

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
            prefix: "".to_owned(),
            output_index,
        }
    }
}

#[typetag::serde]
impl Calc for Speaker {
    /// Reset internal state and register calc tape indices
    fn init(
        &mut self,
        ctx: ControllerCtx,
        _input_indices: Vec<usize>,
        output_range: Range<usize>,
    ) -> Result<(), String> {
        self.output_index = output_range.clone().next().unwrap();
        self.endpoint = ctx.source_endpoint(&self.channel_name);
        self.prefix = ctx.user_ctx.get("speaker_prefix").unwrap().to_owned();
        Ok(())
    }

    fn terminate(&mut self) -> Result<(), String> {
        self.output_index = usize::MAX;
        self.endpoint = Endpoint::default();
        Ok(())
    }

    /// Run calcs for a cycle
    fn eval(&mut self, _tape: &mut [f64]) -> Result<(), String> {
        // Send time on user channel with prefix
        let msg = Msg::Str(format!(
            "{} at {:?}",
            &self.prefix,
            fmt_time(SystemTime::now())
        ));
        self.endpoint.tx().try_send(msg).unwrap(); // Will panic if buffer is full or channel is closed

        // We could write a dummy value to the tape here, but we don't need to
        Ok(())
    }

    /// Map from input field names (like `v`, without prefix) to the state name
    /// that the input should draw from (like `peripheral_0.output_1`, with prefix)
    fn get_input_map(&self) -> BTreeMap<CalcInputName, FieldName> {
        BTreeMap::new()
    }

    /// Change a value in the input map
    fn update_input_map(&mut self, field: &str, _source: &str) -> Result<(), String> {
        Err(format!("Unrecognized field {field}")) // there aren't any input fields
    }

    calc_config!();
    calc_input_names!();
    calc_output_names!(y);
}

/// A dummy calc that receives time from a listener and prints it to the terminal
#[derive(Serialize, Deserialize, Default, Debug)]
pub struct Listener {
    // User inputs
    // input_name: String,
    channel_name: String,
    save_outputs: bool,

    #[serde(skip)]
    endpoint: Endpoint,

    // Values provided by calc orchestrator during init
    #[serde(skip)]
    output_index: usize,
}

impl Listener {
    pub fn new(_input_name: &str, channel_name: &str) -> Self {
        // These will be set during init.
        // Use default indices that will cause an error on the first call if not initialized properly
        let output_index = usize::MAX;

        Self {
            // input_name: input_name.to_owned(),
            channel_name: channel_name.to_owned(),
            save_outputs: false,
            endpoint: Endpoint::default(),
            output_index,
        }
    }
}

#[typetag::serde]
impl Calc for Listener {
    /// Reset internal state and register calc tape indices
    fn init(
        &mut self,
        ctx: ControllerCtx,
        _input_indices: Vec<usize>,
        output_range: Range<usize>,
    ) -> Result<(), String> {
        self.output_index = output_range.clone().next().unwrap();
        self.endpoint = ctx.sink_endpoint(&self.channel_name);
        Ok(())
    }

    fn terminate(&mut self) -> Result<(), String> {
        self.output_index = usize::MAX;
        self.endpoint = Endpoint::default();
        Ok(())
    }

    /// Run calcs for a cycle
    fn eval(&mut self, _tape: &mut [f64]) -> Result<(), String> {
        // Print the time if we received it
        let msg = match self.endpoint.rx().try_recv() {
            Ok(x) => x,
            Err(_) => return Ok(()),
        };

        match msg {
            Msg::Str(s) => {
                info!("{s}");
            }
            x => panic!("Unexpected message type: {x:?}"),
        }

        // We could write a dummy value to the tape here, but we don't need to
        Ok(())
    }

    /// Map from input field names (like `v`, without prefix) to the state name
    /// that the input should draw from (like `peripheral_0.output_1`, with prefix)
    fn get_input_map(&self) -> BTreeMap<CalcInputName, FieldName> {
        BTreeMap::new()
    }

    /// Change a value in the input map
    fn update_input_map(&mut self, field: &str, _source: &str) -> Result<(), String> {
        Err(format!("Unrecognized field {field}")) // there aren't any input fields
    }

    calc_config!();
    calc_input_names!();
    calc_output_names!(y);
}
