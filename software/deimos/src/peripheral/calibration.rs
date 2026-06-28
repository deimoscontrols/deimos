//! Storage and retrieval of calibration records.

use crate::fmt_time;
use serde::{Deserialize, Serialize};
use std::{
    env, fs,
    io::Read,
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};
use tracing::{debug, info, warn};

/// Current schema version for the shared, top-level calibration record fields.
///
/// Peripheral-specific payloads may have their own independent schema versions.
pub const CURRENT_CAL_SCHEMA_VERSION: u16 = 1;
const CAL_FILE_NAME: &str = "cal.json";
const DEFAULT_RECORD_STORE_RELATIVE_PATH: &str = ".deimos/records";
const DEIMOS_CONTROLS_RECORD_BASE_URL: &str = "https://deimoscontrols.com/records";
const MAX_CAL_JSON_BYTES: u64 = 5 * 1024 * 1024;
const CAL_QUERY_TIMEOUT: Duration = Duration::from_secs(5);

/// Basic information to be included in all top-level calibration records.
///
/// This core record is intended to be parseable for every calibration artifact,
/// even when the peripheral-specific payload is unknown to the current software.
#[derive(Serialize, Deserialize, Debug, Default)]
pub struct CalRecordCore {
    /// Schema version for these shared top-level fields.
    pub schema_version: u16,
    /// Peripheral implementation kind, usually from [`crate::peripheral::Peripheral::kind`].
    pub peripheral_kind: String,
    /// Numeric peripheral model identifier.
    pub model_number: u64,
    /// Unit serial number.
    pub serial_number: u64,
    /// Calibration procedure identifier.
    pub procedure: String,
    /// Version of the calibration procedure that produced this record.
    pub procedure_version: u16,
    /// UTC timestamp when this record was generated.
    pub generated_at_utc: String,
    /// Records-folder references for calibrators used by the procedure.
    pub calibrators: Vec<String>,
}

impl CalRecordCore {
    /// Construct a new shared calibration record core using the current schema version.
    ///
    /// The generation timestamp is filled from the current system time using the
    /// crate-wide fixed-width UTC timestamp formatter.
    pub fn new(
        peripheral_kind: impl Into<String>,
        model_number: u64,
        serial_number: u64,
        procedure: impl Into<String>,
        procedure_version: u16,
        calibrators: Vec<String>,
    ) -> Self {
        Self {
            schema_version: CURRENT_CAL_SCHEMA_VERSION,
            peripheral_kind: peripheral_kind.into(),
            model_number,
            serial_number,
            procedure: procedure.into(),
            procedure_version,
            generated_at_utc: fmt_time(SystemTime::now()),
            calibrators,
        }
    }
}

/// Locate and return a JSON calibration record for `slug`.
///
/// Lookup order is:
///
/// 1. The default local cache at `~/.deimos/records/<slug>/cal.json`.
/// 2. Each caller-provided local source at `<source>/<slug>/cal.json`.
/// 3. `https://deimoscontrols.com/records/<slug>/cal.json`, unless `offline_only`
///    is set.
///
/// A calibration fetched from `deimoscontrols.com` is written back into the
/// default local cache before it is returned.
///
/// The slug is validated as relative path segments containing only ASCII
/// alphanumeric characters, `_`, and `-`. This prevents path traversal for local
/// stores and keeps the remote URL fixed to the public Deimos calibration host.
///
/// # Errors
///
/// Returns an error if the slug is invalid, if a local file exists but cannot be
/// read, if the remote query receives an unexpected response, or if a fetched
/// calibration cannot be cached locally. Remote transport failures like DNS
/// errors or timeouts are treated as misses so callers that allow missing
/// calibrations can fall back to defaults while offline.
pub fn query_cals<P: AsRef<Path>>(
    slug: &str,
    local_sources: &[P],
    offline_only: bool,
) -> Result<Option<String>, String> {
    let slug_segments = validate_cal_slug(slug)?;
    debug!(slug, "Querying calibration record.");

    // The built-in cache is always checked first so repeated calls work offline
    // after one successful remote lookup.
    if let Some(default_store) = default_cal_store() {
        if let Some(cals) = try_read_local_cal(&default_store, &slug_segments)? {
            info!(
                slug,
                source = %default_store.display(),
                "Discovered calibration record in default local store."
            );
            return Ok(Some(cals));
        }
    } else {
        debug!(
            slug,
            "No home directory available for default calibration store."
        );
    }

    // Caller-provided stores are checked in order. This lets applications layer
    // project-local or deployment-local calibration bundles after the user cache.
    for local_source in local_sources {
        if let Some(cals) = try_read_local_cal(local_source.as_ref(), &slug_segments)? {
            info!(
                slug,
                source = %local_source.as_ref().display(),
                "Discovered calibration record in local store."
            );
            return Ok(Some(cals));
        }
    }

    if offline_only {
        info!(
            slug,
            "Calibration record was not found in local stores; offline_only is set."
        );
        return Ok(None);
    }

    // Remote lookup is deliberately fixed to deimoscontrols.com for now. That
    // keeps this helper from becoming a general-purpose URL fetcher.
    let remote_result = fetch_deimos_controls_cal(&slug_segments);
    let cals = match remote_result {
        Ok(Some(cals)) => cals,
        Ok(None) => {
            info!(
                slug,
                "Calibration record was not found at deimoscontrols.com."
            );
            return Ok(None);
        }
        Err(err) if err.is_transport_failure => {
            warn!(
                slug,
                error = %err.message,
                "Calibration record could not be queried from deimoscontrols.com; treating remote lookup as missing."
            );
            return Ok(None);
        }
        Err(err) => return Err(err.message),
    };
    info!(slug, "Fetched calibration record from deimoscontrols.com.");

    // Remote records are cached locally so a later controller run can resolve
    // the same calibration without requiring network access.
    let default_store = default_cal_store()
        .ok_or_else(|| "Unable to determine home directory for calibration cache".to_owned())?;
    write_cached_cal(&default_store, &slug_segments, &cals)?;
    info!(
        slug,
        cache = %default_store.display(),
        "Cached calibration record in default local store."
    );

    Ok(Some(cals))
}

/// Validate and split a calibration slug into safe relative path segments.
///
/// The same segments are used for local filesystem paths and for the fixed
/// deimoscontrols.com URL path, so this function rejects anything that could
/// become path traversal, a URL escape hatch, or a platform-specific path
/// separator.
fn validate_cal_slug(slug: &str) -> Result<Vec<&str>, String> {
    let segments = slug.split('/').collect::<Vec<_>>();
    if segments.is_empty() {
        return Err("Calibration slug must not be empty".to_owned());
    }

    for segment in &segments {
        // Empty, current-directory, and parent-directory segments would make
        // the slug ambiguous or allow traversal outside the intended store.
        if segment.is_empty() || *segment == "." || *segment == ".." {
            return Err(format!(
                "Calibration slug contains invalid segment `{segment}`"
            ));
        }
        // Keep the accepted character set intentionally narrow. If route slugs
        // need richer names later, add explicit escaping instead of relaxing
        // this into arbitrary URL/path text.
        if !segment
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        {
            return Err(format!(
                "Calibration slug segment `{segment}` contains unsupported characters"
            ));
        }
    }

    Ok(segments)
}

/// Return the default user-local unit record cache root.
///
/// The path is `~/.deimos/records` when a home directory can be inferred from
/// `HOME`. If `HOME` is unavailable, callers can still use explicit local
/// sources or remote lookup.
fn default_cal_store() -> Option<PathBuf> {
    env::var_os("HOME")
        .map(PathBuf::from)
        .map(|home| home.join(DEFAULT_RECORD_STORE_RELATIVE_PATH))
}

/// Construct `<root>/<slug>/cal.json` from a validated slug.
///
/// This assumes `slug_segments` came from [`validate_cal_slug`]. Keeping path
/// assembly centralized makes it easier to preserve the same layout for cache,
/// caller-provided local stores, and generated website artifacts.
fn cal_path(root: &Path, slug_segments: &[&str]) -> PathBuf {
    let mut path = root.to_path_buf();
    for segment in slug_segments {
        path.push(segment);
    }
    path.push(CAL_FILE_NAME);
    path
}

/// Try to read one local calibration record from a store root.
///
/// Missing files are not errors because lookup should continue through later
/// local stores and then, optionally, the remote source. Other I/O failures are
/// returned because they usually indicate a permissions or filesystem problem
/// that should not be silently skipped.
fn try_read_local_cal(root: &Path, slug_segments: &[&str]) -> Result<Option<String>, String> {
    let path = cal_path(root, slug_segments);
    match fs::read_to_string(&path) {
        Ok(cals) => Ok(Some(cals)),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            debug!(path = %path.display(), "Calibration record not found in local store.");
            Ok(None)
        }
        Err(err) => Err(format!("Failed to read {}: {err}", path.display())),
    }
}

/// Fetch a calibration record from the fixed Deimos Controls public endpoint.
///
/// This function intentionally accepts validated slug segments, not a caller
/// supplied URL. Redirects are disabled and the body size is bounded so the
/// calibration lookup remains a narrow, deterministic HTTP GET for
/// `https://deimoscontrols.com/records/<slug>/cal.json`.
fn fetch_deimos_controls_cal(slug_segments: &[&str]) -> Result<Option<String>, RemoteCalError> {
    let url = format!(
        "{}/{}/{}",
        DEIMOS_CONTROLS_RECORD_BASE_URL,
        slug_segments.join("/"),
        CAL_FILE_NAME
    );
    // Use a short timeout because calibration lookup happens during setup and
    // should fail quickly when the public endpoint is unavailable.
    let client = reqwest::blocking::Client::builder()
        .timeout(CAL_QUERY_TIMEOUT)
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|err| {
            RemoteCalError::unexpected(format!("Failed to build calibration HTTP client: {err}"))
        })?;
    let response = client.get(&url).send().map_err(|err| {
        RemoteCalError::transport(format!(
            "Failed to fetch calibration record from {url}: {err}"
        ))
    })?;
    debug!(url, status = %response.status(), "Received calibration query response.");

    // A missing remote cal is a normal miss, not a hard failure. Other
    // unsuccessful statuses are treated as errors because they may indicate
    // service or publishing problems.
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    if !response.status().is_success() {
        return Err(RemoteCalError::unexpected(format!(
            "Failed to fetch calibration record from {url}: HTTP {}",
            response.status()
        )));
    }
    // Honor Content-Length when available so obviously oversized responses are
    // rejected before allocating/reading the body.
    if let Some(content_length) = response.content_length()
        && content_length > MAX_CAL_JSON_BYTES
    {
        return Err(RemoteCalError::unexpected(format!(
            "Calibration record from {url} is too large: {content_length} bytes"
        )));
    }

    // Also enforce the size limit while reading in case the server did not
    // provide a Content-Length header.
    let mut body = Vec::new();
    let mut reader = response.take(MAX_CAL_JSON_BYTES + 1);
    reader.read_to_end(&mut body).map_err(|err| {
        RemoteCalError::transport(format!(
            "Failed to read calibration record from {url}: {err}"
        ))
    })?;
    if body.len() as u64 > MAX_CAL_JSON_BYTES {
        return Err(RemoteCalError::unexpected(format!(
            "Calibration record from {url} exceeds {} bytes",
            MAX_CAL_JSON_BYTES
        )));
    }

    // Calibration records are JSON text. Returning UTF-8 text instead of raw
    // bytes keeps parsing responsibility with the caller while still rejecting
    // invalid text at the query boundary.
    String::from_utf8(body).map(Some).map_err(|err| {
        RemoteCalError::unexpected(format!(
            "Calibration record from {url} was not valid UTF-8: {err}"
        ))
    })
}

/// Error returned by the fixed public calibration endpoint lookup.
///
/// Network transport failures are separated from semantic response errors so
/// offline controller startup can fall back to default calibrations without also
/// hiding malformed or unexpectedly large remote records.
struct RemoteCalError {
    message: String,
    is_transport_failure: bool,
}

impl RemoteCalError {
    /// Build an error for DNS, timeout, connection, or response-body I/O failures.
    fn transport(message: String) -> Self {
        Self {
            message,
            is_transport_failure: true,
        }
    }

    /// Build an error for a remote response that was reached but unusable.
    fn unexpected(message: String) -> Self {
        Self {
            message,
            is_transport_failure: false,
        }
    }
}

/// Write a fetched calibration record into a local cache root.
///
/// The cache path uses the same `<root>/<slug>/cal.json` layout as local
/// lookup. Write failures are logged and returned so callers know that the
/// current lookup succeeded but future offline lookup may not.
fn write_cached_cal(root: &Path, slug_segments: &[&str], cals: &str) -> Result<(), String> {
    let path = cal_path(root, slug_segments);
    let parent = path.parent().ok_or_else(|| {
        format!(
            "Unable to determine parent directory for {}",
            path.display()
        )
    })?;
    // Create the slug directory hierarchy lazily only after a remote record has
    // actually been fetched.
    fs::create_dir_all(parent).map_err(|err| {
        warn!(
            path = %parent.display(),
            "Failed to create calibration cache directory: {err}"
        );
        format!("Failed to create {}: {err}", parent.display())
    })?;
    // Overwrite the cached copy atomically enough for this use case. A future
    // cache with concurrent writers could switch this to write-then-rename.
    fs::write(&path, cals).map_err(|err| {
        warn!(
            path = %path.display(),
            "Failed to write calibration cache file: {err}"
        );
        format!("Failed to write {}: {err}", path.display())
    })
}
