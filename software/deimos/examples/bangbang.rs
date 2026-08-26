//! A hysteretic bang-bang controller driving a Rev7 digital output.

use std::io::{self, Write};
use std::thread;
use std::time::Duration;

use deimos::calc::{Hysteretic, min};
use deimos::controller::context::ControllerCtx;
use deimos::peripheral::DeimosDaqRev7;
use deimos::{Controller, LoopMethod};

fn main() -> Result<(), String> {
    // Configure controller.
    let mut ctx = ControllerCtx::default();
    ctx.op_name = "bangbang_example".to_owned();
    ctx.op_dir = "./software/deimos/examples".into();
    ctx.dt_ns = (1e9 / 10.0) as u32; // 10 Hz internal cycle rate
    ctx.loop_method = LoopMethod::Efficient;

    // Build controller and associate DAQ(s).
    let mut controller = Controller::new(ctx);
    controller.add_peripheral("daq", Box::new(DeimosDaqRev7 { serial_number: 3 }))?;

    // Thermocouples fail high on open-circuit, so use the lower of TC0 and TC1 to
    // tolerate one open-circuit sensor.
    controller.add_calc("tc_min", min(&["daq.tc_0_K", "daq.tc_1_K"]));

    // Turn DO0 on above 90 K and off below 80 K.
    // `persistence=20` requires this condition to hold for 20 cycles (2 seconds)
    // before triggering a state change.
    controller.add_calc(
        "bangbang",
        Hysteretic::new_with_values("tc_min.y".to_owned(), 80.0, 90.0, 20, 0.0, 1.0)?,
    );
    controller.set_peripheral_input_source("daq.do0", "bangbang.y");

    // Filter the latest-value snapshot unfiltered and wait for its first sample.
    let mut run_handle = controller.run_nonblocking(None, None, true)?;

    // Periodically print the temperatures and output state.
    println!(""); // A blank line to be deleted on the first cycle.
    while run_handle.is_running() {
        // Wait
        thread::sleep(Duration::from_millis(100));
        if !run_handle.is_running() {
            break;
        }

        // Read values
        let values = run_handle.read().values;
        let tc0 = values["daq.tc_0_K"];
        let tc1 = values["daq.tc_1_K"];
        let do0 = values["daq.do0"];

        // Spam
        print!("\r\x1b[2K"); // Delete previous line
        print!("tc0: {tc0:.2} K    {tc1:.2} K    do0: {do0:.0}");
        let _ = io::stdout().flush();
    }

    println!();
    run_handle.join().map(|_| ())
}
