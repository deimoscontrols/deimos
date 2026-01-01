//! A 1kHz control program with a single DAQ.
//!
//! Demonstrated here:
//!   * Setting up a simple control program and connecting to hardware
//!   * Storing data
//!   * Performing calculations in the loop
//!   * Serialization and deserialization of the control program

use crate::peripheral::AnalogIRev4;
use controller::context::ControllerCtx;
use deimos::*;

fn main() {
    // Define idle controller
    let mut ctx = ControllerCtx::default();
    ctx.op_name = "basic_example".into();
    ctx.dt_ns = (1e9_f64 / 1000.0).ceil() as u32; // 1000 Hz
    ctx.op_dir = "./software/deimos/examples".into();
    let mut controller = Controller::new(ctx);

    // Associate hardware peripherals
    controller.add_peripheral("p1", Box::new(AnalogIRev4 { serial_number: 1 }));

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
