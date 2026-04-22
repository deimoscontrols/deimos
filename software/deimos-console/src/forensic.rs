//! Per-session forensic CSV log for the Deimos operator console.
//!
//! Each session produces one or more CSV files recording every received `Row` with
//! the viewer-side receive timestamp prepended. When a file grows past
//! [`MAX_FILE_BYTES`], the log rotates to a new shard before the next write.
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
}

impl ForensicLog {
    /// Open a new forensic log at `base_path`.
    ///
    /// The header is written immediately. If `base_path` already exists it is truncated.
    pub fn new(base_path: &Path, channel_names: &[String]) -> std::io::Result<Self> {
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
        if self.bytes_written >= MAX_FILE_BYTES {
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
        let _ = self.writer.flush();
    }
}

// ----- formatting helpers -----------------------------------------------------------------------

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

// ----- file helpers -----------------------------------------------------------------------------

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

// ---- tests -------------------------------------------------------------------------------------

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

        // ---- header assertions ----
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

        // ---- collect data rows ----
        let data_rows: Vec<&str> = lines.collect();
        assert_eq!(
            data_rows.len(),
            6,
            "expected 6 data rows (3 pre-stall + 3 post-stall), got {}",
            data_rows.len()
        );

        // ---- column count per row ----
        // Header has 4 metadata cols + N_CHANNELS channel cols.
        let expected_cols = 4 + N_CHANNELS;
        for row in &data_rows {
            let col_count = row.split(',').count();
            assert_eq!(
                col_count, expected_cols,
                "row has wrong column count ({col_count} vs {expected_cols}): {row}"
            );
        }

        // ---- seq values: pre-stall then post-stall, gap in between ----
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
}
