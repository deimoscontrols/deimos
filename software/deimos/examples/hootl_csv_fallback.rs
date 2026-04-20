//! HOOTL session wiring both `CsvDispatcher` and `TimescaleDbDispatcher`.
//!
//! This example exercises the `data-dispatch` spec's "emergency fallback" scenario:
//!
//! > WHEN a controller is configured with both a `TimescaleDbDispatcher` and a
//! > `CsvDispatcher`, and the database connection drops, THEN the `CsvDispatcher`
//! > MUST continue to record every subsequent row to disk.
//!
//! ## What this example actually reveals (spec gap)
//!
//! The current implementation does NOT satisfy the fallback scenario.  There are
//! two failure modes depending on where the DB failure occurs:
//!
//! 1. **Failure at `init`** — `Controller::run` calls `dispatcher.init(…).unwrap()`.
//!    If `TimescaleDbDispatcher::init` returns `Err` (e.g. the DB host is
//!    unreachable), the controller **panics**.  The CSV dispatcher is never given
//!    a chance to run.
//!
//! 2. **Failure at `consume`** — If the DB connection drops mid-run (worker thread
//!    crashes and the mpsc sender becomes disconnected), `TimescaleDbDispatcher::consume`
//!    returns `Err`.  The controller treats any dispatcher `Err` from `consume` as
//!    fatal: it collects all errors and returns `Err(String)`, terminating the session
//!    immediately.  CSV recording stops at the same cycle the DB error was observed.
//!
//! Both cases are flagged with `<!-- REVIEW: -->` in `specs/data-dispatch/spec.md`.
//!
//! ## How to run
//!
//! ```
//! # Expect a panic — this is the observed (non-spec-compliant) behavior.
//! RUST_LOG=info DEIMOS_DB_PW=ignored cargo run --example hootl_csv_fallback \
//!     --manifest-path software/deimos/Cargo.toml
//! ```
//!
//! The panic message will read something like:
//!   "called `Result::unwrap()` on an `Err` value: \"Failed to connect postgres client: …\""
//!
//! ## Confirming CSV-only still works
//!
//! Phase 2 of this example (run after the panic is caught) registers only the
//! `CsvDispatcher`.  It runs for 2 seconds and then counts the rows in the output
//! CSV.  The row count must equal the number of completed operating cycles
//! (`rate_hz * duration_secs`).

use std::fs;
use std::time::{Duration, SystemTime};

use deimos::{
    Controller, LoopMethod, Overflow, Termination, ThreadChannelSocket,
    controller::context::ControllerCtx,
    dispatcher::{CsvDispatcher, TimescaleDbDispatcher},
    peripheral::{DeimosDaqRev7, HootlTransport},
};

/// Rate and duration used for both test phases.
const RATE_HZ: f64 = 20.0;
const RUN_SECS: u64 = 2;

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
    controller
}

fn main() {
    if std::env::var("RUST_LOG").is_err() {
        unsafe { std::env::set_var("RUST_LOG", "info") };
    }

    // -----------------------------------------------------------------------
    // Phase 1: wire both CsvDispatcher and TimescaleDbDispatcher with a bogus
    // DB URL.  Expected result: PANIC at init (unwrap on Err).
    // -----------------------------------------------------------------------
    println!("=== Phase 1: Both dispatchers (bogus DB URL) ===");
    println!("Expected: panic because TimescaleDbDispatcher::init returns Err");
    println!("and the controller calls .unwrap() on that Err.");
    println!();

    // Supply a dummy password via env var so the init doesn't fail on the env
    // lookup itself before attempting the connection.
    unsafe { std::env::set_var("DEIMOS_DB_PW", "ignored") };

    let op_dir_phase1 = std::env::temp_dir().join("deimos_csv_fallback_phase1");
    fs::create_dir_all(&op_dir_phase1).expect("Failed to create temp op_dir for phase 1");

    let phase1_result = std::panic::catch_unwind(|| {
        let mut controller = make_controller("csv_fallback_p1", &op_dir_phase1);

        // CSV dispatcher — should be unaffected by the DB failure, but in
        // practice never initializes because the DB init panics first.
        let csv = CsvDispatcher::new(50, Overflow::Wrap);
        controller.add_dispatcher("csv", csv);

        // TSDB dispatcher with a deliberately invalid host / database.
        let tsdb = TimescaleDbDispatcher::new(
            "nope",                     // dbname
            "localhost:1",              // host — port 1 is almost certainly closed
            "invalid",                  // user
            "DEIMOS_DB_PW",             // token_name (env var set above to "ignored")
            Duration::ZERO,             // buffer_time
            Duration::from_secs(86400), // retention_time
        );
        controller.add_dispatcher("tsdb", tsdb);

        let end = Some(SystemTime::now() + Duration::from_secs(RUN_SECS + 5));
        let mut hootl_handle = controller
            .attach_hootl_driver("p1", HootlTransport::thread_channel("hootl_chan"), end)
            .expect("Failed to attach HOOTL driver");

        // This call is expected to panic inside Controller::run when it calls
        // TimescaleDbDispatcher::init(…).unwrap() and the DB is unreachable.
        let result = controller.run(&None, None);
        hootl_handle.join().expect("HOOTL runner thread panicked");
        result
    });

    match phase1_result {
        Err(_) => {
            println!("Phase 1 CONFIRMED: controller panicked on unreachable DB.");
            println!(
                "CSV rows in phase 1 op_dir: {}",
                count_csv_rows(&op_dir_phase1)
            );
            println!("(0 rows — CSV never ran because the panic pre-empted it.)");
        }
        Ok(Err(e)) => {
            println!("Phase 1: controller returned Err (not a panic): {e}");
        }
        Ok(Ok(_)) => {
            println!("Phase 1: controller ran successfully — unexpected with bogus DB URL.");
        }
    }

    println!();

    // -----------------------------------------------------------------------
    // Phase 2: only CsvDispatcher — confirm it records all rows cleanly.
    // -----------------------------------------------------------------------
    println!("=== Phase 2: CSV-only (no TSDB) ===");
    println!(
        "Expected: {RATE_HZ}Hz × {RUN_SECS}s = {} rows",
        expected_rows()
    );

    let op_dir_phase2 = std::env::temp_dir().join("deimos_csv_fallback_phase2");
    fs::create_dir_all(&op_dir_phase2).expect("Failed to create temp op_dir for phase 2");

    let mut controller = make_controller("csv_fallback_p2", &op_dir_phase2);
    let csv = CsvDispatcher::new(50, Overflow::Wrap);
    controller.add_dispatcher("csv", csv);

    let end = Some(SystemTime::now() + Duration::from_secs(RUN_SECS + 5));
    let mut hootl_handle = controller
        .attach_hootl_driver("p1", HootlTransport::thread_channel("hootl_chan"), end)
        .expect("Failed to attach HOOTL driver for phase 2");

    let result = controller.run(&None, None);
    hootl_handle
        .join()
        .expect("HOOTL runner thread panicked in phase 2");

    match result {
        Ok(_) => println!("Phase 2: controller exited cleanly."),
        Err(e) => eprintln!("Phase 2: controller exited with error: {e}"),
    }

    let rows = count_csv_rows(&op_dir_phase2);
    let expected = expected_rows();

    println!("Phase 2 CSV row count : {rows}");
    println!("Expected (approx)     : {expected}");
    println!("CSV dir               : {op_dir_phase2:?}");

    // Allow a small tolerance because the HOOTL driver may terminate slightly
    // before or after the controller timeout.
    let tolerance = (expected as f64 * 0.10) as usize + 2;
    if rows.abs_diff(expected) <= tolerance {
        println!("Phase 2 PASS: row count within tolerance of expected.");
    } else {
        eprintln!(
            "Phase 2 FAIL: row count {rows} is outside tolerance [{}, {}] of expected {expected}.",
            expected.saturating_sub(tolerance),
            expected + tolerance
        );
        std::process::exit(1);
    }

    println!();
    println!("=== Summary ===");
    println!("Spec gap confirmed: the 'emergency fallback' scenario in data-dispatch spec is NOT");
    println!(
        "satisfied. See <!-- REVIEW: --> annotation in specs/data-dispatch/spec.md for details."
    );
}

/// Count non-header data rows in any *.csv file under `dir`.
fn count_csv_rows(dir: &std::path::Path) -> usize {
    let Ok(entries) = fs::read_dir(dir) else {
        return 0;
    };
    entries
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|x| x == "csv"))
        .map(|e| {
            let content = fs::read_to_string(e.path()).unwrap_or_default();
            // Subtract 1 for the header row; ignore trailing empty lines.
            content
                .lines()
                .filter(|l| !l.trim().is_empty())
                .count()
                .saturating_sub(1)
        })
        .sum()
}

/// Approximate expected row count for the run.
fn expected_rows() -> usize {
    (RATE_HZ * RUN_SECS as f64) as usize
}
