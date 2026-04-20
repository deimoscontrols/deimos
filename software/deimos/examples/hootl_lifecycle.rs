//! End-to-end HOOTL lifecycle verification.
//!
//! Runs a controller backed by a single `DeimosDaqRev7` HOOTL peripheral over an
//! in-process `ThreadChannelSocket`.  No OS network sockets or `/dev` nodes are
//! required.  The run terminates automatically after 3 seconds.
//!
//! Run with:
//!
//! ```
//! RUST_LOG=info cargo run --example hootl_lifecycle --manifest-path software/deimos/Cargo.toml
//! ```
//!
//! from the repo root.  Expected log output (in order):
//!   "Binding peripherals."
//!   "Configuring peripherals."
//!   "All peripherals acknowledged configuration."
//!   "Entering control loop."
//!
//! These correspond to the four lifecycle stages from the specs:
//!   Connecting (implicit initial state) → Binding → Configuring → OperatingRoundtrip.

use std::time::{Duration, SystemTime};

use deimos::{
    Controller, LoopMethod, Termination, ThreadChannelSocket,
    controller::context::ControllerCtx,
    dispatcher::CsvDispatcher,
    peripheral::{DeimosDaqRev7, HootlTransport},
};

fn main() {
    // Ensure RUST_LOG is set so lifecycle stage logs are visible.
    if std::env::var("RUST_LOG").is_err() {
        // Fall back to info-level if the caller didn't set RUST_LOG.
        unsafe { std::env::set_var("RUST_LOG", "info") };
    }

    // Build a temporary op directory so logging init doesn't fail.
    let op_dir = std::env::temp_dir().join("deimos_hootl_lifecycle");
    std::fs::create_dir_all(&op_dir).expect("Failed to create temp op_dir");

    // Configure a 20 Hz controller that terminates after 3 seconds.
    let rate_hz = 20.0;
    let mut ctx = ControllerCtx::default();
    ctx.op_name = "hootl_lifecycle".to_string();
    ctx.op_dir = op_dir;
    ctx.dt_ns = (1e9_f64 / rate_hz).ceil() as u32;
    ctx.loop_method = LoopMethod::Efficient;
    ctx.termination_criteria = Some(Termination::Timeout(Duration::from_secs(3)));

    let mut controller = Controller::new(ctx);

    // Replace the default UDP socket with an in-process thread-channel socket.
    controller.clear_sockets();
    controller.add_socket(
        "hootl_chan",
        Box::new(ThreadChannelSocket::new("hootl_chan")),
    );

    // Register a DeimosDaqRev7 peripheral (serial 1).
    controller.add_peripheral("p1", Box::new(DeimosDaqRev7 { serial_number: 1 }));

    // Attach a CSV dispatcher to record output rows.
    let csv_dispatcher = CsvDispatcher::new(50, deimos::Overflow::Wrap);
    controller.add_dispatcher("csv", csv_dispatcher);

    // Attach a HOOTL driver backed by the same thread-channel socket.
    // The driver will terminate itself when end is reached or the controller stops.
    let end = Some(SystemTime::now() + Duration::from_secs(5));
    let mut hootl_handle = controller
        .attach_hootl_driver("p1", HootlTransport::thread_channel("hootl_chan"), end)
        .expect("Failed to attach HOOTL driver");

    // Run the controller.  Expected lifecycle stages in the INFO logs:
    //   "Binding peripherals."        <- Binding stage
    //   "Configuring peripherals."    <- Configuring stage
    //   "All peripherals acknowledged configuration."
    //   "Entering control loop."      <- OperatingRoundtrip stage
    let result = controller.run(&None, None);

    // Wait for the HOOTL runner thread to finish.
    hootl_handle.join().expect("HOOTL runner thread panicked");

    match result {
        Ok(_) => println!("Controller exited cleanly."),
        Err(e) => eprintln!("Controller exited with error: {e}"),
    }
}
