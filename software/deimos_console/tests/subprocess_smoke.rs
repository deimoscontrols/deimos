//! End-to-end subprocess smoke test: bottle up the `hootl_with_console` two-terminal
//! invocation in a single `cargo test` command.
//!
//! Spawns the `deimos-console` viewer and the `hootl_with_console` controller example as
//! child processes, lets them run together for the example's hard-coded 30 s window, then
//! asserts:
//!
//! - the controller exits cleanly with `dropped_frames: 0`,
//! - the console emits a sustained per-second tick line at ~20 Hz with `stalls_detected=0`
//!   and zero entries on every drop counter,
//! - the console sees the controller's session-end schema (`health -> SessionEnded`).
//!
//! ## Gating
//!
//! `#[ignore]` because:
//!
//! 1. The test takes ~35 s end-to-end (controller has a hard-coded 30 s runtime).
//! 2. It opens an eframe window — requires a desktop session, fails in headless CI.
//! 3. It uses the production multicast group + port (`239.255.0.1:29573`), so concurrent
//!    runs on the same machine would collide.
//!
//! Run manually with:
//!
//! ```sh
//! cargo test -p deimos_console --test subprocess_smoke -- --ignored --nocapture
//! ```
//!
//! `--nocapture` is optional; the test panics with a descriptive message either way.
//!
//! ## Prerequisites
//!
//! Both binaries must be built ahead of the test. The test asserts the expected paths
//! exist and panics with a build command if they do not.

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Compile-time path to the `deimos-console` viewer binary.
const CONSOLE_BIN: &str = env!("CARGO_BIN_EXE_deimos-console");

/// How long to wait for the controller subprocess to exit on its own. The example is
/// hard-coded to 30 s; 60 s gives generous headroom for slow CI machines or first-run
/// delays.
const CONTROLLER_TIMEOUT: Duration = Duration::from_secs(60);

/// Path to the `hootl_with_console` example binary (workspace-root `target/debug/examples`).
fn controller_bin() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    // CARGO_MANIFEST_DIR = `<repo>/software/deimos_console`; the example lives at
    // `<repo>/target/debug/examples/hootl_with_console`.
    manifest
        .join("../../target/debug/examples/hootl_with_console")
        .canonicalize()
        .unwrap_or_else(|_| {
            panic!(
                "hootl_with_console example binary not found. Build it first:\n  \
                 cargo build --example hootl_with_console -p deimos"
            )
        })
}

/// Path to the operator console example config.
fn console_config() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest.join("examples/console.toml")
}

#[test]
#[ignore = "requires desktop session; takes ~35 s; collides with production multicast port"]
fn hootl_with_console_round_trips_cleanly() {
    let controller_path = controller_bin();
    let config_path = console_config();
    assert!(
        config_path.exists(),
        "console config not found at {}",
        config_path.display()
    );

    // Start the console first so it has joined the multicast group before the controller
    // begins emitting. Without this the first ~50 ms of Rows would arrive before the
    // receiver is bound, and the first sustained tick could miss a few frames.
    let mut console = Command::new(CONSOLE_BIN)
        .args(["--config", config_path.to_str().expect("config path utf8")])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn deimos-console");

    // 250 ms head-start: the receiver thread joins the multicast group and the eframe
    // window is allocated. Less than 250 ms occasionally lost the first Schema in manual
    // runs on cold caches.
    std::thread::sleep(Duration::from_millis(250));

    let controller_start = Instant::now();
    let mut controller = Command::new(&controller_path)
        // Keep RUST_LOG terse — the assertions look for specific stdout lines, not the
        // tracing output, so reducing log volume keeps the captured stream small.
        .env("RUST_LOG", "deimos=info")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn hootl_with_console");

    // Wait for the controller to exit on its own. The example has a 30 s timeout baked in
    // so this should return well within CONTROLLER_TIMEOUT.
    let controller_output = loop {
        if let Some(status) = controller.try_wait().expect("controller try_wait") {
            // Process exited; collect the captured output.
            let output = controller
                .wait_with_output()
                .expect("controller wait_with_output after exit");
            assert!(
                status.success(),
                "controller exited with non-zero status: {status:?}"
            );
            break output;
        }
        if controller_start.elapsed() > CONTROLLER_TIMEOUT {
            let _ = controller.kill();
            panic!(
                "controller did not exit within {CONTROLLER_TIMEOUT:?} (hard timeout — \
                 the example is hard-coded to ~30 s; suspect a hang)"
            );
        }
        std::thread::sleep(Duration::from_millis(200));
    };

    // Stop the console. SIGKILL is fine — we've already collected everything we need by
    // letting the controller drive the full 30 s session. The console's stderr buffer
    // contains the per-second ticks plus the session-end health transition.
    let _ = console.kill();
    let console_output = console
        .wait_with_output()
        .expect("console wait_with_output");

    let controller_stdout = String::from_utf8_lossy(&controller_output.stdout).into_owned();
    let console_stderr = String::from_utf8_lossy(&console_output.stderr).into_owned();

    // --- Controller side --------------------------------------------------------------

    assert!(
        controller_stdout.contains("Controller exited cleanly."),
        "controller stdout missing 'Controller exited cleanly.' line\n--- stdout ---\n{controller_stdout}"
    );
    assert!(
        controller_stdout.contains("ReportingDispatcher dropped_frames: 0"),
        "controller did not report zero dropped frames (regression on the send side)\n--- stdout ---\n{controller_stdout}"
    );

    // --- Console side -----------------------------------------------------------------

    // The viewer should have transitioned NoSchemaYet -> Fresh once the controller's first
    // schema arrived.
    assert!(
        console_stderr.contains("health -> Fresh"),
        "console never transitioned to Fresh — multicast delivery may have failed\n\
         --- stderr ---\n{console_stderr}"
    );

    // The session-end schema must have closed out the run on the viewer side.
    assert!(
        console_stderr.contains("health -> SessionEnded"),
        "console did not observe the controller's session-end schema\n\
         --- stderr ---\n{console_stderr}"
    );

    // Look for at least one tick line that shows the steady-state contract:
    //   - rate within [18.0, 21.0] Hz (configured 20 Hz, accept noise around setup/teardown)
    //   - every drop / stall counter still at zero
    //   - the new processor thread is keeping pending=0 (the core property of the recent
    //     decouple-console-buffer change)
    let mut good_tick = false;
    for line in console_stderr.lines() {
        if !line.contains("deimos-console: tick") {
            continue;
        }
        if !line.contains("wire_drops=0")
            || !line.contains("recv_drops=0")
            || !line.contains("overwritten_frames=0")
            || !line.contains("schema_drift_drops=0")
            || !line.contains("pending_overflow_drops=0")
            || !line.contains("stalls_detected=0")
            || !line.contains("stale_rows_evicted=0")
            || !line.contains("pending=0")
        {
            continue;
        }
        // Extract `rate=NN.NHz` and bounds-check.
        if let Some(rate) = parse_rate_hz(line)
            && (18.0..=21.0).contains(&rate)
        {
            good_tick = true;
            break;
        }
    }
    assert!(
        good_tick,
        "no per-second tick line met the steady-state contract \
         (rate in [18.0, 21.0] Hz, all counters at zero)\n\
         --- stderr ---\n{console_stderr}"
    );
}

/// Parse the `rate=NN.NHz` field out of a tick line, or `None` if the format drifts.
fn parse_rate_hz(line: &str) -> Option<f64> {
    let after = line.split_once("rate=")?.1;
    let num = after.split_once("Hz")?.0;
    num.parse().ok()
}
