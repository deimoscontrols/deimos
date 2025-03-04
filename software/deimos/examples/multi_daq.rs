//! A 200Hz control program with two DAQ modules connected via UDP and time-synchronized.
//!
//! Demonstrated here:
//!   * Setting up a simple control program and connecting to hardware
//!   * Storing data
//!   * Performing calculations in the loop
//!   * Serialization and deserialization of the control program

use std::time::Duration;

use crate::calc::{Constant, Sin};
use crate::peripheral::{AnalogIRev3, AnalogIRev4};
use controller::context::ControllerCtx;
use deimos::*;

fn main() {
    // Set op name
    // None -> Let the controller set the name of the op automatically
    let op_name = std::fs::read_to_string("./op_name.tmp").unwrap_or_else(|_| {
        format!(
            "{:?}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos() as u64
        )
    });
    std::fs::write("./op_name.tmp", &op_name).unwrap();

    // Collect initalizers for custom peripherals, if needed
    let peripheral_plugins = None;

    // Set control rate
    let rate_hz = 200.0;
    let dt_ns = (1e9_f64 / rate_hz).ceil() as u32;

    // Define idle controller
    let mut ctx = ControllerCtx::default();
    ctx.op_name = op_name;
    ctx.dt_ns = dt_ns;
    let mut controller = Controller::new(ctx);

    // Scan for peripherals on LAN
    let scanned_peripherals = controller.scan(10, &peripheral_plugins);
    println!("Scan found: {scanned_peripherals:?}\n");

    // Associate peripherals
    controller.add_peripheral("p1", Box::new(AnalogIRev3 { serial_number: 1 }));
    controller.add_peripheral("p2", Box::new(AnalogIRev3 { serial_number: 2 }));
    controller.add_peripheral("p3", Box::new(AnalogIRev4 { serial_number: 1 }));
    controller.add_peripheral("p4", Box::new(AnalogIRev4 { serial_number: 2 }));
    controller.add_peripheral("p5", Box::new(AnalogIRev4 { serial_number: 3 }));
    controller.add_peripheral("p6", Box::new(AnalogIRev4 { serial_number: 4 }));

    // Set up database dispatchers
    let timescale_dispatcher: Box<dyn Dispatcher> = Box::new(TimescaleDbDispatcher::new(
        "tsdb",
        "/run/postgresql/", // Unix socket interface
        "jlogan",
        "POSTGRES_PW",
        Duration::from_nanos(1),
        1,
    ));
    let csv_dispatcher: Box<dyn Dispatcher> =
        Box::new(CsvDispatcher::new(50, dispatcher::Overflow::Wrap));
    controller.add_dispatcher(timescale_dispatcher);
    controller.add_dispatcher(csv_dispatcher);

    // Set up calc graph
    let duty = Constant::new(0.5, true);
    let freq = Sin::new(1.0 / (rate_hz / 100.0), 0.25, 100.0, 250_000.0, true);
    let freq1 = Sin::new(20.0, 0.25, 10.0, 200.0, true);
    controller.add_calc("duty", Box::new(duty));
    controller.add_calc("freq", Box::new(freq));
    controller.add_calc("freq1", Box::new(freq1));
    controller.set_peripheral_input_source("p1.pwm0_duty", "duty.y");
    controller.set_peripheral_input_source("p1.pwm0_freq", "freq.y");
    controller.set_peripheral_input_source("p1.pwm1_duty", "duty.y");
    controller.set_peripheral_input_source("p1.pwm1_freq", "freq1.y");
    controller.set_peripheral_input_source("p1.pwm3_duty", "duty.y");
    controller.set_peripheral_input_source("p1.pwm3_freq", "freq.y");

    // Serialize and deserialize the controller (for demonstration purposes)
    #[cfg(feature = "ser")]
    {
        let serialized_controller = serde_json::to_string_pretty(&controller).unwrap();
        let _: Controller = serde_json::from_str(&serialized_controller).unwrap();
    }

    // Run the control program
    println!("Starting controller");
    controller.run(&peripheral_plugins).unwrap();
}
