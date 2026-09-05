//! Bypass nominal control program structure to deliver settings
//! and communication that are not explicitly supported.
//!
//! Demonstrated here:
//!   * Running a control program with no peripherals, only calcs
//!   * Using `user_ctx` and `user_channels` fields for sideloading
//!   * Defining custom calcs

// For definining calcs
use deimos::{calc::*, controller::channel::Msg};

use serde::{Deserialize, Serialize};

use std::{
    collections::BTreeMap,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

// For using the controller
use controller::context::ControllerCtx;
use deimos::*;

fn main() {
    // Set control rate
    let rate_hz = 4.0;
    let dt_ns = (1e9_f64 / rate_hz).ceil() as u32;

    // Set termination criteria to end the control loop after a set duration from start of operating
    let termination_criteria = Some(Termination::Timeout(Duration::from_millis(500)));

    // Define idle controller
    let mut ctx = ControllerCtx::default();
    ctx.dt_ns = dt_ns;
    ctx.termination_criteria = termination_criteria;
    ctx.user_ctx
        .insert("speaker_offset_s".to_owned(), "42.0".to_owned());
    let mut controller = Controller::new(ctx);

    // Clear default UDP socket, which we will not be using
    controller.clear_sockets();

    // Add calcs that use sideloading channel for comms.
    //
    // The speaker communicates on a channel rather than through the calc tape.
    // The listener exposes the most recently received value as a normal calc output.
    controller.add_calc("speaker", Box::new(Speaker::new("time channel")));
    controller.add_calc("listener", Box::new(Listener::new("time channel")));

    // Serialize and deserialize the controller (for demonstration purposes)
    {
        let serialized_controller = serde_json::to_string_pretty(&controller).unwrap();
        let _: Controller = serde_json::from_str(&serialized_controller).unwrap();
    }

    // Run to planned termination
    controller.run(&None, None).unwrap();
}

/// A dummy calc that calls out the time on a channel each cycle
#[derive(Serialize, Deserialize, Default, Debug)]
pub struct Speaker {
    channel_name: String,
}

impl Speaker {
    pub fn new(channel_name: &str) -> Self {
        Self {
            channel_name: channel_name.to_owned(),
        }
    }
}

#[typetag::serde]
impl Calc for Speaker {
    fn init(&self, ctx: ControllerCtx) -> Result<CalcFn, String> {
        let endpoint = ctx.source_endpoint(&self.channel_name);
        let offset_s = ctx
            .user_ctx
            .get("speaker_offset_s")
            .ok_or_else(|| "Missing `speaker_offset_s` user context".to_owned())?
            .parse::<f64>()
            .map_err(|err| format!("Invalid `speaker_offset_s`: {err}"))?;
        Ok(Box::new(move |_, _| {
            let now_s = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(f64::NAN, |duration| duration.as_secs_f64());
            let msg = Msg::Val(now_s + offset_s);
            let _ = endpoint.tx().try_send(msg);
            Ok(())
        }))
    }

    /// Map from input field names (like `v`, without prefix) to the state name
    /// that the input should draw from (like `peripheral_0.output_1`, with prefix)
    fn input_map(&self) -> BTreeMap<CalcInputName, FieldName> {
        BTreeMap::new()
    }

    calc_names!((), ());
}

/// A dummy calc that receives time from a speaker and exposes it as an output
#[derive(Serialize, Deserialize, Default, Debug)]
pub struct Listener {
    channel_name: String,
}

impl Listener {
    pub fn new(channel_name: &str) -> Self {
        Self {
            channel_name: channel_name.to_owned(),
        }
    }
}

#[typetag::serde]
impl Calc for Listener {
    fn init(&self, ctx: ControllerCtx) -> Result<CalcFn, String> {
        let endpoint = ctx.sink_endpoint(&self.channel_name);
        let mut received_time_s = f64::NAN;
        Ok(Box::new(move |_, outputs| {
            if let Ok(Msg::Val(value)) = endpoint.rx().try_recv() {
                received_time_s = value;
            }
            outputs[0] = received_time_s;
            Ok(())
        }))
    }

    /// Map from input field names (like `v`, without prefix) to the state name
    /// that the input should draw from (like `peripheral_0.output_1`, with prefix)
    fn input_map(&self) -> BTreeMap<CalcInputName, FieldName> {
        BTreeMap::new()
    }

    calc_names!((), (received_time_s));
}
