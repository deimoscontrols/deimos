//! Interactive HOOTL session with the reporting dispatcher.
//!
//! Runs a HOOTL controller for 30 seconds (or until Ctrl-C) and multicasts each
//! dispatched `Row` on the default group `239.255.0.1:29573`.  One channel has a
//! declared unit (`V`) so the viewer renders a unit-labeled axis.
//!
//! ## Two-terminal usage
//!
//! Run both commands from the **repo root**.
//!
//! **Terminal 1 — start the controller:**
//!
//! ```sh
//! RUST_LOG=info cargo run --example hootl_with_console \
//!     --manifest-path software/deimos/Cargo.toml
//! ```
//!
//! Expected output (in order):
//!
//! ```text
//! === Deimos HOOTL with operator console ===
//!
//! Reporting dispatcher: udp://239.255.0.1:29573
//! Schema period: 2 s  |  Rate: 20 Hz  |  Duration: 30 s
//!
//! To view in the operator console, run in another terminal:
//!
//!   cargo run -p deimos_console -- --config software/deimos_console/examples/console.toml
//!
//! Unit-labeled channel: voltage.y (unit: V)
//! Starting controller...
//!
//! INFO  Binding peripherals.
//! INFO  Configuring peripherals.
//! INFO  All peripherals acknowledged configuration.
//! INFO  Entering control loop.
//! ```
//!
//! The controller runs for 30 seconds, then prints dispatcher outcomes and exits.
//! Press Ctrl-C to stop earlier.
//!
//! **Terminal 2 — start the viewer:**
//!
//! Start this after Terminal 1 prints the multicast address.  The viewer will
//! buffer incoming `Row` packets until it receives a `Schema` packet (emitted by
//! the controller every 2 seconds while Operating), then begin rendering traces.
//!
//! ```sh
//! cargo run -p deimos_console -- \
//!     --config software/deimos_console/examples/console.toml
//! ```
//!
//! The viewer window opens immediately.  Channels appear once the first `Schema`
//! arrives (within 2 seconds of the controller entering Operating).  The status
//! bar shows connection health (fresh / stale / no-schema-yet) and a dropped-frame
//! counter.
//!
//! The `voltage.y` channel is listed in the "Currents and Voltages" panel of the
//! sample config; its axis is labeled `V` because the `Affine` calc declares that
//! unit via `with_output_unit("V")`.
//!
//! ## Channels produced
//!
//! - `p1.ain0` through `p1.ain7` — raw HOOTL counter values (no unit)
//! - `voltage.y` — `p1.ain0` scaled by 0.001 with output unit `"V"` (via `Affine`)
//!
//! The `voltage.y` channel is the one to look at first in the viewer: it has a
//! declared unit and the axis will be labeled `V`.
//!
//! ## Config file
//!
//! The viewer config is at `software/deimos_console/examples/console.toml`.  It
//! points at the same multicast group and port (`239.255.0.1:29573`) and defines
//! two panels.  Edit the `[[panels]]` entries to watch different channels.
//!
//! ## Forensic log
//!
//! The viewer writes a per-session CSV to the path configured in `console.toml`
//! (or a temp directory if unset), recording every received `Row` with viewer and
//! controller timestamps.  This log is independent of the controller's own CSV.

use std::net::Ipv4Addr;
use std::sync::atomic::Ordering;
use std::time::{Duration, SystemTime};

use deimos::{
    Controller, LoopMethod, Overflow, Termination, ThreadChannelSocket,
    calc::Affine,
    controller::context::ControllerCtx,
    dispatcher::{CsvDispatcher, ReportingDispatcher},
    peripheral::{DeimosDaqRev7, HootlTransport},
};

/// Multicast group used by the reporting dispatcher.
const MULTICAST_GROUP: Ipv4Addr = Ipv4Addr::new(239, 255, 0, 1);

/// UDP port used by the reporting dispatcher.
const MULTICAST_PORT: u16 = 29573;

/// How long to run before the controller stops automatically.
/// Set to a long value so the user can observe the viewer.  Send SIGINT (Ctrl-C)
/// to stop earlier.
const RUN_SECS: u64 = 30;

/// Reporting rate in Hz.
const RATE_HZ: f64 = 20.0;

fn main() {
    if std::env::var("RUST_LOG").is_err() {
        unsafe { std::env::set_var("RUST_LOG", "info") };
    }

    // Startup banner with connection info for the viewer.
    println!();
    println!("=== Deimos HOOTL with operator console ===");
    println!();
    println!(
        "Reporting dispatcher: udp://{}:{}",
        MULTICAST_GROUP, MULTICAST_PORT
    );
    println!(
        "Schema period: 2 s  |  Rate: {} Hz  |  Duration: {} s",
        RATE_HZ, RUN_SECS
    );
    println!();
    println!("To view in the operator console, run in another terminal:");
    println!();
    println!(
        "  cargo run -p deimos_console -- --config software/deimos_console/examples/console.toml"
    );
    println!();
    println!("Unit-labeled channel: voltage.y (unit: V)");
    println!("Starting controller...");
    println!();

    let op_dir = std::env::temp_dir().join("deimos_hootl_with_console");
    std::fs::create_dir_all(&op_dir).expect("Failed to create temp op_dir");

    let mut ctx = ControllerCtx::default();
    ctx.op_name = "hootl_with_console".to_string();
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
    controller
        .add_peripheral("p1", Box::new(DeimosDaqRev7 { serial_number: 1 }))
        .unwrap();

    // Add a unit-labeled calc: scale p1.ain0 by 0.001 and annotate the output
    // as volts.  The reporting dispatcher will include "V" in the Schema packet,
    // and the viewer will label the axis accordingly.
    let voltage_calc = Affine::new(
        "p1.ain0".to_string(), // input channel name
        0.001,                 // slope: HOOTL counter → ~millivolt-scale float
        0.0,                   // offset
        true,                  // save_outputs: include in dispatched rows
    )
    .with_output_unit("V");
    controller.add_calc("voltage", voltage_calc);

    // Reporting dispatcher: multicast every Row to the operator console. Clone the
    // dropped-frames handle before handing the dispatcher to the controller so we can
    // read the final count after `Controller::run` returns.
    let reporting = ReportingDispatcher::new(
        MULTICAST_GROUP,
        MULTICAST_PORT,
        None,                   // outbound_interface: let the OS choose
        Duration::from_secs(2), // schema_period: re-emit Schema every 2 s
    );
    let dropped_handle = reporting.dropped_frames_handle();
    controller.add_dispatcher("reporting", reporting);

    // CSV dispatcher: keep a local record alongside the multicast stream.
    let csv = CsvDispatcher::new(50, Overflow::Wrap);
    controller.add_dispatcher("csv", csv);

    let end = Some(SystemTime::now() + Duration::from_secs(RUN_SECS + 10));
    let mut hootl_handle = controller
        .attach_hootl_driver("p1", HootlTransport::thread_channel("hootl_chan"), end)
        .expect("Failed to attach HOOTL driver");

    let result = controller.run(&None, None);
    hootl_handle.join().expect("HOOTL runner thread panicked");

    // The dropped-frames check doubles as a smoke test: at 20 Hz the kernel UDP send
    // buffer should comfortably hold every non-blocking send_to even with no receiver
    // attached, so a non-zero count signals a regression (e.g. a smaller default
    // SO_SNDBUF on a new platform). Viewer-side drops are tracked separately.
    match result {
        Ok(_reason) => {
            println!();
            println!("Controller exited cleanly.");
        }
        Err(e) => {
            eprintln!("Controller exited with error: {e}");
            std::process::exit(1);
        }
    }

    let dropped = dropped_handle.load(Ordering::Relaxed);
    println!("ReportingDispatcher dropped_frames: {dropped}");
    if dropped != 0 {
        eprintln!("FAIL: expected dropped_frames == 0 at {RATE_HZ} Hz, got {dropped}.");
        std::process::exit(1);
    }
}
