use std::net::Ipv4Addr;
use std::path::PathBuf;

use serde::Deserialize;

fn default_window_seconds() -> f64 {
    30.0
}

fn default_staleness_threshold_secs() -> f64 {
    2.0
}

/// Top-level configuration for the Deimos operator console.
#[derive(Debug, Deserialize)]
pub struct DeimosConsoleConfig {
    /// Multicast group address to subscribe to (e.g. 239.255.0.1).
    pub multicast_group: Ipv4Addr,
    /// UDP port to listen on.
    pub port: u16,
    /// Local network interface address to bind for multicast reception.
    /// If `None`, the OS chooses the default interface.
    pub interface: Option<Ipv4Addr>,
    /// How many seconds of data to keep visible in each scrolling trace.
    /// Samples older than this are evicted from the ring buffer.
    /// Defaults to 30 seconds.
    #[serde(default = "default_window_seconds")]
    pub window_seconds: f64,
    /// How many seconds without a received `Row` before the connection is considered stale.
    /// Defaults to 2 seconds.
    #[serde(default = "default_staleness_threshold_secs")]
    pub staleness_threshold_secs: f64,
    /// Panel definitions — each panel renders a set of channels.
    #[serde(default)]
    pub panels: Vec<PanelConfig>,

    /// If set, a forensic CSV log is written to this path for each session.
    ///
    /// Each session opens (or re-opens) this file, writing a header row followed by one data
    /// row per received `Row` message. When the file exceeds 64 MiB, the log rotates to a new
    /// shard (e.g. `forensic_1.csv`, `forensic_2.csv`, …). When `None`, no log is written.
    #[serde(default)]
    pub forensic_log_path: Option<PathBuf>,
}

/// Configuration for a single display panel.
#[derive(Debug, Deserialize)]
pub struct PanelConfig {
    /// Human-readable title shown in the panel header.
    pub title: String,
    /// Channel names to display in this panel. Must match the names emitted
    /// in the controller's Schema packet.
    pub channels: Vec<String>,
}
