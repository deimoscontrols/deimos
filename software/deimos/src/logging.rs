use std::{
    fs::{self, File, OpenOptions},
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
};

use tracing_appender::non_blocking::{NonBlocking, WorkerGuard};
use tracing_subscriber::{
    EnvFilter, Registry, fmt, layer::SubscriberExt, reload, util::SubscriberInitExt,
};

type FileLayer = fmt::Layer<
    Registry,
    fmt::format::DefaultFields,
    fmt::format::Format<fmt::format::Full, fmt::time::ChronoUtc>,
    NonBlocking,
>;

/// Global root logger.
static LOGGING_GUARDS: OnceLock<LoggingGuards> = OnceLock::new();

/// Logger thread handles, which must be kept alive for as long as the logging targets will be used.
/// Flushed automatically when dropped.
pub(crate) struct LoggingGuards {
    _stdout: Mutex<WorkerGuard>,
    file: Mutex<WorkerGuard>,

    /// A handle to modify the file logger to point to a different file.
    file_reload: reload::Handle<FileLayer, Registry>,
}

impl LoggingGuards {
    fn new(
        stdout: WorkerGuard,
        file: WorkerGuard,
        file_reload: reload::Handle<FileLayer, Registry>,
    ) -> Self {
        Self {
            _stdout: Mutex::new(stdout),
            file: Mutex::new(file),
            file_reload,
        }
    }

    /// Point the file logger at a different file.
    fn update_file_layer(&self, logfile: File) -> Result<(), String> {
        let (file_writer, file_guard) = tracing_appender::non_blocking(logfile);
        let file_layer = build_file_layer(file_writer);

        let mut guard = self
            .file
            .lock()
            .map_err(|_| "Logging file guard lock poisoned".to_string())?;

        self.file_reload
            .modify(|layer| {
                *layer = file_layer;
            })
            .map_err(|e| format!("Failed to reload logging file layer: {e}"))?;

        *guard = file_guard;
        Ok(())
    }
}

/// Build a formatted file logger on top of a thread-safe nonblocking writer.
fn build_file_layer(file_writer: NonBlocking) -> FileLayer {
    fmt::layer::<Registry>()
        .with_timer(fmt::time::ChronoUtc::rfc_3339())
        .with_writer(file_writer)
        .with_ansi(false)
}

/// Set up file and terminal logging.
pub(crate) fn init_logging(
    op_dir: &Path,
    op_name: &str,
) -> Result<(PathBuf, &'static LoggingGuards), String> {
    // Build file writer
    let log_dir = op_dir.join("logs");
    fs::create_dir_all(&log_dir).map_err(|e| format!("Failed to create log directory: {e}"))?;
    let log_path = log_dir.join(format!("{op_name}.log"));
    let logfile = OpenOptions::new()
        .create(true)
        .truncate(false)
        .append(true)
        .open(&log_path)
        .map_err(|e| format!("Failed to create log file: {e}"))?;

    // If we're already initialized, just update the file logger to point at the new file
    if let Some(guards) = LOGGING_GUARDS.get() {
        guards.update_file_layer(logfile)?;
        return Ok((log_path, guards));
    }

    // Build new terminal and file writers
    let (stdout_writer, stdout_guard) = tracing_appender::non_blocking(std::io::stdout());
    let (file_writer, file_guard) = tracing_appender::non_blocking(logfile);

    // Filter for log level
    let env_filter = EnvFilter::try_from_default_env()
        .or_else(|_| EnvFilter::try_new("info"))
        .map_err(|e| format!("Failed to set up logging env filter: {e}"))?;

    // Formatting for terminal logger
    let stdout_layer = fmt::layer()
        .with_timer(fmt::time::ChronoUtc::rfc_3339())
        .with_writer(stdout_writer)
        .with_target(false);

    // Build file logger (with formatting) from writer
    let file_layer = build_file_layer(file_writer);
    let (file_layer, file_reload) = reload::Layer::<FileLayer, Registry>::new(file_layer);

    // Set up global root logger
    tracing_subscriber::registry()
        .with(file_layer)
        .with(env_filter)
        .with(stdout_layer)
        .try_init()
        .map_err(|e| format!("Failed to initialize logging: {e}"))?;

    let guards =
        LOGGING_GUARDS.get_or_init(|| LoggingGuards::new(stdout_guard, file_guard, file_reload));

    Ok((log_path, guards))
}
