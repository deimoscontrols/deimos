use std::{
    fs::{self, OpenOptions},
    path::{Path, PathBuf},
};

use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::{EnvFilter, fmt, layer::SubscriberExt, util::SubscriberInitExt};

/// Logger thread handles, which must be kept alive for as long as the logging targets will be used.
/// Flushed automatically when dropped.
pub(crate) struct LoggingGuards {
    _stdout: WorkerGuard,
    _file: WorkerGuard,
}

impl LoggingGuards {
    fn new(stdout: WorkerGuard, file: WorkerGuard) -> Self {
        Self {
            _stdout: stdout,
            _file: file,
        }
    }
}

/// Set up file and terminal logging.
pub(crate) fn init_logging(
    op_dir: &Path,
    op_name: &str,
) -> Result<(PathBuf, LoggingGuards), String> {
    let log_dir = op_dir.join("logs");
    fs::create_dir_all(&log_dir).map_err(|e| format!("Failed to create log directory: {e}"))?;
    let log_path = log_dir.join(format!("{op_name}.log"));
    let logfile = OpenOptions::new()
        .create(true)
        .truncate(false)
        .append(true)
        .write(true)
        .open(&log_path)
        .map_err(|e| format!("Failed to create log file: {e}"))?;

    let (stdout_writer, stdout_guard) = tracing_appender::non_blocking(std::io::stdout());
    let (file_writer, file_guard) = tracing_appender::non_blocking(logfile);

    let env_filter = EnvFilter::try_from_default_env()
        .or_else(|_| EnvFilter::try_new("info"))
        .map_err(|e| format!("Failed to set up logging env filter: {e}"))?;

    let stdout_layer = fmt::layer()
        .with_timer(fmt::time::ChronoUtc::rfc_3339())
        .with_writer(stdout_writer)
        .with_target(false);

    let file_layer = fmt::layer()
        .with_timer(fmt::time::ChronoUtc::rfc_3339())
        .with_writer(file_writer)
        .with_ansi(false);

    tracing_subscriber::registry()
        .with(env_filter)
        .with(stdout_layer)
        .with(file_layer)
        .try_init()
        .map_err(|e| format!("Failed to initialize logging: {e}"))?;

    Ok((log_path, LoggingGuards::new(stdout_guard, file_guard)))
}
