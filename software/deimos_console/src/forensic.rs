//! Per-session forensic CSV log for the Deimos operator console.
//!
//! Each session produces one or more CSV files recording every received `Row` with
//! the viewer-side receive timestamp prepended. When a file grows past
//! [`MAX_FILE_BYTES`], the log rotates to a new shard before the next write.
//!
//! # Receipt, not display
//!
//! The forensic log captures **what the viewer received**, not what was rendered live on the
//! plot. Under a UI stall the live window may elide the pre-stall block so the operator's eye
//! lands on the freshest data (see `app.rs::apply_stall`), but every one of those elided rows
//! is still written here with its original sequence number, controller timestamp, and the
//! viewer-side receipt timestamp captured at `process_row` time. Post-hoc rendering of this
//! CSV is the source of truth for "what data crossed the wire"; the live plot is the source
//! of truth for "what the operator saw."
//!
//! Coverage starts at the first `Schema`: the log file is opened when the schema arrives, and
//! `Row`s buffered before that are flushed in order at open time. A row that arrives before
//! any `Schema` and gets evicted from the bounded pre-schema queue (see `app.rs::pending_rows`
//! and `pending_overflow_drops`) is therefore not in this CSV. The canonical capture path for
//! a run is the controller-side `CsvDispatcher`, not this viewer-side log.
//!
//! Column layout:
//! ```text
//! viewer_received_at,seq,controller_timestamp,controller_system_time,<channel…>
//! ```
//!
//! Float values are formatted with Deimos's fixed-width formatter (`fmt_f64`) for
//! bit-exact consistency with the controller's own CSV files.

use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use deimos::dispatcher::{fmt_f64, fmt_time};

/// Rotate to a new shard once the current file exceeds this size in bytes.
const MAX_FILE_BYTES: u64 = 64 * 1024 * 1024; // 64 MiB

/// Per-session forensic log writer.
///
/// Constructed with [`ForensicLog::new`] at the start of each session. `write_row` appends one
/// CSV line per received `Row`, rotating to a new file when the current shard exceeds
/// [`MAX_FILE_BYTES`].
pub struct ForensicLog {
    /// Base path supplied by the user (e.g. `/tmp/deimos-forensic.csv`).
    base_path: PathBuf,
    /// Current buffered writer.
    writer: BufWriter<File>,
    /// Current shard index (0 for the first file, 1 for the second, …).
    shard: u32,
    /// Pre-formatted CSV header row, reused on rotation.
    header: String,
    /// Reusable row buffer to reduce allocations.
    row_buf: String,
    /// Bytes written to the current shard (including header). Tracked in memory to avoid
    /// the stat(2) lag of `metadata().len()` which would lag by up to one BufWriter buffer.
    bytes_written: u64,
    /// Threshold above which `write_row` rotates to a new shard. Set to [`MAX_FILE_BYTES`] in
    /// production; an internal test constructor lowers it so rotation can be exercised in
    /// unit tests without writing 64 MiB.
    max_bytes: u64,
}

impl ForensicLog {
    /// Open a new forensic log at `base_path`.
    ///
    /// The header is written immediately. If `base_path` already exists it is truncated.
    pub fn new(base_path: &Path, channel_names: &[String]) -> std::io::Result<Self> {
        Self::with_max_bytes(base_path, channel_names, MAX_FILE_BYTES)
    }

    /// Open a forensic log with a custom rotation threshold.
    ///
    /// Used by [`ForensicLog::new`] (with [`MAX_FILE_BYTES`]) and by unit tests to exercise
    /// the rotation path with small inputs.
    fn with_max_bytes(
        base_path: &Path,
        channel_names: &[String],
        max_bytes: u64,
    ) -> std::io::Result<Self> {
        let header = build_header(channel_names);
        let bytes_written = header.len() as u64;
        let writer = open_file(base_path, &header)?;
        Ok(Self {
            base_path: base_path.to_owned(),
            writer,
            shard: 0,
            header,
            row_buf: String::new(),
            bytes_written,
            max_bytes,
        })
    }

    /// Append a single row to the log, rotating to a new file if necessary.
    pub fn write_row(
        &mut self,
        viewer_received_at: SystemTime,
        seq: u64,
        controller_timestamp: f64,
        controller_system_time: &str,
        values: &[f64],
    ) -> std::io::Result<()> {
        // Check file size and rotate before writing if needed.
        // Use the in-memory counter rather than metadata().len() to avoid BufWriter lag.
        if self.bytes_written >= self.max_bytes {
            self.rotate()?;
        }

        format_row(
            &mut self.row_buf,
            viewer_received_at,
            seq,
            controller_timestamp,
            controller_system_time,
            values,
        );
        self.writer.write_all(self.row_buf.as_bytes())?;
        self.bytes_written += self.row_buf.len() as u64;
        Ok(())
    }

    /// Open the next shard, incrementing the shard counter and writing a fresh header.
    fn rotate(&mut self) -> std::io::Result<()> {
        self.writer.flush()?;
        self.shard += 1;
        let path = shard_path(&self.base_path, self.shard);
        self.writer = open_file(&path, &self.header)?;
        self.bytes_written = self.header.len() as u64;
        Ok(())
    }
}

impl Drop for ForensicLog {
    fn drop(&mut self) {
        if let Err(e) = self.writer.flush() {
            let path = shard_path(&self.base_path, self.shard);
            eprintln!(
                "deimos-console: forensic log final flush failed for {}: {e} \
                 (final rows may be truncated)",
                path.display()
            );
        }
    }
}

/// Build the CSV header row.
fn build_header(channel_names: &[String]) -> String {
    let mut h = String::from("viewer_received_at,seq,controller_timestamp,controller_system_time");
    for name in channel_names {
        h.push(',');
        h.push_str(name);
    }
    h.push('\n');
    h
}

/// Format a single CSV data row into `buf`, reusing its allocation.
fn format_row(
    buf: &mut String,
    viewer_received_at: SystemTime,
    seq: u64,
    controller_timestamp: f64,
    controller_system_time: &str,
    values: &[f64],
) {
    buf.clear();
    buf.push_str(&fmt_time(viewer_received_at));
    buf.push(',');
    buf.push_str(&seq.to_string());
    buf.push(',');
    buf.push_str(&fmt_f64(controller_timestamp));
    buf.push(',');
    buf.push_str(controller_system_time);
    for v in values {
        buf.push(',');
        buf.push_str(&fmt_f64(*v));
    }
    buf.push('\n');
}

/// Derive the path for shard `n`.
///
/// - Shard 0 → `base_path` unchanged.
/// - Shard 1 → `<stem>_1.<ext>` (or `<stem>_1` if no extension).
fn shard_path(base: &Path, n: u32) -> PathBuf {
    if n == 0 {
        return base.to_owned();
    }
    let stem = base
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let ext = base
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy()))
        .unwrap_or_default();
    let filename = format!("{stem}_{n}{ext}");
    base.with_file_name(filename)
}

/// Create (or truncate) the file at `path`, write `header`, and return a `BufWriter`.
///
/// Creates any missing parent directories before opening the file. A missing intermediate
/// directory should not silently discard forensic data; `create_dir_all` ensures the path
/// exists so `open` can succeed.
fn open_file(path: &Path, header: &str) -> std::io::Result<BufWriter<File>> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(path)?;
    let mut writer = BufWriter::new(file);
    writer.write_all(header.as_bytes())?;
    Ok(writer)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// Number of channel columns in test rows (temperature + pressure).
    const N_CHANNELS: usize = 2;
    const CHANNEL_NAMES: &[&str] = &["temperature_K", "pressure_Pa"];

    fn channel_names() -> Vec<String> {
        CHANNEL_NAMES.iter().map(|s| s.to_string()).collect()
    }

    /// Write a single row with the given sequence number.
    fn write_seq(log: &mut ForensicLog, seq: u64) -> std::io::Result<()> {
        log.write_row(
            SystemTime::UNIX_EPOCH,
            seq,
            seq as f64 * 0.05, // fake controller timestamp
            "1970-01-01T00:00:00Z",
            &[300.0 + seq as f64, 101325.0 + seq as f64],
        )
    }

    #[test]
    fn forensic_log_contains_pre_and_post_stall_rows() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("forensic.csv");

        let names = channel_names();
        let mut log = ForensicLog::new(&path, &names).expect("ForensicLog::new");

        // Pre-stall rows: seqs 1, 2, 3.
        for seq in 1u64..=3 {
            write_seq(&mut log, seq).expect("write pre-stall");
        }

        // Simulated stall: skip seqs 4-9. Post-stall rows: seqs 10, 11, 12.
        for seq in 10u64..=12 {
            write_seq(&mut log, seq).expect("write post-stall");
        }

        // Drop flushes the BufWriter.
        drop(log);

        let contents = fs::read_to_string(&path).expect("read csv");
        let mut lines = contents.lines();

        let header = lines.next().expect("header line present");
        assert!(
            header
                .starts_with("viewer_received_at,seq,controller_timestamp,controller_system_time"),
            "header prefix mismatch: {header}"
        );
        for col in CHANNEL_NAMES {
            assert!(
                header.contains(col),
                "header missing column {col}: {header}"
            );
        }

        let data_rows: Vec<&str> = lines.collect();
        assert_eq!(
            data_rows.len(),
            6,
            "expected 6 data rows (3 pre-stall + 3 post-stall), got {}",
            data_rows.len()
        );

        // Header has 4 metadata cols + N_CHANNELS channel cols.
        let expected_cols = 4 + N_CHANNELS;
        for row in &data_rows {
            let col_count = row.split(',').count();
            assert_eq!(
                col_count, expected_cols,
                "row has wrong column count ({col_count} vs {expected_cols}): {row}"
            );
        }

        let seqs: Vec<u64> = data_rows
            .iter()
            .map(|row| {
                let seq_str = row.split(',').nth(1).expect("seq column");
                seq_str.parse::<u64>().expect("seq parse")
            })
            .collect();

        let pre_stall: Vec<u64> = seqs.iter().copied().filter(|&s| s <= 3).collect();
        let post_stall: Vec<u64> = seqs.iter().copied().filter(|&s| s >= 10).collect();
        let gap: Vec<u64> = seqs
            .iter()
            .copied()
            .filter(|&s| (4..=9).contains(&s))
            .collect();

        assert_eq!(pre_stall, [1, 2, 3], "pre-stall seqs mismatch");
        assert_eq!(post_stall, [10, 11, 12], "post-stall seqs mismatch");
        assert!(gap.is_empty(), "gap seqs should be absent: {gap:?}");
    }

    #[test]
    fn forensic_log_rotates_when_shard_exceeds_max_bytes() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("forensic.csv");

        let names = channel_names();

        // Pick max_bytes small enough that 20 rows force at least one rotation, but large
        // enough that the test does not depend on the exact bytes-per-row count.
        let mut log =
            ForensicLog::with_max_bytes(&path, &names, 512).expect("ForensicLog::with_max_bytes");

        for seq in 1u64..=20 {
            write_seq(&mut log, seq).expect("write_row");
        }
        drop(log); // flush the BufWriter for shard 0 plus any rotated shards

        // Collect every CSV file the log produced under the tempdir. The base path is shard 0;
        // additional shards are `forensic_1.csv`, `forensic_2.csv`, etc. Sort by filename so
        // assertions are stable across filesystem orderings.
        let mut shard_paths: Vec<PathBuf> = fs::read_dir(dir.path())
            .expect("read_dir tempdir")
            .filter_map(|entry| entry.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|e| e == "csv"))
            .collect();
        shard_paths.sort();

        assert!(
            shard_paths.len() >= 2,
            "rotation should produce at least 2 shards under a 512-byte cap; got {} ({:?})",
            shard_paths.len(),
            shard_paths
        );

        // Shard 0 must exist at the user-supplied base path so existing tooling (the operator
        // pointing the log path at a known file) keeps working across rotation.
        assert!(
            path.exists(),
            "shard 0 must exist at the user-supplied base path"
        );

        // Every shard begins with an identical header so each file is independently parseable
        // by external tooling (Excel, pandas, etc.) without context from the others.
        let header_prefix = "viewer_received_at,seq,controller_timestamp,controller_system_time,temperature_K,pressure_Pa\n";

        let mut total_rows = 0usize;
        for shard in &shard_paths {
            let body = fs::read_to_string(shard)
                .unwrap_or_else(|e| panic!("read {}: {e}", shard.display()));
            assert!(
                body.starts_with(header_prefix),
                "shard {} missing or malformed header",
                shard.display()
            );
            // Subtract one line for the header.
            total_rows += body.lines().count().saturating_sub(1);
        }

        assert_eq!(
            total_rows, 20,
            "all 20 data rows must be preserved across all rotated shards"
        );
    }
}
