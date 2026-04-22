//! HOOTL test: `ReportingDispatcher` drops no frames when the socket buffer is large enough.
//!
//! Registers a `ReportingDispatcher` (alongside a `CsvDispatcher`) and runs a HOOTL session at
//! 20 Hz for 2 seconds.  No multicast receiver is attached — the dispatcher multicasts into the
//! void.  Because the kernel UDP send buffer is large enough at this modest rate, every
//! non-blocking `send_to` succeeds and `dropped_frames` stays at 0.
//!
//! The test clones an `Arc<AtomicU64>` handle from the dispatcher via
//! `ReportingDispatcher::dropped_frames_handle()` *before* handing ownership to the controller.
//! After `Controller::run` returns (and `terminate` has been called), the handle is still live
//! and reflects the final count from the run.
//!
//! ## How to run
//!
//! ```
//! RUST_LOG=warn cargo run --example hootl_reporting \
//!     --manifest-path software/deimos/Cargo.toml
//! ```
//!
//! The example exits 0 if all assertions pass.

use std::fs;
use std::sync::atomic::Ordering;
use std::time::{Duration, SystemTime};

use deimos::{
    Controller, LoopMethod, Overflow, Termination, ThreadChannelSocket,
    controller::context::ControllerCtx,
    dispatcher::{CsvDispatcher, ReportingDispatcher},
    peripheral::{DeimosDaqRev7, HootlTransport},
};

/// Rate and duration for the test run.
const RATE_HZ: f64 = 20.0;
const RUN_SECS: u64 = 2;

fn main() {
    if std::env::var("RUST_LOG").is_err() {
        unsafe { std::env::set_var("RUST_LOG", "info") };
    }

    println!("=== ReportingDispatcher dropped_frames test ===");
    println!("Running at {RATE_HZ} Hz for {RUN_SECS} s with no multicast receiver.");
    println!("Expected: dropped_frames == 0 (socket buffer large enough for modest rate).");
    println!();

    let op_dir = std::env::temp_dir().join("deimos_hootl_reporting");
    fs::create_dir_all(&op_dir).expect("Failed to create temp op_dir");

    let mut ctx = ControllerCtx::default();
    ctx.op_name = "hootl_reporting".to_string();
    ctx.op_dir = op_dir;
    ctx.dt_ns = (1e9_f64 / RATE_HZ).ceil() as u32;
    ctx.loop_method = LoopMethod::Efficient;
    ctx.termination_criteria = Some(Termination::Timeout(Duration::from_secs(RUN_SECS)));

    let mut controller = Controller::new(ctx);
    controller.clear_sockets();
    controller.add_socket(
        "hootl_chan",
        Box::new(ThreadChannelSocket::new("hootl_chan")),
    );
    controller.add_peripheral("p1", Box::new(DeimosDaqRev7 { serial_number: 1 }));

    // Build the reporting dispatcher.  Clone a handle to the dropped-frames counter
    // before handing ownership to the controller — the controller consumes the Box,
    // but the Arc keeps the counter alive so we can read it after the run.
    //
    // Use the default multicast group (239.255.0.1) and port (29573).  No receiver
    // is listening; the kernel accepts the send silently.
    let reporting = ReportingDispatcher::new(
        std::net::Ipv4Addr::new(239, 255, 0, 1), // default multicast group
        29573,                                   // default port
        None,                                    // outbound_interface: let OS choose
        Duration::from_secs(2),                  // schema_period
    );
    let dropped_handle = reporting.dropped_frames_handle();
    controller.add_dispatcher("reporting", reporting);

    // Also add a CSV dispatcher so the controller has a data path even if reporting
    // were to fail init (though it shouldn't — binding a non-blocking UDP socket is
    // essentially infallible on a normal system).
    let csv = CsvDispatcher::new(50, Overflow::Wrap);
    controller.add_dispatcher("csv", csv);

    let end = Some(SystemTime::now() + Duration::from_secs(RUN_SECS + 5));
    let mut hootl_handle = controller
        .attach_hootl_driver("p1", HootlTransport::thread_channel("hootl_chan"), end)
        .expect("Failed to attach HOOTL driver");

    let result = controller.run(&None, None);
    hootl_handle.join().expect("HOOTL runner thread panicked");

    let _reason = result.expect("Controller::run returned Err");
    println!("PASS: reporting dispatcher ran without error.");

    // Assert zero dropped frames.
    let dropped = dropped_handle.load(Ordering::Relaxed);
    println!("dropped_frames: {dropped}");

    if dropped == 0 {
        println!("PASS: dropped_frames == 0.");
    } else {
        eprintln!("FAIL: expected dropped_frames == 0, got {dropped}.");
        std::process::exit(1);
    }

    println!();
    println!("=== All assertions passed ===");
    println!(
        "ReportingDispatcher sent every frame without blocking at {RATE_HZ} Hz × {RUN_SECS} s."
    );
}
