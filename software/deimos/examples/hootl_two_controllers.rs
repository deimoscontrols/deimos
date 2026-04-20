//! Verify that two controllers with disjoint peripheral sets each ignore the other's peripherals.
//!
//! This example exercises the `controller-orchestration` spec's subset-operation requirement:
//!
//! > WHEN two controllers share one Ethernet segment and each owns a different subset of DAQs,
//! > THEN each controller MUST operate only its configured peripherals and MUST NOT error when
//! > it sees the others respond to broadcasts.
//!
//! ## HOOTL interpretation
//!
//! On real hardware, two controllers share one physical network. In HOOTL, there is no shared
//! network: each `ThreadChannelSocket` is a private 1-to-1 channel between a single controller
//! and a single HOOTL driver. The structural isolation is therefore total by construction — a
//! `ThreadChannelSocket` named `"hootl_a"` can only receive packets from the HOOTL driver that
//! opened the matching sink endpoint on the same controller context.
//!
//! What this example verifies instead is the **behavioral half** of the spec requirement: that
//! each controller's channel list (the set of fields it dispatches) contains only the peripheral
//! it was configured with, and contains no channels from the other controller's peripheral.
//! Running both controllers concurrently on overlapping threads confirms they do not interfere.
//!
//! <!-- REVIEW: subset-operation scenario — the "shared transport" half of the spec requirement
//!   ("each controller MUST NOT error when it sees the others respond to broadcasts") cannot be
//!   exercised in HOOTL. ThreadChannelSocket provides one private channel per peripheral; there
//!   is no mechanism to route a second controller's bind broadcasts to a peripheral that is
//!   already in Operating state with the first controller. Verifying broadcast-ignore behavior
//!   requires either real hardware (Ethernet) or a future multi-controller HOOTL transport that
//!   lets multiple controllers share one in-process channel hub. -->
//!
//! ## How to run
//!
//! ```sh
//! RUST_LOG=warn cargo run --example hootl_two_controllers \
//!     --manifest-path software/deimos/Cargo.toml
//! ```
//!
//! from the repo root.  Expected output:
//!   "Controller A channels: [...]"   — contains p1.* columns, no p2.* columns
//!   "Controller B channels: [...]"   — contains p2.* columns, no p1.* columns
//!   "PASS: each controller's channel set is confined to its own peripheral."

use std::time::{Duration, SystemTime};

use deimos::{
    Controller, LoopMethod, Termination, ThreadChannelSocket,
    controller::context::ControllerCtx,
    peripheral::{DeimosDaqRev7, HootlTransport},
};

const RATE_HZ: f64 = 20.0;
const RUN_SECS: u64 = 2;

/// Build one controller owning a single peripheral on its own named channel.
fn build_controller(
    op_name: &str,
    op_dir: &std::path::Path,
    peripheral_name: &str,
    serial_number: u64,
    channel_name: &str,
) -> Controller {
    let mut ctx = ControllerCtx::default();
    ctx.op_name = op_name.to_string();
    ctx.op_dir = op_dir.to_path_buf();
    ctx.dt_ns = (1e9_f64 / RATE_HZ).ceil() as u32;
    ctx.loop_method = LoopMethod::Efficient;
    ctx.termination_criteria = Some(Termination::Timeout(Duration::from_secs(RUN_SECS)));

    let mut controller = Controller::new(ctx);
    controller.clear_sockets();
    controller.add_socket(
        channel_name,
        Box::new(ThreadChannelSocket::new(channel_name)),
    );
    controller.add_peripheral(peripheral_name, Box::new(DeimosDaqRev7 { serial_number }));

    controller
}

fn main() {
    if std::env::var("RUST_LOG").is_err() {
        unsafe { std::env::set_var("RUST_LOG", "warn") };
    }

    let base_dir = std::env::temp_dir().join("deimos_hootl_two_controllers");
    let op_dir_a = base_dir.join("ctrl_a");
    let op_dir_b = base_dir.join("ctrl_b");
    std::fs::create_dir_all(&op_dir_a).expect("Failed to create ctrl_a op_dir");
    std::fs::create_dir_all(&op_dir_b).expect("Failed to create ctrl_b op_dir");

    // -----------------------------------------------------------------------
    // Build controller A (owns peripheral "p1", serial 1) on channel "hootl_a".
    // -----------------------------------------------------------------------
    let end = Some(SystemTime::now() + Duration::from_secs(RUN_SECS + 5));

    let mut ctrl_a = build_controller("ctrl_a", &op_dir_a, "p1", 1, "hootl_a");
    let mut hootl_a = ctrl_a
        .attach_hootl_driver("p1", HootlTransport::thread_channel("hootl_a"), end)
        .expect("Failed to attach HOOTL driver for ctrl_a");

    // -----------------------------------------------------------------------
    // Build controller B (owns peripheral "p2", serial 2) on channel "hootl_b".
    // -----------------------------------------------------------------------
    let mut ctrl_b = build_controller("ctrl_b", &op_dir_b, "p2", 2, "hootl_b");
    let mut hootl_b = ctrl_b
        .attach_hootl_driver("p2", HootlTransport::thread_channel("hootl_b"), end)
        .expect("Failed to attach HOOTL driver for ctrl_b");

    // -----------------------------------------------------------------------
    // Run both controllers concurrently via the non-blocking API.
    // -----------------------------------------------------------------------
    let mut handle_a = ctrl_a
        .run_nonblocking(None, None, true)
        .expect("Controller A failed to start");

    let mut handle_b = ctrl_b
        .run_nonblocking(None, None, true)
        .expect("Controller B failed to start");

    println!("Both controllers running. Waiting {} s...", RUN_SECS);
    std::thread::sleep(Duration::from_secs(RUN_SECS));

    // -----------------------------------------------------------------------
    // Capture channel lists before stopping (they become empty after join).
    // -----------------------------------------------------------------------
    let headers_a = handle_a.headers();
    let headers_b = handle_b.headers();

    // -----------------------------------------------------------------------
    // Stop both controllers and join threads.
    // -----------------------------------------------------------------------
    handle_a.stop();
    handle_b.stop();

    match handle_a.join() {
        Ok(_) => {}
        Err(e) => {
            eprintln!("Controller A exited with error: {e}");
            std::process::exit(1);
        }
    }
    match handle_b.join() {
        Ok(_) => {}
        Err(e) => {
            eprintln!("Controller B exited with error: {e}");
            std::process::exit(1);
        }
    }

    hootl_a.join().expect("HOOTL runner A thread panicked");
    hootl_b.join().expect("HOOTL runner B thread panicked");

    // -----------------------------------------------------------------------
    // Verify peripheral isolation: channel headers are the observable contract.
    //
    // Controller A should have "p1.*" channels and no "p2.*" channels.
    // Controller B should have "p2.*" channels and no "p1.*" channels.
    // -----------------------------------------------------------------------
    println!("Controller A channels: {headers_a:?}");
    println!("Controller B channels: {headers_b:?}");

    // Strip the two leading metadata columns ("timestamp", "time").
    let data_a: Vec<&str> = headers_a.iter().skip(2).map(String::as_str).collect();
    let data_b: Vec<&str> = headers_b.iter().skip(2).map(String::as_str).collect();

    let a_has_p1 = data_a.iter().any(|c| c.starts_with("p1."));
    let a_has_p2 = data_a.iter().any(|c| c.starts_with("p2."));
    let b_has_p2 = data_b.iter().any(|c| c.starts_with("p2."));
    let b_has_p1 = data_b.iter().any(|c| c.starts_with("p1."));

    let mut failed = false;

    if !a_has_p1 {
        eprintln!("FAIL: Controller A has no p1.* channels.");
        failed = true;
    }
    if a_has_p2 {
        eprintln!("FAIL: Controller A exposes p2.* channels — peripheral sets are NOT disjoint.");
        failed = true;
    }
    if !b_has_p2 {
        eprintln!("FAIL: Controller B has no p2.* channels.");
        failed = true;
    }
    if b_has_p1 {
        eprintln!("FAIL: Controller B exposes p1.* channels — peripheral sets are NOT disjoint.");
        failed = true;
    }

    if failed {
        std::process::exit(1);
    }

    println!();
    println!("PASS: each controller's channel set is confined to its own peripheral.");
    println!(
        "Controller A dispatched {} data channels (p1.*); no p2.* channels present.",
        data_a.len()
    );
    println!(
        "Controller B dispatched {} data channels (p2.*); no p1.* channels present.",
        data_b.len()
    );
    println!();
    println!(
        "NOTE: ThreadChannelSocket provides private 1-to-1 channels — there is no shared \
         transport in HOOTL. The 'broadcast-ignore' half of the spec requirement requires \
         real hardware or a future multi-controller HOOTL transport. See inline REVIEW \
         comment in this file."
    );
}
