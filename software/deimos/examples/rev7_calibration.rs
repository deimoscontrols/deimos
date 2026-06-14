//! Rev7 DAQ calibration scaffold.
//!
//! This example starts with the rev7 4-20 mA inputs. For each channel, connect a
//! Fluke 707 configured to auto-step through 0, 5, 10, 15, and 20 mA, then press
//! Enter. The example records 90 seconds, writes one calibration CSV per channel,
//! then processes calibration CSVs by loading them back from disk.

use std::{
    env,
    fs::{self, File, create_dir_all},
    io::{BufWriter, Write, stdin},
    net::Ipv4Addr,
    path::{Path, PathBuf},
    sync::atomic::Ordering,
    time::{Duration, SystemTime},
};

use chrono::{DateTime, SecondsFormat, Utc};
use deimos::{
    Controller, ControllerCtx, DataFrameDispatcher, LoopMethod, Overflow, Termination,
    dispatcher::{DataFrameHandle, ReportingDispatcher},
    math::{polyfit, polyval},
    peripheral::DeimosDaqRev7,
};
use serde::{Deserialize, Serialize};

const MODEL_NAME: &str = "deimos_daq_rev7";
const PERIPHERAL_NAME: &str = "p1";
const SERIAL_NUMBER: u64 = 2;
const RATE_HZ: f64 = 10.0;
const CAPTURE_SECONDS: u64 = 90;
const DATAFRAME_MB: usize = 16;
const REPORTING_MULTICAST_GROUP: Ipv4Addr = Ipv4Addr::new(239, 255, 0, 1);
const REPORTING_MULTICAST_PORT: u16 = 29573;
const REPORTING_SCHEMA_PERIOD: Duration = Duration::from_secs(2);

const REFERENCE_MAX_A: f64 = 0.020;
const STEP_REFERENCE_VALUES_A: [f64; 5] = [0.0, 0.005, 0.010, 0.015, 0.020];
const STEP_DETECTION_TOLERANCE_A: f64 = 0.0015;
const MIN_STEP_DURATION_S: f64 = 2.5;
const REFERENCE_RESISTOR_OHM: f64 = 75.0;
const VOLTAGE_FIT_ORDER: usize = 1;
const ZERO_C_K: f64 = 273.15;
const RTD_MIN_REFERENCE_K: f64 = ZERO_C_K - 200.0;
const RTD_MAX_REFERENCE_K: f64 = ZERO_C_K + 800.0;
const RTD_STEP_BELOW_ZERO_K: f64 = 10.0;
const RTD_STEP_ABOVE_ZERO_K: f64 = 50.0;
const RTD_STEP_DETECTION_TOLERANCE_K: f64 = 5.0;
const MIN_RTD_STEP_DURATION_S: f64 = 1.0;
const RTD_CAPTURE_SECONDS: u64 = 240;

const OUTPUT_ROOT: &str = "./software/deimos/examples/rev7_calibration";
const CONSOLE_CONFIG_PATH: &str = "software/deimos/examples/rev7_calibration_console.toml";
const CALIBRATION_DATA_SUFFIX: &str = "_calibration_data.csv";

#[derive(Clone, Copy, Debug)]
struct CalibrationChannel {
    label: &'static str,
    analog_input_name: &'static str,
    signal_name: &'static str,
    slug: &'static str,
    kind: CalibrationKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CalibrationKind {
    Current4To20,
    Rtd,
}

const CURRENT_CHANNELS: [CalibrationChannel; 4] = [
    CalibrationChannel {
        label: "4-20 mA channel 0 (ain3)",
        analog_input_name: "ain3",
        signal_name: "p1_4_20_mA_0_A.y",
        slug: "4_20_mA_0",
        kind: CalibrationKind::Current4To20,
    },
    CalibrationChannel {
        label: "4-20 mA channel 1 (ain4)",
        analog_input_name: "ain4",
        signal_name: "p1_4_20_mA_1_A.y",
        slug: "4_20_mA_1",
        kind: CalibrationKind::Current4To20,
    },
    CalibrationChannel {
        label: "4-20 mA channel 2 (ain5)",
        analog_input_name: "ain5",
        signal_name: "p1_4_20_mA_2_A.y",
        slug: "4_20_mA_2",
        kind: CalibrationKind::Current4To20,
    },
    CalibrationChannel {
        label: "4-20 mA channel 3 (ain6)",
        analog_input_name: "ain6",
        signal_name: "p1_4_20_mA_3_A.y",
        slug: "4_20_mA_3",
        kind: CalibrationKind::Current4To20,
    },
];

const RTD_CHANNELS: [CalibrationChannel; 3] = [
    CalibrationChannel {
        label: "RTD channel 0 (ain7)",
        analog_input_name: "ain7",
        signal_name: "p1_rtd_0.temperature_K",
        slug: "rtd_0",
        kind: CalibrationKind::Rtd,
    },
    CalibrationChannel {
        label: "RTD channel 1 (ain8)",
        analog_input_name: "ain8",
        signal_name: "p1_rtd_1.temperature_K",
        slug: "rtd_1",
        kind: CalibrationKind::Rtd,
    },
    CalibrationChannel {
        label: "RTD channel 2 (ain9)",
        analog_input_name: "ain9",
        signal_name: "p1_rtd_2.temperature_K",
        slug: "rtd_2",
        kind: CalibrationKind::Rtd,
    },
];

impl CalibrationChannel {
    fn capture_seconds(self) -> u64 {
        match self.kind {
            CalibrationKind::Current4To20 => CAPTURE_SECONDS,
            CalibrationKind::Rtd => RTD_CAPTURE_SECONDS,
        }
    }

    fn fit_units(self) -> &'static str {
        match self.kind {
            CalibrationKind::Current4To20 => "V",
            CalibrationKind::Rtd => "K",
        }
    }

    fn fit_heading(self) -> &'static str {
        match self.kind {
            CalibrationKind::Current4To20 => "Voltage fit",
            CalibrationKind::Rtd => "Temperature fit",
        }
    }

    fn min_step_duration_s(self) -> f64 {
        match self.kind {
            CalibrationKind::Current4To20 => MIN_STEP_DURATION_S,
            CalibrationKind::Rtd => MIN_RTD_STEP_DURATION_S,
        }
    }

    fn display_scale(self) -> f64 {
        match self.kind {
            CalibrationKind::Current4To20 => 1e3,
            CalibrationKind::Rtd => 1.0,
        }
    }

    fn value_axis_label(self) -> &'static str {
        match self.kind {
            CalibrationKind::Current4To20 => "Current (mA)",
            CalibrationKind::Rtd => "Temperature (K)",
        }
    }

    fn reference_axis_label(self) -> &'static str {
        match self.kind {
            CalibrationKind::Current4To20 => "Reference current (mA)",
            CalibrationKind::Rtd => "Reference temperature (K)",
        }
    }

    fn error_plot_title(self) -> &'static str {
        match self.kind {
            CalibrationKind::Current4To20 => "Error vs reference (%FS)",
            CalibrationKind::Rtd => "Temperature error vs reference",
        }
    }

    fn error_axis_label(self) -> &'static str {
        match self.kind {
            CalibrationKind::Current4To20 => "Error (%FS, 20 mA)",
            CalibrationKind::Rtd => "Error (K)",
        }
    }

    fn fit_plot_title(self) -> &'static str {
        match self.kind {
            CalibrationKind::Current4To20 => "Expected voltage vs measured voltage",
            CalibrationKind::Rtd => "Expected temperature vs measured temperature",
        }
    }

    fn fit_x_axis_label(self) -> &'static str {
        match self.kind {
            CalibrationKind::Current4To20 => "Measured voltage (V)",
            CalibrationKind::Rtd => "Measured temperature (K)",
        }
    }

    fn fit_y_axis_label(self) -> &'static str {
        match self.kind {
            CalibrationKind::Current4To20 => "Expected voltage (V)",
            CalibrationKind::Rtd => "Expected temperature (K)",
        }
    }

    fn residual_plot_title(self) -> &'static str {
        match self.kind {
            CalibrationKind::Current4To20 => "Residual after voltage fit, expressed as current",
            CalibrationKind::Rtd => "Residual after temperature fit",
        }
    }

    fn residual_axis_label(self) -> &'static str {
        match self.kind {
            CalibrationKind::Current4To20 => "Estimated current residual (uA)",
            CalibrationKind::Rtd => "Temperature residual (K)",
        }
    }
}

fn all_calibration_channels() -> impl Iterator<Item = CalibrationChannel> {
    CURRENT_CHANNELS.into_iter().chain(RTD_CHANNELS)
}

#[derive(Debug)]
struct ChannelCapture {
    channel: CalibrationChannel,
    op_name: String,
    op_dir: PathBuf,
    timestamps_ns: Vec<i64>,
    times_s: Vec<f64>,
    measured_a: Vec<f64>,
}

#[derive(Debug)]
struct ErrorSample {
    time_s: f64,
    reference_a: f64,
    measured_a: f64,
    error_a: f64,
}

#[derive(Debug)]
struct StepSegment {
    reference_a: f64,
    accept_start_idx: usize,
    accept_end_idx: usize,
    start_time_s: f64,
    stop_time_s: f64,
    accept_start_time_s: f64,
    accept_stop_time_s: f64,
}

#[derive(Debug)]
struct ChannelAnalysis {
    segments: Vec<StepSegment>,
    voltage_fit: VoltageFit,
    rmse_a: f64,
    mean_error_a: f64,
    max_abs_error_a: f64,
    samples: Vec<ErrorSample>,
}

#[derive(Debug)]
struct VoltageFit {
    coefficients: Vec<f64>,
    r2: f64,
    residuals_v: Vec<f64>,
}

#[derive(Debug, Serialize, Deserialize)]
struct CalibrationRecord {
    model: String,
    serial_number: u64,
    channel: String,
    signal_name: String,
    timestamp_ns: i64,
    time_s: f64,
    measured_a: f64,
}

#[derive(Debug, Serialize)]
struct CalibrationJsonRecord {
    model: String,
    serial_number: u64,
    channel: String,
    signal_name: String,
    datestamp_utc: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    reference_resistor_ohm: Option<f64>,
    polynomial_order: usize,
    polynomial_coefficients: Vec<f64>,
    r2: f64,
    input_units: String,
    output_units: String,
    accepted_sample_count: usize,
    detected_step_count: usize,
}

enum RunMode {
    Collect,
    Process(PathBuf),
    CollectAndProcess,
}

enum PromptDecision {
    Start,
    Skip,
}

fn main() -> Result<(), String> {
    let mode = parse_args()?;
    create_dir_all(OUTPUT_ROOT).map_err(|e| format!("Failed to create {OUTPUT_ROOT}: {e}"))?;

    match mode {
        RunMode::Collect => {
            let paths = collect_all_channels(false)?;
            println!("Collected {} calibration data files.", paths.len());
            for path in paths {
                println!("  {}", path.display());
            }
        }
        RunMode::Process(path) => {
            let paths = discover_calibration_data(&path)?;
            process_calibration_files(&paths, summary_dir_for_process_path(&path)?)?;
        }
        RunMode::CollectAndProcess => {
            let paths = collect_all_channels(true)?;
            println!(
                "Collected and processed {} calibration data files.",
                paths.len()
            );
        }
    }

    Ok(())
}

fn parse_args() -> Result<RunMode, String> {
    let args = env::args().skip(1).collect::<Vec<_>>();
    match args.as_slice() {
        [] => Ok(RunMode::CollectAndProcess),
        [mode] if mode == "collect" => Ok(RunMode::Collect),
        [mode, folder] if mode == "process" => Ok(RunMode::Process(PathBuf::from(folder))),
        _ => Err(format!(
            "Usage:\n  rev7_calibration\n  rev7_calibration collect\n  rev7_calibration process <folder-or-calibration-csv>"
        )),
    }
}

fn collect_all_channels(process_after_each_run: bool) -> Result<Vec<PathBuf>, String> {
    println!("Rev7 calibration collection");
    println!(
        "4-20 mA channels use a Fluke 707 auto-step through 0, 5, 10, 15, and 20 mA. Each current channel records for {CAPTURE_SECONDS} s at {RATE_HZ} Hz."
    );
    println!(
        "RTD channels use manual holds from -200 C to +800 C. Each RTD channel records for {RTD_CAPTURE_SECONDS} s at {RATE_HZ} Hz."
    );
    println!(
        "Monitor live channels with: cargo run -p deimos_console -- --config {CONSOLE_CONFIG_PATH}"
    );
    println!(
        "Reporting dispatcher: udp://{}:{}",
        REPORTING_MULTICAST_GROUP, REPORTING_MULTICAST_PORT
    );

    let mut paths = Vec::new();
    for channel in all_calibration_channels() {
        if matches!(prompt_for_channel(channel)?, PromptDecision::Skip) {
            println!("Skipped {}.", channel.label);
            continue;
        }
        let capture = run_channel_capture(channel)?;
        let summary_dir = capture.op_dir.clone();
        let path = write_calibration_data(&capture)?;
        println!("Saved {}", path.display());
        if process_after_each_run {
            process_calibration_files(std::slice::from_ref(&path), &summary_dir)?;
        }
        paths.push(path);
    }

    Ok(paths)
}

fn prompt_for_channel(channel: CalibrationChannel) -> Result<PromptDecision, String> {
    println!();
    println!("Connect the calibrator to {}.", channel.label);
    match channel.kind {
        CalibrationKind::Current4To20 => {
            println!(
                "Start the 0/5/10/15/20 mA auto-step sequence, then press Enter to record {} seconds.",
                channel.capture_seconds(),
            );
        }
        CalibrationKind::Rtd => {
            println!(
                "Manually step from -200 C to +800 C. Hold about 5 s every 10 K below 0 C and every 100 K above 0 C, then press Enter to record {} seconds.",
                channel.capture_seconds(),
            );
        }
    }
    println!("Press Enter to start this run, or type s then Enter to skip it.");
    let mut line = String::new();
    stdin()
        .read_line(&mut line)
        .map_err(|e| format!("Failed to read operator prompt: {e}"))?;

    if line.trim().eq_ignore_ascii_case("s") {
        Ok(PromptDecision::Skip)
    } else {
        Ok(PromptDecision::Start)
    }
}

fn run_channel_capture(channel: CalibrationChannel) -> Result<ChannelCapture, String> {
    let op_name = op_name_for_channel(channel)?;
    let op_dir = Path::new(OUTPUT_ROOT).join(&op_name);
    create_dir_all(&op_dir).map_err(|e| format!("Failed to create {}: {e}", op_dir.display()))?;

    let mut controller =
        Controller::new(controller_context(op_name.clone(), op_dir.clone(), channel));
    controller.add_peripheral(
        PERIPHERAL_NAME,
        Box::new(DeimosDaqRev7 {
            serial_number: SERIAL_NUMBER,
        }),
    );

    let (df_dispatcher, _df_handle) = DataFrameDispatcher::new(DATAFRAME_MB, Overflow::Wrap, None);
    let df_handle = df_dispatcher.handle();
    controller.add_dispatcher("dataframe", df_dispatcher);

    let reporting = ReportingDispatcher::new(
        REPORTING_MULTICAST_GROUP,
        REPORTING_MULTICAST_PORT,
        None,
        REPORTING_SCHEMA_PERIOD,
    );
    let dropped_frames = reporting.dropped_frames_handle();
    controller.add_dispatcher("reporting", reporting);

    controller.run(&None, None)?;

    let dropped = dropped_frames.load(Ordering::Relaxed);
    if dropped != 0 {
        println!(
            "ReportingDispatcher dropped_frames for {}: {dropped}",
            channel.label
        );
    }

    extract_capture(channel, op_name, op_dir, df_handle)
}

fn controller_context(
    op_name: String,
    op_dir: PathBuf,
    channel: CalibrationChannel,
) -> ControllerCtx {
    let mut ctx = ControllerCtx::default();
    ctx.op_name = op_name;
    ctx.op_dir = op_dir;
    ctx.dt_ns = (1e9_f64 / RATE_HZ).round() as u32;
    ctx.loop_method = LoopMethod::Performant;
    ctx.termination_criteria = Some(Termination::Timeout(Duration::from_secs(
        channel.capture_seconds(),
    )));
    ctx
}

fn extract_capture(
    channel: CalibrationChannel,
    op_name: String,
    op_dir: PathBuf,
    df_handle: DataFrameHandle,
) -> Result<ChannelCapture, String> {
    let timestamps = df_handle.timestamp()?;
    let columns = df_handle.columns()?;
    let measurements = columns.get(channel.signal_name).ok_or_else(|| {
        let mut available = columns.keys().cloned().collect::<Vec<_>>();
        available.sort();
        format!(
            "Signal '{}' was not found in dataframe columns: {available:?}",
            channel.signal_name
        )
    })?;

    let first_timestamp = *timestamps
        .first()
        .ok_or_else(|| format!("No samples were captured for {}", channel.label))?;

    let mut timestamps_ns = Vec::with_capacity(timestamps.len());
    let mut times_s = Vec::with_capacity(timestamps.len());
    let mut measured_a = Vec::with_capacity(timestamps.len());
    let mut last_time_s = None;

    for (&timestamp, &measurement) in timestamps.iter().zip(measurements.iter()) {
        let time_s = (timestamp - first_timestamp) as f64 * 1e-9;

        if !time_s.is_finite() || !measurement.is_finite() {
            continue;
        }
        if last_time_s.is_some_and(|last| time_s <= last) {
            continue;
        }

        timestamps_ns.push(timestamp);
        times_s.push(time_s);
        measured_a.push(measurement);
        last_time_s = Some(time_s);
    }

    if times_s.len() < 2 {
        return Err(format!(
            "Need at least two finite monotonic samples for {}, got {}",
            channel.label,
            times_s.len()
        ));
    }

    Ok(ChannelCapture {
        channel,
        op_name,
        op_dir,
        timestamps_ns,
        times_s,
        measured_a,
    })
}

fn write_calibration_data(capture: &ChannelCapture) -> Result<PathBuf, String> {
    let path = capture
        .op_dir
        .join(format!("{}{}", capture.op_name, CALIBRATION_DATA_SUFFIX));
    let mut writer = csv::WriterBuilder::new()
        .has_headers(true)
        .from_path(&path)
        .map_err(|e| format!("Failed to create {}: {e}", path.display()))?;

    for ((&timestamp_ns, &time_s), &measured_a) in capture
        .timestamps_ns
        .iter()
        .zip(capture.times_s.iter())
        .zip(capture.measured_a.iter())
    {
        writer
            .serialize(CalibrationRecord {
                model: MODEL_NAME.to_owned(),
                serial_number: SERIAL_NUMBER,
                channel: capture.channel.slug.to_owned(),
                signal_name: capture.channel.signal_name.to_owned(),
                timestamp_ns,
                time_s,
                measured_a,
            })
            .map_err(|e| format!("Failed to write {}: {e}", path.display()))?;
    }

    writer
        .flush()
        .map_err(|e| format!("Failed to flush {}: {e}", path.display()))?;

    Ok(path)
}

fn discover_calibration_data(path: &Path) -> Result<Vec<PathBuf>, String> {
    let mut paths = Vec::new();

    if path.is_file() {
        if is_calibration_data_file(path) {
            paths.push(path.to_path_buf());
        } else {
            return Err(format!(
                "{} is not a calibration data CSV ending in {CALIBRATION_DATA_SUFFIX}",
                path.display()
            ));
        }
    } else if path.is_dir() {
        discover_calibration_data_recursive(path, &mut paths)?;
    } else {
        return Err(format!("{} does not exist", path.display()));
    }

    paths.sort();
    if paths.is_empty() {
        return Err(format!(
            "No calibration data CSV files ending in {CALIBRATION_DATA_SUFFIX} found under {}",
            path.display()
        ));
    }

    Ok(paths)
}

fn discover_calibration_data_recursive(
    path: &Path,
    paths: &mut Vec<PathBuf>,
) -> Result<(), String> {
    for entry in
        fs::read_dir(path).map_err(|e| format!("Failed to read {}: {e}", path.display()))?
    {
        let entry =
            entry.map_err(|e| format!("Failed to read entry under {}: {e}", path.display()))?;
        let child = entry.path();
        if child.is_dir() {
            discover_calibration_data_recursive(&child, paths)?;
        } else if is_calibration_data_file(&child) {
            paths.push(child);
        }
    }

    Ok(())
}

fn is_calibration_data_file(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.ends_with(CALIBRATION_DATA_SUFFIX))
}

fn summary_dir_for_process_path(path: &Path) -> Result<&Path, String> {
    if path.is_file() {
        path.parent()
            .ok_or_else(|| format!("Unable to determine parent folder for {}", path.display()))
    } else {
        Ok(path)
    }
}

fn process_calibration_files(paths: &[PathBuf], summary_dir: &Path) -> Result<(), String> {
    create_dir_all(summary_dir)
        .map_err(|e| format!("Failed to create {}: {e}", summary_dir.display()))?;
    let summary_path =
        summary_dir.join(format!("rev7_calibration_summary_{}.csv", utc_datestamp()));
    let mut summary = BufWriter::new(
        File::create(&summary_path)
            .map_err(|e| format!("Failed to create {}: {e}", summary_path.display()))?,
    );
    writeln!(
        summary,
        "source,channel,signal,detected_steps,accepted_samples,rmse_a,mean_error_a,max_abs_error_a,voltage_fit_c0,voltage_fit_c1,voltage_fit_r2"
    )
    .map_err(|e| format!("Failed to write {}: {e}", summary_path.display()))?;

    for path in paths {
        let capture = read_calibration_data(path)?;
        let analysis = analyze_capture(&capture)?;
        let samples_path = write_error_samples(&capture, &analysis)?;
        let plot_path = write_analysis_plots(&capture, &analysis)?;
        let record_path = write_calibration_json_record(&capture, &analysis)?;

        writeln!(
            summary,
            "{},{},{},{},{},{:.12},{:.12},{:.12},{:.12},{:.12},{:.12}",
            path.display(),
            capture.channel.label,
            capture.channel.signal_name,
            analysis.segments.len(),
            analysis.samples.len(),
            analysis.rmse_a,
            analysis.mean_error_a,
            analysis.max_abs_error_a,
            analysis.voltage_fit.coefficients[0],
            analysis.voltage_fit.coefficients[1],
            analysis.voltage_fit.r2,
        )
        .map_err(|e| format!("Failed to write {}: {e}", summary_path.display()))?;

        let (error_scale, error_units) = match capture.channel.kind {
            CalibrationKind::Current4To20 => (1e6, "uA"),
            CalibrationKind::Rtd => (1.0, "K"),
        };
        println!(
            "{}: steps={} accepted_samples={} rmse={:.3} {error_units} mean_error={:.3} {error_units} max_abs_error={:.3} {error_units}",
            capture.channel.label,
            analysis.segments.len(),
            analysis.samples.len(),
            analysis.rmse_a * error_scale,
            analysis.mean_error_a * error_scale,
            analysis.max_abs_error_a * error_scale,
        );
        println!("  source {}", path.display());
        println!("  wrote {}", samples_path.display());
        println!("  wrote {}", plot_path.display());
        println!("  wrote {}", record_path.display());
    }

    println!("Summary written to {}", summary_path.display());
    println!(
        "Calibration polynomial records and residual plots were written next to each source file."
    );

    Ok(())
}

fn read_calibration_data(path: &Path) -> Result<ChannelCapture, String> {
    let mut reader = csv::ReaderBuilder::new()
        .has_headers(true)
        .from_path(path)
        .map_err(|e| format!("Failed to open {}: {e}", path.display()))?;
    let mut records = Vec::new();

    for record in reader.deserialize::<CalibrationRecord>() {
        records.push(record.map_err(|e| format!("Failed to read {}: {e}", path.display()))?);
    }

    let first = records
        .first()
        .ok_or_else(|| format!("No calibration records found in {}", path.display()))?;
    let channel = channel_for_signal_name(&first.signal_name)
        .ok_or_else(|| format!("Unknown calibration signal '{}'", first.signal_name))?;
    let op_dir = path
        .parent()
        .ok_or_else(|| format!("Unable to determine parent folder for {}", path.display()))?
        .to_path_buf();
    let op_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .and_then(|name| name.strip_suffix(CALIBRATION_DATA_SUFFIX))
        .unwrap_or("rev7_calibration")
        .to_owned();

    let mut timestamps_ns = Vec::with_capacity(records.len());
    let mut times_s = Vec::with_capacity(records.len());
    let mut measured_a = Vec::with_capacity(records.len());
    let mut last_time_s = None;

    for record in records {
        if record.model != MODEL_NAME {
            return Err(format!(
                "{} has model '{}', expected '{MODEL_NAME}'",
                path.display(),
                record.model
            ));
        }
        if record.signal_name != channel.signal_name {
            return Err(format!(
                "{} contains mixed signals: '{}' and '{}'",
                path.display(),
                channel.signal_name,
                record.signal_name
            ));
        }
        if !record.time_s.is_finite() || !record.measured_a.is_finite() {
            continue;
        }
        if last_time_s.is_some_and(|last| record.time_s <= last) {
            continue;
        }

        timestamps_ns.push(record.timestamp_ns);
        times_s.push(record.time_s);
        measured_a.push(record.measured_a);
        last_time_s = Some(record.time_s);
    }

    if times_s.len() < 2 {
        return Err(format!(
            "Need at least two finite monotonic samples in {}, got {}",
            path.display(),
            times_s.len()
        ));
    }

    Ok(ChannelCapture {
        channel,
        op_name,
        op_dir,
        timestamps_ns,
        times_s,
        measured_a,
    })
}

fn analyze_capture(capture: &ChannelCapture) -> Result<ChannelAnalysis, String> {
    let segments = detect_step_segments(capture)?;
    let samples = accepted_error_samples(capture, &segments);

    if samples.is_empty() {
        return Err(format!(
            "No accepted middle-half step samples found for {}",
            capture.channel.label
        ));
    }
    let voltage_fit = fit_expected_value(&samples, capture.channel.kind)?;

    let mean_error_a =
        samples.iter().map(|sample| sample.error_a).sum::<f64>() / samples.len() as f64;
    let rmse_a = (samples
        .iter()
        .map(|sample| sample.error_a * sample.error_a)
        .sum::<f64>()
        / samples.len() as f64)
        .sqrt();
    let max_abs_error_a = samples
        .iter()
        .map(|sample| sample.error_a.abs())
        .fold(0.0, f64::max);

    Ok(ChannelAnalysis {
        segments,
        voltage_fit,
        rmse_a,
        mean_error_a,
        max_abs_error_a,
        samples,
    })
}

fn fit_expected_value(
    samples: &[ErrorSample],
    kind: CalibrationKind,
) -> Result<VoltageFit, String> {
    let points = samples
        .iter()
        .map(|sample| {
            (
                fit_input_value(sample, kind),
                fit_expected_value_y(sample, kind),
            )
        })
        .collect::<Vec<_>>();
    let coefficients = polyfit(&points, VOLTAGE_FIT_ORDER)?;
    let expected_mean_v = points.iter().map(|(_, y)| *y).sum::<f64>() / points.len() as f64;

    let mut ss_res = 0.0;
    let mut ss_tot = 0.0;
    let mut residuals_v = Vec::with_capacity(points.len());
    for (measured, expected) in points {
        let fitted = polyval(measured, &coefficients);
        let residual = expected - fitted;
        residuals_v.push(residual);
        ss_res += residual * residual;
        let centered_expected_v = expected - expected_mean_v;
        ss_tot += centered_expected_v * centered_expected_v;
    }

    let r2 = if ss_tot > 0.0 {
        1.0 - ss_res / ss_tot
    } else if ss_res == 0.0 {
        1.0
    } else {
        0.0
    };

    Ok(VoltageFit {
        coefficients,
        r2,
        residuals_v,
    })
}

fn detect_step_segments(capture: &ChannelCapture) -> Result<Vec<StepSegment>, String> {
    let mut segments = Vec::new();
    let mut active_reference = None;
    let mut active_start_idx = 0;

    for (idx, &measured_a) in capture.measured_a.iter().enumerate() {
        let reference = nearest_step_reference(measured_a, capture.channel.kind);
        if reference != active_reference {
            if let Some(reference_a) = active_reference {
                push_step_segment(capture, reference_a, active_start_idx, idx, &mut segments);
            }
            active_reference = reference;
            active_start_idx = idx;
        }
    }

    if let Some(reference_a) = active_reference {
        push_step_segment(
            capture,
            reference_a,
            active_start_idx,
            capture.measured_a.len(),
            &mut segments,
        );
    }

    if segments.is_empty() {
        return Err(no_segments_error(capture));
    }

    Ok(segments)
}

fn nearest_step_reference(measured: f64, kind: CalibrationKind) -> Option<f64> {
    match kind {
        CalibrationKind::Current4To20 => STEP_REFERENCE_VALUES_A
            .iter()
            .copied()
            .min_by(|a, b| (measured - *a).abs().total_cmp(&(measured - *b).abs()))
            .filter(|reference_a| (measured - *reference_a).abs() <= STEP_DETECTION_TOLERANCE_A),
        CalibrationKind::Rtd => nearest_rtd_reference_k(measured),
    }
}

fn nearest_rtd_reference_k(measured_k: f64) -> Option<f64> {
    if !(RTD_MIN_REFERENCE_K..=RTD_MAX_REFERENCE_K).contains(&measured_k) {
        return None;
    }

    let step_k = if measured_k < ZERO_C_K {
        RTD_STEP_BELOW_ZERO_K
    } else {
        RTD_STEP_ABOVE_ZERO_K
    };
    let reference_k = ZERO_C_K + ((measured_k - ZERO_C_K) / step_k).round() * step_k;

    if (RTD_MIN_REFERENCE_K..=RTD_MAX_REFERENCE_K).contains(&reference_k)
        && (measured_k - reference_k).abs() <= RTD_STEP_DETECTION_TOLERANCE_K
    {
        Some(reference_k)
    } else {
        None
    }
}

fn no_segments_error(capture: &ChannelCapture) -> String {
    match capture.channel.kind {
        CalibrationKind::Current4To20 => format!(
            "No stable 0/5/10/15/20 mA steps found for {} using +/- {:.3} mA tolerance and {:.1} s minimum duration",
            capture.channel.label,
            STEP_DETECTION_TOLERANCE_A * 1e3,
            capture.channel.min_step_duration_s(),
        ),
        CalibrationKind::Rtd => format!(
            "No stable RTD temperature holds found for {} using +/- {:.1} K tolerance and {:.1} s minimum duration",
            capture.channel.label,
            RTD_STEP_DETECTION_TOLERANCE_K,
            capture.channel.min_step_duration_s(),
        ),
    }
}

fn push_step_segment(
    capture: &ChannelCapture,
    reference_a: f64,
    start_idx: usize,
    end_idx: usize,
    segments: &mut Vec<StepSegment>,
) {
    if end_idx <= start_idx + 1 {
        return;
    }

    let start_time_s = capture.times_s[start_idx];
    let stop_time_s = capture.times_s[end_idx - 1];
    let duration_s = stop_time_s - start_time_s;
    if duration_s < capture.channel.min_step_duration_s() {
        return;
    }

    let accept_start_time_s = start_time_s + 0.25 * duration_s;
    let accept_stop_time_s = start_time_s + 0.75 * duration_s;
    let accept_start_idx = (start_idx..end_idx)
        .find(|&idx| capture.times_s[idx] >= accept_start_time_s)
        .unwrap_or(start_idx);
    let accept_end_idx = (accept_start_idx..end_idx)
        .find(|&idx| capture.times_s[idx] > accept_stop_time_s)
        .unwrap_or(end_idx);

    if accept_end_idx <= accept_start_idx {
        return;
    }

    segments.push(StepSegment {
        reference_a,
        accept_start_idx,
        accept_end_idx,
        start_time_s,
        stop_time_s,
        accept_start_time_s,
        accept_stop_time_s,
    });
}

fn accepted_error_samples(capture: &ChannelCapture, segments: &[StepSegment]) -> Vec<ErrorSample> {
    let mut samples = Vec::new();
    for segment in segments {
        for idx in segment.accept_start_idx..segment.accept_end_idx {
            let measured_a = capture.measured_a[idx];
            samples.push(ErrorSample {
                time_s: capture.times_s[idx],
                reference_a: segment.reference_a,
                measured_a,
                error_a: segment.reference_a - measured_a,
            });
        }
    }

    samples
}

fn measured_voltage_v(sample: &ErrorSample) -> f64 {
    sample.measured_a * REFERENCE_RESISTOR_OHM
}

fn expected_voltage_v(reference_a: f64) -> f64 {
    reference_a * REFERENCE_RESISTOR_OHM
}

fn fit_input_value(sample: &ErrorSample, kind: CalibrationKind) -> f64 {
    match kind {
        CalibrationKind::Current4To20 => measured_voltage_v(sample),
        CalibrationKind::Rtd => sample.measured_a,
    }
}

fn fit_expected_value_y(sample: &ErrorSample, kind: CalibrationKind) -> f64 {
    match kind {
        CalibrationKind::Current4To20 => expected_voltage_v(sample.reference_a),
        CalibrationKind::Rtd => sample.reference_a,
    }
}

fn write_error_samples(
    capture: &ChannelCapture,
    analysis: &ChannelAnalysis,
) -> Result<PathBuf, String> {
    let path = capture
        .op_dir
        .join(format!("{}_error_samples.csv", capture.op_name));
    let mut writer = BufWriter::new(
        File::create(&path).map_err(|e| format!("Failed to create {}: {e}", path.display()))?,
    );
    writeln!(writer, "time_s,reference_a,measured_a,error_a")
        .map_err(|e| format!("Failed to write {}: {e}", path.display()))?;

    for sample in &analysis.samples {
        writeln!(
            writer,
            "{:.9},{:.12},{:.12},{:.12}",
            sample.time_s, sample.reference_a, sample.measured_a, sample.error_a
        )
        .map_err(|e| format!("Failed to write {}: {e}", path.display()))?;
    }

    Ok(path)
}

fn write_calibration_json_record(
    capture: &ChannelCapture,
    analysis: &ChannelAnalysis,
) -> Result<PathBuf, String> {
    let path = capture
        .op_dir
        .join(format!("{}_calibration.json", capture.op_name));
    let record = CalibrationJsonRecord {
        model: MODEL_NAME.to_owned(),
        serial_number: SERIAL_NUMBER,
        channel: capture.channel.analog_input_name.to_owned(),
        signal_name: capture.channel.signal_name.to_owned(),
        datestamp_utc: utc_datestamp(),
        reference_resistor_ohm: match capture.channel.kind {
            CalibrationKind::Current4To20 => Some(REFERENCE_RESISTOR_OHM),
            CalibrationKind::Rtd => None,
        },
        polynomial_order: VOLTAGE_FIT_ORDER,
        polynomial_coefficients: analysis.voltage_fit.coefficients.clone(),
        r2: analysis.voltage_fit.r2,
        input_units: capture.channel.fit_units().to_owned(),
        output_units: capture.channel.fit_units().to_owned(),
        accepted_sample_count: analysis.samples.len(),
        detected_step_count: analysis.segments.len(),
    };
    let json = serde_json::to_string_pretty(&record)
        .map_err(|e| format!("Failed to serialize calibration record: {e}"))?;
    fs::write(&path, json).map_err(|e| format!("Failed to write {}: {e}", path.display()))?;

    Ok(path)
}

fn write_analysis_plots(
    capture: &ChannelCapture,
    analysis: &ChannelAnalysis,
) -> Result<PathBuf, String> {
    let path = capture
        .op_dir
        .join(format!("{}_plots.html", capture.op_name));
    let display_scale = capture.channel.display_scale();

    let raw_time_s = capture.times_s.clone();
    let raw_measured_ma = capture
        .measured_a
        .iter()
        .map(|measured_a| measured_a * display_scale)
        .collect::<Vec<_>>();
    let accepted_time_s = analysis
        .samples
        .iter()
        .map(|sample| sample.time_s)
        .collect::<Vec<_>>();
    let accepted_measured_ma = analysis
        .samples
        .iter()
        .map(|sample| sample.measured_a * display_scale)
        .collect::<Vec<_>>();
    let accepted_reference_ma = analysis
        .samples
        .iter()
        .map(|sample| sample.reference_a * display_scale)
        .collect::<Vec<_>>();
    let error_plot_y = analysis
        .samples
        .iter()
        .map(|sample| match capture.channel.kind {
            CalibrationKind::Current4To20 => 100.0 * sample.error_a / REFERENCE_MAX_A,
            CalibrationKind::Rtd => sample.error_a,
        })
        .collect::<Vec<_>>();
    let fit_measured_x = analysis
        .samples
        .iter()
        .map(|sample| fit_input_value(sample, capture.channel.kind))
        .collect::<Vec<_>>();
    let fit_expected_y = analysis
        .samples
        .iter()
        .map(|sample| fit_expected_value_y(sample, capture.channel.kind))
        .collect::<Vec<_>>();
    let min_fit_x = fit_measured_x.iter().copied().fold(f64::INFINITY, f64::min);
    let max_fit_x = fit_measured_x
        .iter()
        .copied()
        .fold(f64::NEG_INFINITY, f64::max);
    let fit_line_x = vec![min_fit_x, max_fit_x];
    let fit_line_y = fit_line_x
        .iter()
        .map(|&measured| polyval(measured, &analysis.voltage_fit.coefficients))
        .collect::<Vec<_>>();
    let fit_residual_y = analysis
        .voltage_fit
        .residuals_v
        .iter()
        .map(|residual| match capture.channel.kind {
            CalibrationKind::Current4To20 => residual / REFERENCE_RESISTOR_OHM * 1e6,
            CalibrationKind::Rtd => *residual,
        })
        .collect::<Vec<_>>();
    let voltage_fit_label = format!(
        "{}: expected = {:.9} + {:.9} * measured, R^2 = {:.9}",
        capture.channel.fit_heading(),
        analysis.voltage_fit.coefficients[0],
        analysis.voltage_fit.coefficients[1],
        analysis.voltage_fit.r2,
    );

    let mut reference_step_time_s = Vec::new();
    let mut reference_step_ma = Vec::new();
    for segment in &analysis.segments {
        reference_step_time_s.push(Some(segment.start_time_s));
        reference_step_time_s.push(Some(segment.stop_time_s));
        reference_step_time_s.push(None);
        reference_step_ma.push(Some(segment.reference_a * display_scale));
        reference_step_ma.push(Some(segment.reference_a * display_scale));
        reference_step_ma.push(None);
    }
    let accepted_shapes = analysis
        .segments
        .iter()
        .map(|segment| {
            serde_json::json!({
                "type": "rect",
                "xref": "x",
                "yref": "paper",
                "x0": segment.accept_start_time_s,
                "x1": segment.accept_stop_time_s,
                "y0": 0.0,
                "y1": 1.0,
                "fillcolor": "rgba(34, 136, 51, 0.14)",
                "line": { "width": 0 },
                "layer": "below",
            })
        })
        .collect::<Vec<_>>();

    let title = format!(
        "{} SN{} {} calibration",
        MODEL_NAME, SERIAL_NUMBER, capture.channel.slug
    );
    let html = format!(
        r##"<!doctype html>
<html lang="en">
<head>
    <meta charset="utf-8" />
    <title>{title}</title>
    <script src="https://cdn.plot.ly/plotly-3.0.1.min.js"></script>
    <style>
        html, body {{
            margin: 0;
            background: #f7f7f2;
            color: #1f2523;
            font-family: system-ui, -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif;
        }}
        main {{
            box-sizing: border-box;
            width: min(1280px, 100vw);
            margin: 0 auto;
            padding: 20px;
        }}
        h1 {{
            margin: 0 0 16px;
            font-size: 22px;
            font-weight: 650;
        }}
        .plot {{
            width: 100%;
            height: 460px;
            margin-bottom: 24px;
        }}
        .plot-heading {{
            margin: 8px 0 8px;
            font-size: 16px;
            font-weight: 650;
            color: #1f2523;
        }}
    </style>
</head>
<body>
<main>
    <h1>{title}</h1>
    <div id="time-overlay" class="plot"></div>
    <div id="relative-error" class="plot"></div>
    <h2 class="plot-heading">{voltage_fit_label_html}</h2>
    <div id="voltage-fit" class="plot"></div>
    <div id="voltage-fit-residual" class="plot"></div>
</main>
<script>
const rawTimeS = {raw_time_s};
const rawMeasuredMA = {raw_measured_ma};
const acceptedTimeS = {accepted_time_s};
const acceptedMeasuredMA = {accepted_measured_ma};
const acceptedReferenceMA = {accepted_reference_ma};
const referenceStepTimeS = {reference_step_time_s};
const referenceStepMA = {reference_step_ma};
const errorPlotY = {error_plot_y};
const acceptedShapes = {accepted_shapes};
const fitMeasuredX = {fit_measured_x};
const fitExpectedY = {fit_expected_y};
const fitLineX = {fit_line_x};
const fitLineY = {fit_line_y};
const fitResidualY = {fit_residual_y};
const voltageFitLabel = {voltage_fit_label};

Plotly.newPlot("time-overlay", [
    {{
        x: rawTimeS,
        y: rawMeasuredMA,
        mode: "markers",
        type: "scatter",
        name: "Measured",
        marker: {{ size: 4, color: "rgba(51, 102, 204, 0.42)" }}
    }},
    {{
        x: referenceStepTimeS,
        y: referenceStepMA,
        mode: "lines",
        type: "scatter",
        name: "Detected reference step",
        connectgaps: false,
        line: {{ width: 2, color: "#d65f00" }}
    }},
    {{
        x: acceptedTimeS,
        y: acceptedMeasuredMA,
        mode: "markers",
        type: "scatter",
        name: "Accepted middle-half samples",
        marker: {{ size: 5, color: "#228833" }}
    }}
], {{
    title: {{ text: "Detected steps and accepted calibration regions" }},
    xaxis: {{ title: {{ text: "Time (s)", standoff: 16 }}, automargin: true }},
    yaxis: {{ title: {{ text: {value_axis_label}, standoff: 18 }}, automargin: true }},
    shapes: acceptedShapes,
    margin: {{ l: 88, r: 24, t: 64, b: 78 }}
}}, {{ responsive: true }});

Plotly.newPlot("relative-error", [
    {{
        x: acceptedReferenceMA,
        y: errorPlotY,
        mode: "markers",
        type: "scatter",
        name: "Error",
        marker: {{ size: 4, color: "#228833" }}
    }}
], {{
    title: {{ text: {error_plot_title} }},
    xaxis: {{ title: {{ text: {reference_axis_label}, standoff: 16 }}, automargin: true }},
    yaxis: {{ title: {{ text: {error_axis_label}, standoff: 18 }}, automargin: true }},
    margin: {{ l: 88, r: 24, t: 64, b: 78 }}
}}, {{ responsive: true }});

Plotly.newPlot("voltage-fit", [
    {{
        x: fitMeasuredX,
        y: fitExpectedY,
        mode: "markers",
        type: "scatter",
        name: "Accepted calibration samples",
        marker: {{ size: 5, color: "#3366cc" }}
    }},
    {{
        x: fitLineX,
        y: fitLineY,
        mode: "lines",
        type: "scatter",
        name: "Linear fit",
        line: {{ width: 2, color: "#d65f00" }}
    }}
], {{
    title: {{ text: {fit_plot_title} }},
    xaxis: {{ title: {{ text: {fit_x_axis_label}, standoff: 16 }}, automargin: true }},
    yaxis: {{ title: {{ text: {fit_y_axis_label}, standoff: 18 }}, automargin: true }},
    margin: {{ l: 88, r: 24, t: 100, b: 78 }}
}}, {{ responsive: true }});

Plotly.newPlot("voltage-fit-residual", [
    {{
        x: fitMeasuredX,
        y: fitResidualY,
        mode: "markers",
        type: "scatter",
        name: "Fit residual as current",
        marker: {{ size: 5, color: "#aa3377" }}
    }}
], {{
    title: {{ text: {residual_plot_title} }},
    xaxis: {{ title: {{ text: {fit_x_axis_label}, standoff: 16 }}, automargin: true }},
    yaxis: {{ title: {{ text: {residual_axis_label}, standoff: 18 }}, automargin: true }},
    margin: {{ l: 88, r: 24, t: 64, b: 78 }}
}}, {{ responsive: true }});
</script>
</body>
</html>
"##,
        title = html_escape(&title),
        voltage_fit_label_html = html_escape(&voltage_fit_label),
        value_axis_label = serde_json::to_string(capture.channel.value_axis_label())
            .map_err(|e| format!("Failed to serialize value axis label: {e}"))?,
        raw_time_s = serde_json::to_string(&raw_time_s)
            .map_err(|e| format!("Failed to serialize plot raw time data: {e}"))?,
        raw_measured_ma = serde_json::to_string(&raw_measured_ma)
            .map_err(|e| format!("Failed to serialize plot raw measurement data: {e}"))?,
        accepted_time_s = serde_json::to_string(&accepted_time_s)
            .map_err(|e| format!("Failed to serialize plot accepted time data: {e}"))?,
        accepted_measured_ma = serde_json::to_string(&accepted_measured_ma)
            .map_err(|e| format!("Failed to serialize plot accepted measurement data: {e}"))?,
        accepted_reference_ma = serde_json::to_string(&accepted_reference_ma)
            .map_err(|e| format!("Failed to serialize plot accepted reference data: {e}"))?,
        reference_step_time_s = serde_json::to_string(&reference_step_time_s)
            .map_err(|e| format!("Failed to serialize plot reference-step time data: {e}"))?,
        reference_step_ma = serde_json::to_string(&reference_step_ma)
            .map_err(|e| format!("Failed to serialize plot reference-step data: {e}"))?,
        error_plot_y = serde_json::to_string(&error_plot_y)
            .map_err(|e| format!("Failed to serialize plot relative-error y data: {e}"))?,
        accepted_shapes = serde_json::to_string(&accepted_shapes)
            .map_err(|e| format!("Failed to serialize plot accepted-region shapes: {e}"))?,
        fit_measured_x = serde_json::to_string(&fit_measured_x)
            .map_err(|e| format!("Failed to serialize plot measured-voltage data: {e}"))?,
        fit_expected_y = serde_json::to_string(&fit_expected_y)
            .map_err(|e| format!("Failed to serialize plot expected-voltage data: {e}"))?,
        fit_line_x = serde_json::to_string(&fit_line_x)
            .map_err(|e| format!("Failed to serialize plot fit-line measured-voltage data: {e}"))?,
        fit_line_y = serde_json::to_string(&fit_line_y)
            .map_err(|e| format!("Failed to serialize plot fit-line expected-voltage data: {e}"))?,
        fit_residual_y = serde_json::to_string(&fit_residual_y)
            .map_err(|e| format!("Failed to serialize plot fit residual current data: {e}"))?,
        voltage_fit_label = serde_json::to_string(&voltage_fit_label)
            .map_err(|e| format!("Failed to serialize voltage-fit label: {e}"))?,
        error_plot_title = serde_json::to_string(capture.channel.error_plot_title())
            .map_err(|e| format!("Failed to serialize error plot title: {e}"))?,
        reference_axis_label = serde_json::to_string(capture.channel.reference_axis_label())
            .map_err(|e| format!("Failed to serialize reference axis label: {e}"))?,
        error_axis_label = serde_json::to_string(capture.channel.error_axis_label())
            .map_err(|e| format!("Failed to serialize error axis label: {e}"))?,
        fit_plot_title = serde_json::to_string(capture.channel.fit_plot_title())
            .map_err(|e| format!("Failed to serialize fit plot title: {e}"))?,
        fit_x_axis_label = serde_json::to_string(capture.channel.fit_x_axis_label())
            .map_err(|e| format!("Failed to serialize fit x-axis label: {e}"))?,
        fit_y_axis_label = serde_json::to_string(capture.channel.fit_y_axis_label())
            .map_err(|e| format!("Failed to serialize fit y-axis label: {e}"))?,
        residual_plot_title = serde_json::to_string(capture.channel.residual_plot_title())
            .map_err(|e| format!("Failed to serialize residual plot title: {e}"))?,
        residual_axis_label = serde_json::to_string(capture.channel.residual_axis_label())
            .map_err(|e| format!("Failed to serialize residual axis label: {e}"))?,
    );

    fs::write(&path, html).map_err(|e| format!("Failed to write {}: {e}", path.display()))?;
    Ok(path)
}

fn op_name_for_channel(channel: CalibrationChannel) -> Result<String, String> {
    Ok(format!(
        "{MODEL_NAME}_sn{SERIAL_NUMBER}_{}_{}",
        channel.slug,
        utc_datestamp()
    ))
}

fn utc_datestamp() -> String {
    DateTime::<Utc>::from(SystemTime::now())
        .to_rfc3339_opts(SecondsFormat::Secs, true)
        .replace(':', "")
}

fn channel_for_signal_name(signal_name: &str) -> Option<CalibrationChannel> {
    all_calibration_channels().find(|channel| channel.signal_name == signal_name)
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}
