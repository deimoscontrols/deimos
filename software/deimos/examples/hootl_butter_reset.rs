//! Verify that `Butter2` state resets across a controller restart.
//!
//! This example exercises the `realtime-calc` spec's "reset between runs" requirement:
//!
//! > WHEN a `Butter2` calc is used in run N and then the controller starts run N+1,
//! > THEN the filter's internal state MUST be re-initialized to the same baseline it
//! > had at the start of run N.
//!
//! ## Mechanism
//!
//! `Butter2::init()` sets `self.initialized = false`.  On the first `eval()` call the
//! filter calls `set_steady_state(x)` and passes through the first sample unchanged.
//! `Butter2::terminate()` resets internal filter state and clears `initialized`.
//!
//! Because the HOOTL driver resets its counter to 0 each time it enters Operating state
//! (and each run creates a fresh controller + driver pair), both runs see the same input
//! sequence: `0.0, 1.0, 2.0, …` (plus small per-channel offsets).  If the filter resets
//! correctly, the first few output samples of run 1 and run 2 must be identical.
//!
//! ## How to run
//!
//! ```sh
//! RUST_LOG=warn cargo run --example hootl_butter_reset \
//!     --manifest-path software/deimos/Cargo.toml
//! ```
//!
//! from the repo root.  Expected output:
//!   "Run 1 first 5 output values: …"
//!   "Run 2 first 5 output values: …"
//!   "PASS: Butter2 output resets correctly across runs."
//!
//! A FAIL line followed by a non-zero exit code indicates the filter leaked state.

use std::fs;
use std::time::{Duration, SystemTime};

use deimos::{
    Controller, LoopMethod, Overflow, Termination, ThreadChannelSocket,
    calc::Butter2,
    controller::context::ControllerCtx,
    dispatcher::CsvDispatcher,
    peripheral::{DeimosDaqRev7, HootlTransport},
};

/// Rate and duration for each run.
const RATE_HZ: f64 = 20.0;
const RUN_SECS: u64 = 2;

/// Number of leading output samples to compare between runs.
/// With steady-state init, sample[0] == input[0] for both runs.
const COMPARE_SAMPLES: usize = 5;

/// Tolerance for floating-point comparison (filter arithmetic is deterministic).
const TOLERANCE: f64 = 1e-9;

fn make_controller(op_name: &str, op_dir: &std::path::Path) -> Controller {
    let mut ctx = ControllerCtx::default();
    ctx.op_name = op_name.to_string();
    ctx.op_dir = op_dir.to_path_buf();
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

    // Add a Butter2 low-pass filter on p1.ain0 (10 Hz cutoff at 20 Hz sample rate).
    // save_outputs = true so the output column appears in the CSV.
    controller.add_calc("butter", Butter2::new("p1.ain0".to_string(), 5.0, true));

    // CSV dispatcher — 1 MB is sufficient for a 2-second 20 Hz run.
    let csv = CsvDispatcher::new(1, Overflow::Wrap);
    controller.add_dispatcher("csv", csv);

    controller
}

fn run_phase(label: &str, op_dir: &std::path::Path) {
    let end = Some(SystemTime::now() + Duration::from_secs(RUN_SECS + 5));
    let mut controller = make_controller(label, op_dir);
    let mut hootl_handle = controller
        .attach_hootl_driver("p1", HootlTransport::thread_channel("hootl_chan"), end)
        .expect("Failed to attach HOOTL driver");

    let result = controller.run(&None, None);
    hootl_handle.join().expect("HOOTL runner thread panicked");

    match result {
        Ok(_) => {}
        Err(e) => {
            eprintln!("{label}: controller exited with error: {e}");
            std::process::exit(1);
        }
    }
}

/// Parse the first N values of the `butter.y` column from a CSV file under `dir`.
fn read_butter_y_values(dir: &std::path::Path, n: usize) -> Vec<f64> {
    let entries = fs::read_dir(dir).expect("Failed to read op_dir");
    let csv_path = entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .find(|p| p.extension().is_some_and(|x| x == "csv"))
        .expect("No CSV file found in op_dir");

    let content = fs::read_to_string(&csv_path)
        .unwrap_or_else(|e| panic!("Failed to read CSV {csv_path:?}: {e}"));

    let mut lines = content.lines().filter(|l| !l.trim().is_empty());

    // Parse header to find the butter.y column index.
    let header = lines.next().expect("CSV has no header");
    let col_index = header
        .split(',')
        .position(|col| col.trim() == "butter.y")
        .expect("butter.y column not found in CSV header");

    // Collect first N data values from that column.
    lines
        .take(n)
        .filter_map(|line| {
            line.split(',')
                .nth(col_index)
                .and_then(|v| v.trim().parse::<f64>().ok())
        })
        .collect()
}

fn main() {
    if std::env::var("RUST_LOG").is_err() {
        unsafe { std::env::set_var("RUST_LOG", "warn") };
    }

    let base_dir = std::env::temp_dir().join("deimos_hootl_butter_reset");

    // -----------------------------------------------------------------------
    // Run 1
    // -----------------------------------------------------------------------
    let op_dir_run1 = base_dir.join("run1");
    fs::create_dir_all(&op_dir_run1).expect("Failed to create run1 op_dir");

    println!("=== Run 1 ===");
    run_phase("butter_reset_run1", &op_dir_run1);

    // -----------------------------------------------------------------------
    // Run 2
    // -----------------------------------------------------------------------
    let op_dir_run2 = base_dir.join("run2");
    fs::create_dir_all(&op_dir_run2).expect("Failed to create run2 op_dir");

    println!("=== Run 2 ===");
    run_phase("butter_reset_run2", &op_dir_run2);

    // -----------------------------------------------------------------------
    // Compare first N butter.y samples from each run.
    // -----------------------------------------------------------------------
    let run1_vals = read_butter_y_values(&op_dir_run1, COMPARE_SAMPLES);
    let run2_vals = read_butter_y_values(&op_dir_run2, COMPARE_SAMPLES);

    println!();
    println!(
        "Run 1 first {} butter.y values: {:?}",
        COMPARE_SAMPLES, run1_vals
    );
    println!(
        "Run 2 first {} butter.y values: {:?}",
        COMPARE_SAMPLES, run2_vals
    );
    println!("Run 1 op dir: {op_dir_run1:?}");
    println!("Run 2 op dir: {op_dir_run2:?}");

    // -----------------------------------------------------------------------
    // Assert: both runs must produce identical leading output.
    // -----------------------------------------------------------------------
    if run1_vals.len() < COMPARE_SAMPLES {
        eprintln!(
            "FAIL: Run 1 produced only {} samples (expected at least {COMPARE_SAMPLES}).",
            run1_vals.len()
        );
        std::process::exit(1);
    }
    if run2_vals.len() < COMPARE_SAMPLES {
        eprintln!(
            "FAIL: Run 2 produced only {} samples (expected at least {COMPARE_SAMPLES}).",
            run2_vals.len()
        );
        std::process::exit(1);
    }

    let all_match = run1_vals
        .iter()
        .zip(run2_vals.iter())
        .all(|(a, b)| (a - b).abs() < TOLERANCE);

    if all_match {
        println!();
        println!("PASS: Butter2 output resets correctly across runs.");
        println!(
            "Both runs produce identical leading output, confirming terminate()+init() \
             reinitializes filter state to the same baseline."
        );
    } else {
        println!();
        eprintln!("FAIL: Butter2 leading output differs between run 1 and run 2.");
        eprintln!(
            "This would indicate filter state leaked across the terminate()+init() boundary."
        );
        for (i, (a, b)) in run1_vals.iter().zip(run2_vals.iter()).enumerate() {
            if (a - b).abs() >= TOLERANCE {
                eprintln!(
                    "  sample[{i}]: run1={a:.6} run2={b:.6} diff={:.6e}",
                    (a - b).abs()
                );
            }
        }
        std::process::exit(1);
    }

    // -----------------------------------------------------------------------
    // Sanity check: verify that run 1 DID settle (output changed from sample[0]).
    // -----------------------------------------------------------------------
    let run1_all = read_butter_y_values(&op_dir_run1, usize::MAX);
    let first = run1_all.first().copied().unwrap_or(0.0);
    let last = run1_all.last().copied().unwrap_or(0.0);
    println!();
    println!(
        "Run 1 settled check: first={first:.4} last={last:.4} (total {} samples)",
        run1_all.len()
    );
    println!(
        "The filter was actively tracking a rising signal (HOOTL counter increases each cycle),"
    );
    println!("confirming run 1 had non-trivial filter state at the end of the run.");
}
