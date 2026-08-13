//! A 1kHz control program with a single DAQ.
//!
//! Demonstrated here:
//!   * Setting up a simple control program and connecting to hardware
//!   * Storing data
//!   * Performing calculations in the loop
//!   * Serialization and deserialization of the control program

use crate::peripheral::DeimosDaqRev7;
use controller::context::ControllerCtx;
use std::net::Ipv4Addr;
use std::sync::atomic::Ordering;
use std::time::{Duration, SystemTime};

use deimos::{dispatcher::ReportingDispatcher, *};

/// Multicast group used by the reporting dispatcher.
const MULTICAST_GROUP: Ipv4Addr = Ipv4Addr::new(239, 255, 0, 1);

/// UDP port used by the reporting dispatcher.
const MULTICAST_PORT: u16 = 29573;

fn main() {
    // Define idle controller
    let mut ctx = ControllerCtx::default();
    ctx.op_name = "basic_example".into();
    let rate_hz = 10.0;
    ctx.dt_ns = (1e9_f64 / rate_hz).ceil() as u32;
    ctx.op_dir = "./software/deimos/examples".into();
    ctx.loop_method = LoopMethod::Efficient;
    let mut controller = Controller::new(ctx);

    let reporting = ReportingDispatcher::new(
        MULTICAST_GROUP,
        MULTICAST_PORT,
        None,                   // outbound_interface: let the OS choose
        Duration::from_secs(2), // schema_period: re-emit Schema every 2 s
    );
    let dropped_handle = reporting.dropped_frames_handle();
    controller.add_dispatcher("reporting", reporting);

    // Associate hardware peripherals
    controller
        .add_peripheral("p1", Box::new(DeimosDaqRev7 { serial_number: 3 }))
        .unwrap();

    // Set up data targets
    let csv_dispatcher: Box<dyn Dispatcher> = CsvDispatcher::new(50, dispatcher::Overflow::Wrap);
    controller.add_dispatcher("csv", csv_dispatcher);

    // Serialize and deserialize the controller (for demonstration purposes)
    let serialized_controller = serde_json::to_string_pretty(&controller).unwrap();
    let _: Controller = serde_json::from_str(&serialized_controller).unwrap();
    // std::fs::write("./basic_example.json", &serialized_controller).unwrap();

    // Run control program
    controller.run(&None, None).unwrap();
}
