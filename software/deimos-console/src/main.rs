mod app;
mod config;
mod forensic;
mod receiver;

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Parser;

use app::DeimosConsoleApp;
use config::DeimosConsoleConfig;

/// Operator console viewer for Deimos realtime reporting.
///
/// Subscribes to a UDP multicast stream published by the Deimos reporting dispatcher,
/// renders scrolling scope traces for configured channel groups, and writes a per-session
/// forensic CSV log so operators can reconstruct exactly what was on screen at any moment.
///
/// The console connects automatically when the controller starts Operating and recovers
/// gracefully when it restarts (buffering Row packets until the next Schema is received).
/// Connection health is shown in the status bar; use the Freeze button to inspect a moment
/// in time without interrupting data collection.
#[derive(Parser)]
#[command(
    name = "deimos-console",
    about = "Operator console viewer for Deimos realtime reporting",
    long_about = "Subscribes to a UDP multicast stream published by the Deimos reporting \
dispatcher, renders scrolling scope traces for configured channel groups, and writes a \
per-session forensic CSV log. Accepts a TOML config file that specifies the multicast \
address/port, display panels, and optional forensic log path. See the crate README for \
the full config-file format and field descriptions."
)]
struct Cli {
    /// Path to the TOML configuration file.
    ///
    /// The file must contain at minimum `multicast_group` and `port` fields.
    /// All other fields are optional and fall back to documented defaults.
    /// See the crate README or `examples/console.toml` for a complete example.
    #[arg(long, short, value_name = "FILE")]
    config: PathBuf,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    let config_bytes = std::fs::read_to_string(&cli.config)
        .with_context(|| format!("failed to read config file: {}", cli.config.display()))?;

    let config: DeimosConsoleConfig = toml::from_str(&config_bytes)
        .with_context(|| format!("failed to parse config file: {}", cli.config.display()))?;

    eprintln!(
        "[{}] deimos-console: starting, listening on {}:{} (forensic_log={})",
        chrono::Local::now().format("%H:%M:%S%.3f"),
        config.multicast_group,
        config.port,
        config
            .forensic_log_path
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "<disabled>".to_string()),
    );

    // Spawn the UDP multicast receiver thread. The join handle is intentionally dropped here;
    // the thread exits when the process terminates or on a non-transient socket error.
    let (_tx, rx, _recv_handle) =
        receiver::spawn(&config).with_context(|| "failed to start multicast receiver thread")?;

    let options = eframe::NativeOptions::default();

    eframe::run_native(
        "Deimos Console",
        options,
        Box::new(|_cc| Ok(Box::new(DeimosConsoleApp::new(config, rx)))),
    )
    .map_err(|e| anyhow::anyhow!("eframe error: {e}"))?;

    Ok(())
}
