//! Operator console viewer for Deimos realtime reporting.
//!
//! Subscribes to the multicast stream published by a `ReportingDispatcher`, renders scrolling
//! traces for configured channel groups, and writes a per-session forensic CSV. Connection health
//! is shown in the status bar; use Freeze to inspect a moment without interrupting data
//! collection. See the crate README for the config-file format.

mod app;
mod config;
mod forensic;
mod receiver;

use std::error::Error;
use std::path::PathBuf;

use clap::Parser;

use app::DeimosConsoleApp;
use config::DeimosConsoleConfig;

#[derive(Parser)]
#[command(
    name = "deimos-console",
    about = "Operator console viewer for Deimos realtime reporting"
)]
struct Cli {
    /// Path to the TOML configuration file (see crate README for fields).
    #[arg(long, short, value_name = "FILE")]
    config: PathBuf,
}

fn main() -> Result<(), Box<dyn Error>> {
    let cli = Cli::parse();

    let config_bytes = std::fs::read_to_string(&cli.config)
        .map_err(|e| format!("failed to read config file {}: {e}", cli.config.display()))?;
    let config: DeimosConsoleConfig = toml::from_str(&config_bytes)
        .map_err(|e| format!("failed to parse config file {}: {e}", cli.config.display()))?;

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

    let (rx, _recv_handle, counters) = receiver::spawn(&config)
        .map_err(|e| format!("failed to start multicast receiver thread: {e}"))?;

    eframe::run_native(
        "Deimos Console",
        eframe::NativeOptions::default(),
        Box::new(|_cc| Ok(Box::new(DeimosConsoleApp::new(config, rx, counters)))),
    )
    .map_err(|e| format!("eframe error: {e}"))?;

    Ok(())
}
