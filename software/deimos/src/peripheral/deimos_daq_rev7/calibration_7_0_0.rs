//! Rev7 DAQ 7.0.0 calibration procedure.
//!
//! This module collects and postprocesses manually assisted rev7 calibration
//! runs for 4-20 mA, RTD, thermocouple, and voltage inputs. It writes one
//! calibration CSV per channel, then processes calibration CSVs by loading them
//! back from disk.

use std::{
    fs::{self, File, create_dir_all},
    io::{BufWriter, Write, stdin},
    net::Ipv4Addr,
    path::{Path, PathBuf},
    sync::atomic::Ordering,
    thread::sleep,
    time::{Duration, Instant, SystemTime},
};

use crate::{
    Controller, ControllerCtx, DataFrameDispatcher, LoopMethod, Overflow, Termination,
    calc::{ktype_corrected_temp_k, ktype_voltage_v, pt100_resistance_ohm, pt100_temp_k},
    dispatcher::{DataFrameHandle, ReportingDispatcher},
    math::{polyfit, polyval},
};
use chrono::{DateTime, SecondsFormat, Utc};

use super::DeimosDaqRev7;
use serde::{Deserialize, Serialize};

const MODEL_NAME: &str = "deimos_daq_rev7";
const PERIPHERAL_NAME: &str = "p1";
const RATE_HZ: f64 = 100.0;
const DATAFRAME_MB: usize = 64;
const REPORTING_MULTICAST_GROUP: Ipv4Addr = Ipv4Addr::new(239, 255, 0, 1);
const REPORTING_MULTICAST_PORT: u16 = 29573;
const REPORTING_SCHEMA_PERIOD: Duration = Duration::from_secs(2);

const REFERENCE_MAX_A: f64 = 0.020;
const STEP_REFERENCE_VALUES_A: [f64; 5] = [0.0, 0.005, 0.010, 0.015, 0.020];
const STEP_DETECTION_TOLERANCE_A: f64 = 0.0015;
const MIN_STEP_DURATION_S: f64 = 2.5;
const CAPTURE_SECONDS: u64 = 90;
const REFERENCE_RESISTOR_OHM: f64 = 75.0;
const FLUKE_707_CURRENT_ACCURACY_A: f64 = REFERENCE_MAX_A * 0.015 / 100.0 + 2.0e-6;
const FLUKE_707_VOLTAGE_ACCURACY_READING_FRAC: f64 = 0.015 / 100.0;
const FLUKE_707_VOLTAGE_ACCURACY_V: f64 = 2.0e-3;
const VOLTAGE_FIT_ORDER: usize = 1;
const VOLTAGE_HOLD_COUNT: usize = 4;
const VOLTAGE_HOLD_SECONDS: f64 = 5.0;
const MIN_VOLTAGE_HOLD_DURATION_S: f64 = 2.0;
const VOLTAGE_CAPTURE_SECONDS: u64 = 300;
const VOLTAGE_0_2V5_MAX_V: f64 = 2.5;
const VOLTAGE_0_15V_MAX_V: f64 = 15.0;
const VOLTAGE_X26_MIN_V: f64 = -1.024 / 25.7;
const VOLTAGE_X26_MAX_V: f64 = (VOLTAGE_0_2V5_MAX_V - 1.024) / 25.7;

const ZERO_C_K: f64 = 273.15;
const RTD_MIN_REFERENCE_K: f64 = ZERO_C_K - 200.0;
const RTD_MAX_REFERENCE_K: f64 = ZERO_C_K + 800.0;
const RTD_STEP_K: f64 = 10.0;
const RTD_STEP_DETECTION_TOLERANCE_K: f64 = 5.0;
const MIN_RTD_STEP_DURATION_S: f64 = 0.5;
const RTD_CAPTURE_SECONDS: u64 = 240;
const VA720_ACCURACY_K: f64 = 0.33;
const RTD_REFERENCE_CURRENT_A: f64 = 250e-6;
const RTD_FRONTEND_GAIN: f64 = 25.7;

const TC_MIN_REFERENCE_K: f64 = ZERO_C_K - 210.0;
const TC_MAX_REFERENCE_K: f64 = ZERO_C_K + 1260.0;
const TC_STEP_K: f64 = 10.0;
const TC_STEP_DETECTION_TOLERANCE_K: f64 = 5.0;
const MIN_TC_STEP_DURATION_S: f64 = 1.0;
const TC_CAPTURE_SECONDS: u64 = 240;
const VA710_TEMPERATURE_ACCURACY_K: f64 = 0.3;
const VA710_COLD_JUNCTION_ACCURACY_K: f64 = 0.3;
const VA710_NEAR_ROOM_VOLTAGE_ACCURACY_K: f64 = 0.25;
const BOARD_COLD_JUNCTION_SIGNAL_NAME: &str = "p1_board_rtd_filtered.y";

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
    Thermocouple,
    Voltage,
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

const THERMOCOUPLE_CHANNELS: [CalibrationChannel; 2] = [
    CalibrationChannel {
        label: "Thermocouple channel 0 (ain10)",
        analog_input_name: "ain10",
        signal_name: "p1_tc_0.temperature_K",
        slug: "tc_0",
        kind: CalibrationKind::Thermocouple,
    },
    CalibrationChannel {
        label: "Thermocouple channel 1 (ain11)",
        analog_input_name: "ain11",
        signal_name: "p1_tc_1.temperature_K",
        slug: "tc_1",
        kind: CalibrationKind::Thermocouple,
    },
];

const VOLTAGE_CHANNELS: [CalibrationChannel; 6] = [
    CalibrationChannel {
        label: "0-2.5 V channel 0 (ain12)",
        analog_input_name: "ain12",
        signal_name: "p1_0_2V5_0.y",
        slug: "0_2V5_0",
        kind: CalibrationKind::Voltage,
    },
    CalibrationChannel {
        label: "0-2.5 V channel 1 (ain15)",
        analog_input_name: "ain15",
        signal_name: "p1_0_2V5_1.y",
        slug: "0_2V5_1",
        kind: CalibrationKind::Voltage,
    },
    CalibrationChannel {
        label: "0-15 V channel 0 (ain16)",
        analog_input_name: "ain16",
        signal_name: "p1_0_15V_0.y",
        slug: "0_15V_0",
        kind: CalibrationKind::Voltage,
    },
    CalibrationChannel {
        label: "0-15 V channel 1 (ain17)",
        analog_input_name: "ain17",
        signal_name: "p1_0_15V_1.y",
        slug: "0_15V_1",
        kind: CalibrationKind::Voltage,
    },
    CalibrationChannel {
        label: "x26 voltage channel 0 (ain18)",
        analog_input_name: "ain18",
        signal_name: "p1_x26_0.y",
        slug: "x26_0",
        kind: CalibrationKind::Voltage,
    },
    CalibrationChannel {
        label: "x26 voltage channel 1 (ain19)",
        analog_input_name: "ain19",
        signal_name: "p1_x26_1.y",
        slug: "x26_1",
        kind: CalibrationKind::Voltage,
    },
];

impl CalibrationChannel {
    fn capture_seconds(self) -> u64 {
        match self.kind {
            CalibrationKind::Current4To20 => CAPTURE_SECONDS,
            CalibrationKind::Rtd => RTD_CAPTURE_SECONDS,
            CalibrationKind::Thermocouple => TC_CAPTURE_SECONDS,
            CalibrationKind::Voltage => VOLTAGE_CAPTURE_SECONDS,
        }
    }

    fn fit_units(self) -> &'static str {
        match self.kind {
            CalibrationKind::Current4To20 => "V",
            CalibrationKind::Rtd => "V",
            CalibrationKind::Thermocouple => "V",
            CalibrationKind::Voltage => "V",
        }
    }

    fn fit_heading(self) -> &'static str {
        match self.kind {
            CalibrationKind::Current4To20 => "Voltage fit",
            CalibrationKind::Rtd => "Voltage fit",
            CalibrationKind::Thermocouple => "Thermocouple voltage fit",
            CalibrationKind::Voltage => "Voltage fit",
        }
    }

    fn min_step_duration_s(self) -> f64 {
        match self.kind {
            CalibrationKind::Current4To20 => MIN_STEP_DURATION_S,
            CalibrationKind::Rtd => MIN_RTD_STEP_DURATION_S,
            CalibrationKind::Thermocouple => MIN_TC_STEP_DURATION_S,
            CalibrationKind::Voltage => MIN_VOLTAGE_HOLD_DURATION_S,
        }
    }

    fn display_scale(self) -> f64 {
        match self.kind {
            CalibrationKind::Current4To20 => 1e3,
            CalibrationKind::Rtd => 1.0,
            CalibrationKind::Thermocouple => 1.0,
            CalibrationKind::Voltage => 1.0,
        }
    }

    fn value_axis_label(self) -> &'static str {
        match self.kind {
            CalibrationKind::Current4To20 => "Current (mA)",
            CalibrationKind::Rtd => "Temperature (K)",
            CalibrationKind::Thermocouple => "Adjusted temperature (K)",
            CalibrationKind::Voltage => "Voltage (V)",
        }
    }

    fn reference_axis_label(self) -> &'static str {
        match self.kind {
            CalibrationKind::Current4To20 => "Reference current (mA)",
            CalibrationKind::Rtd => "Reference temperature (K)",
            CalibrationKind::Thermocouple => "Reference temperature (K)",
            CalibrationKind::Voltage => "Reference voltage (V)",
        }
    }

    fn error_plot_title(self) -> &'static str {
        match self.kind {
            CalibrationKind::Current4To20 => "Current error vs reference",
            CalibrationKind::Rtd => "Temperature error vs reference",
            CalibrationKind::Thermocouple => "Thermocouple temperature error vs reference",
            CalibrationKind::Voltage => "Voltage error vs reference",
        }
    }

    fn error_axis_label(self) -> &'static str {
        match self.kind {
            CalibrationKind::Current4To20 => "Error (mA)",
            CalibrationKind::Rtd => "Error (K)",
            CalibrationKind::Thermocouple => "Error (K)",
            CalibrationKind::Voltage => "Error (V)",
        }
    }

    fn error_accuracy_limit(self) -> f64 {
        match self.kind {
            CalibrationKind::Current4To20 => FLUKE_707_CURRENT_ACCURACY_A * 1e3,
            CalibrationKind::Rtd => VA720_ACCURACY_K,
            CalibrationKind::Thermocouple => {
                VA710_TEMPERATURE_ACCURACY_K
                    + VA710_COLD_JUNCTION_ACCURACY_K
                    + VA710_NEAR_ROOM_VOLTAGE_ACCURACY_K
            }
            CalibrationKind::Voltage => 0.0,
        }
    }

    fn error_accuracy_limit_at_reference(self, reference: f64) -> f64 {
        match self.kind {
            CalibrationKind::Voltage => fluke_707_voltage_accuracy_v(reference),
            _ => self.error_accuracy_limit(),
        }
    }

    fn error_accuracy_label(self) -> &'static str {
        match self.kind {
            CalibrationKind::Current4To20 => "Fluke 707 accuracy range",
            CalibrationKind::Rtd => "VA720 accuracy range",
            CalibrationKind::Thermocouple => "VA710 approximate accuracy range",
            CalibrationKind::Voltage => "Fluke 707 voltage accuracy range",
        }
    }

    fn fit_plot_title(self) -> &'static str {
        match self.kind {
            CalibrationKind::Current4To20 => "Expected voltage vs measured voltage",
            CalibrationKind::Rtd => "Corrected RTD voltage vs measured voltage",
            CalibrationKind::Thermocouple => "Corrected thermocouple voltage vs measured voltage",
            CalibrationKind::Voltage => "Expected voltage vs measured voltage",
        }
    }

    fn fit_x_axis_label(self) -> &'static str {
        match self.kind {
            CalibrationKind::Current4To20 => "Measured voltage (V)",
            CalibrationKind::Rtd => "Measured RTD voltage (V)",
            CalibrationKind::Thermocouple => "Measured thermocouple voltage (V)",
            CalibrationKind::Voltage => "Measured voltage (V)",
        }
    }

    fn fit_y_axis_label(self) -> &'static str {
        match self.kind {
            CalibrationKind::Current4To20 => "Expected voltage (V)",
            CalibrationKind::Rtd => "Corrected RTD voltage (V)",
            CalibrationKind::Thermocouple => "Corrected thermocouple voltage (V)",
            CalibrationKind::Voltage => "Expected voltage (V)",
        }
    }

    fn residual_plot_title(self) -> &'static str {
        match self.kind {
            CalibrationKind::Current4To20 => "Residual after voltage fit, expressed as current",
            CalibrationKind::Rtd => "Residual temperature error after voltage fit propagation",
            CalibrationKind::Thermocouple => {
                "Residual temperature error after voltage fit propagation"
            }
            CalibrationKind::Voltage => "Residual after voltage fit",
        }
    }

    fn residual_axis_label(self) -> &'static str {
        match self.kind {
            CalibrationKind::Current4To20 => "Estimated current residual (uA)",
            CalibrationKind::Rtd => "Temperature residual (K)",
            CalibrationKind::Thermocouple => "Temperature residual (K)",
            CalibrationKind::Voltage => "Voltage residual (V)",
        }
    }

    fn residual_x_axis_label(self) -> &'static str {
        self.reference_axis_label()
    }

    fn residual_accuracy_limit(self) -> f64 {
        match self.kind {
            CalibrationKind::Current4To20 => FLUKE_707_CURRENT_ACCURACY_A * 1e6,
            CalibrationKind::Rtd => VA720_ACCURACY_K,
            CalibrationKind::Thermocouple => {
                VA710_TEMPERATURE_ACCURACY_K
                    + VA710_COLD_JUNCTION_ACCURACY_K
                    + VA710_NEAR_ROOM_VOLTAGE_ACCURACY_K
            }
            CalibrationKind::Voltage => 0.0,
        }
    }

    fn residual_accuracy_limit_at_reference(self, reference: f64) -> f64 {
        match self.kind {
            CalibrationKind::Voltage => fluke_707_voltage_accuracy_v(reference),
            _ => self.residual_accuracy_limit(),
        }
    }

    fn residual_accuracy_label(self) -> &'static str {
        match self.kind {
            CalibrationKind::Current4To20 => "Fluke 707 accuracy range",
            CalibrationKind::Rtd => "VA720 accuracy range",
            CalibrationKind::Thermocouple => "VA710 approximate accuracy range",
            CalibrationKind::Voltage => "Fluke 707 voltage accuracy range",
        }
    }

    fn voltage_reference_range_v(self) -> Option<(f64, f64)> {
        match self.slug {
            "0_2V5_0" | "0_2V5_1" => Some((0.0, VOLTAGE_0_2V5_MAX_V)),
            "0_15V_0" | "0_15V_1" => Some((0.0, VOLTAGE_0_15V_MAX_V)),
            "x26_0" | "x26_1" => Some((VOLTAGE_X26_MIN_V, VOLTAGE_X26_MAX_V)),
            _ => None,
        }
    }

    fn tc_voltage_signal_name(self) -> Option<&'static str> {
        match self.slug {
            "tc_0" => Some("p1_tc_0_V.y"),
            "tc_1" => Some("p1_tc_1_V.y"),
            _ => None,
        }
    }

    fn calibration_signal_name(self) -> &'static str {
        self.tc_voltage_signal_name().unwrap_or(self.signal_name)
    }
}

fn all_calibration_channels() -> impl Iterator<Item = CalibrationChannel> {
    CURRENT_CHANNELS
        .into_iter()
        .chain(RTD_CHANNELS)
        .chain(THERMOCOUPLE_CHANNELS)
        .chain(VOLTAGE_CHANNELS)
}

#[derive(Debug)]
struct ChannelCapture {
    serial_number: u64,
    channel: CalibrationChannel,
    op_name: String,
    op_dir: PathBuf,
    timestamps_ns: Vec<i64>,
    times_s: Vec<f64>,
    measured_a: Vec<f64>,
    reference_a: Vec<Option<f64>>,
    calibrator_cold_junction_temperature_k: Option<f64>,
    board_cold_junction_temperature_k: Vec<f64>,
    board_cold_junction_offset_k: Option<f64>,
    thermocouple_voltage_v: Vec<f64>,
}

#[derive(Debug, Clone)]
struct ManualReferenceWindow {
    reference_a: f64,
    start_time_s: f64,
    stop_time_s: f64,
}

#[derive(Debug)]
struct ErrorSample {
    time_s: f64,
    reference_a: f64,
    measured_a: f64,
    error_a: f64,
    thermocouple_voltage_v: Option<f64>,
    thermocouple_cold_junction_temperature_k: Option<f64>,
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
    measured_x: Vec<f64>,
    expected_y: Vec<f64>,
    residuals: Vec<f64>,
    temperature_error_fit: Option<NormalizedLinearFit>,
}

#[derive(Debug, Clone, Serialize)]
struct NormalizedLinearFit {
    coefficients: Vec<f64>,
    r2: f64,
    x_center: f64,
    x_scale: f64,
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
    #[serde(default)]
    reference_a: Option<f64>,
    #[serde(default)]
    calibrator_cold_junction_temperature_k: Option<f64>,
    #[serde(default)]
    board_cold_junction_temperature_k: Option<f64>,
    #[serde(default)]
    board_cold_junction_offset_k: Option<f64>,
    #[serde(default)]
    thermocouple_voltage_v: Option<f64>,
}

#[derive(Debug, Serialize)]
struct CalibrationJsonRecord {
    #[serde(flatten)]
    core: CalibrationJsonCore,
}

#[derive(Debug, Serialize)]
struct ThermocoupleCalibrationJsonRecord {
    #[serde(flatten)]
    core: CalibrationJsonCore,
    #[serde(flatten)]
    thermocouple_cold_junction: ThermocoupleColdJunctionJsonRecord,
}

#[derive(Debug, Serialize)]
struct CalibrationJsonCore {
    model: String,
    serial_number: u64,
    channel: String,
    signal_name: String,
    datestamp_utc: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    reference_resistor_ohm: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    rtd_reference_current_a: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    rtd_frontend_gain: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature_error_fit: Option<NormalizedLinearFit>,
    polynomial_order: usize,
    polynomial_coefficients: Vec<f64>,
    r2: f64,
    input_units: String,
    output_units: String,
    accepted_sample_count: usize,
    detected_step_count: usize,
}

#[derive(Debug, Serialize)]
struct ThermocoupleColdJunctionJsonRecord {
    calibrator_cold_junction_temperature_k: f64,
    cold_junction_temperature_offset_k: f64,
    cold_junction_rtd_voltage_offset_v: f64,
    board_cold_junction_offset_k: f64,
}

#[derive(Debug, Serialize)]
struct CalibrationSummary {
    model: String,
    serial_number: u64,
    datestamp_utc: String,
    records: Vec<CalibrationSummaryRecord>,
}

#[derive(Debug, Serialize)]
struct CalibrationSummaryRecord {
    source: String,
    channel_label: String,
    channel: String,
    signal_name: String,
    detected_step_count: usize,
    accepted_sample_count: usize,
    error_units: String,
    rmse: f64,
    mean_error: f64,
    max_abs_error: f64,
    polynomial_coefficients: Vec<f64>,
    r2: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature_error_fit: Option<NormalizedLinearFit>,
    input_units: String,
    output_units: String,
    samples_path: String,
    plot_light_path: String,
    plot_dark_path: String,
    calibration_record_path: String,
}

struct PlotPaths {
    light: PathBuf,
    dark: PathBuf,
}

#[derive(Clone, Copy)]
enum PlotTheme {
    Light,
    Dark,
}

impl PlotTheme {
    fn file_suffix(self) -> &'static str {
        match self {
            Self::Light => "light",
            Self::Dark => "dark",
        }
    }

    fn page_background(self) -> &'static str {
        match self {
            Self::Light => {
                "radial-gradient(circle at top left, rgba(130, 50, 186, 0.16), transparent 32%), linear-gradient(180deg, #ffffff 0%, #f6f7fb 100%)"
            }
            Self::Dark => {
                "radial-gradient(circle at top left, rgba(172, 55, 255, 0.24), transparent 30%), linear-gradient(180deg, #242833 0%, #1e2129 100%)"
            }
        }
    }

    fn plot_background(self) -> &'static str {
        match self {
            Self::Light => "#ffffff",
            Self::Dark => "#2b2f3a",
        }
    }

    fn text_color(self) -> &'static str {
        match self {
            Self::Light => "#171922",
            Self::Dark => "#f2f0f6",
        }
    }

    fn grid_color(self) -> &'static str {
        match self {
            Self::Light => "#d8deea",
            Self::Dark => "#47404f",
        }
    }

    fn trace_color(self) -> &'static str {
        match self {
            Self::Light => "#000000",
            Self::Dark => "#f2f0f6",
        }
    }

    fn box_fill_color(self) -> &'static str {
        match self {
            Self::Light => "rgba(0, 0, 0, 0.08)",
            Self::Dark => "rgba(242, 240, 246, 0.10)",
        }
    }

    fn accepted_fill_color(self) -> &'static str {
        match self {
            Self::Light => "rgba(130, 50, 186, 0.11)",
            Self::Dark => "rgba(172, 55, 255, 0.18)",
        }
    }

    fn accuracy_fill_color(self) -> &'static str {
        match self {
            Self::Light => "rgba(130, 50, 186, 0.14)",
            Self::Dark => "rgba(172, 55, 255, 0.20)",
        }
    }
}

enum PromptDecision {
    Start {
        calibrator_cold_junction_temperature_k: Option<f64>,
    },
    Skip,
}

pub fn run_procedure(
    sn: u64,
    collect: bool,
    process: bool,
    dst: impl AsRef<Path>,
) -> Result<(), String> {
    let dst = dst.as_ref();

    match (collect, process) {
        (true, true) => {
            create_dir_all(dst).map_err(|e| format!("Failed to create {}: {e}", dst.display()))?;
            let paths = collect_all_channels(sn, dst, true)?;
            println!(
                "Collected and processed {} calibration data files.",
                paths.len()
            );
        }
        (true, false) => {
            create_dir_all(dst).map_err(|e| format!("Failed to create {}: {e}", dst.display()))?;
            let paths = collect_all_channels(sn, dst, false)?;
            println!("Collected {} calibration data files.", paths.len());
            for path in paths {
                println!("  {}", path.display());
            }
        }
        (false, true) => {
            let paths = discover_calibration_data(dst)?;
            process_calibration_files(sn, &paths, summary_dir_for_process_path(dst)?)?;
        }
        (false, false) => {
            return Err("At least one of collect or process must be true.".to_owned());
        }
    }

    Ok(())
}

fn collect_all_channels(
    sn: u64,
    output_root: &Path,
    process_after_each_run: bool,
) -> Result<Vec<PathBuf>, String> {
    println!("Rev7 calibration collection");
    println!(
        "4-20 mA channels use a Fluke 707 auto-step through 0, 5, 10, 15, and 20 mA during the recording. Each current channel records for {CAPTURE_SECONDS} s at {RATE_HZ} Hz."
    );
    println!(
        "RTD channels use manual holds stepping up from -200 C to +700 C, then back down from +700 C to -200 C during the recording. Each RTD channel records for {RTD_CAPTURE_SECONDS} s at {RATE_HZ} Hz."
    );
    println!(
        "Thermocouple channels use a VA710 simulator with manual holds stepping from -200 C to +1250 C and back down to -200 C during the recording. Target 50 K increments below 100 C and 100 K increments above 100 C. Enter the VA710 cold-junction temperature before each thermocouple run; each thermocouple channel records for {TC_CAPTURE_SECONDS} s at {RATE_HZ} Hz."
    );
    println!(
        "Voltage channels use a signal generator measured by the Fluke 707. For each voltage channel, the procedure prompts for {VOLTAGE_HOLD_COUNT} evenly spaced target holds; enter the Fluke voltage once the signal is stable."
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
        let PromptDecision::Start {
            calibrator_cold_junction_temperature_k,
        } = prompt_for_channel(channel)?
        else {
            println!("Skipped {}.", channel.label);
            continue;
        };
        let capture = run_channel_capture(
            sn,
            output_root,
            channel,
            calibrator_cold_junction_temperature_k,
        )?;
        let summary_dir = capture.op_dir.clone();
        let path = write_calibration_data(&capture)?;
        println!("Saved {}", path.display());
        if process_after_each_run {
            process_calibration_files(sn, std::slice::from_ref(&path), &summary_dir)?;
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
                "Ready the 0/5/10/15/20 mA auto-step sequence. Press Enter to start recording, then start the stepping sequence during the {} second run.",
                channel.capture_seconds(),
            );
        }
        CalibrationKind::Rtd => {
            println!(
                "Ready the RTD calibrator at -200 C. Press Enter to start recording, then manually step up to +700 C and back down to -200 C during the {} second run with about 5 s holds every 100 K.",
                channel.capture_seconds(),
            );
        }
        CalibrationKind::Thermocouple => {
            println!(
                "Ready the VA710 thermocouple simulator at -200 C. During the {} second run, manually step up to +1250 C and back down to -200 C with stable holds, targeting 50 K increments below 100 C and 100 K increments above 100 C.",
                channel.capture_seconds(),
            );
            println!(
                "Enter the VA710 cold-junction temperature in degC, or type s then Enter to skip this run."
            );
            let line = read_operator_line()?;
            if line.trim().eq_ignore_ascii_case("s") {
                return Ok(PromptDecision::Skip);
            }
            let calibrator_cold_junction_temperature_c = line.trim().parse::<f64>().map_err(|e| {
                format!(
                    "Expected VA710 cold-junction temperature in degC or 's' to skip, got '{}': {e}",
                    line.trim()
                )
            })?;
            println!("Press Enter to start this run, or type s then Enter to skip it.");
            let line = read_operator_line()?;
            if line.trim().eq_ignore_ascii_case("s") {
                return Ok(PromptDecision::Skip);
            }
            return Ok(PromptDecision::Start {
                calibrator_cold_junction_temperature_k: Some(
                    ZERO_C_K + calibrator_cold_junction_temperature_c,
                ),
            });
        }
        CalibrationKind::Voltage => {
            let (min_v, max_v) = channel
                .voltage_reference_range_v()
                .ok_or_else(|| format!("Missing voltage reference range for {}", channel.label))?;
            println!(
                "Connect the signal generator to this input and measure it with the Fluke 707. This run will prompt for {VOLTAGE_HOLD_COUNT} target holds from {min_v:.6} V to {max_v:.6} V."
            );
        }
    }
    println!("Press Enter to start this run, or type s then Enter to skip it.");
    let line = read_operator_line()?;

    if line.trim().eq_ignore_ascii_case("s") {
        Ok(PromptDecision::Skip)
    } else {
        Ok(PromptDecision::Start {
            calibrator_cold_junction_temperature_k: None,
        })
    }
}

fn read_operator_line() -> Result<String, String> {
    let mut line = String::new();
    stdin()
        .read_line(&mut line)
        .map_err(|e| format!("Failed to read operator prompt: {e}"))?;
    Ok(line)
}

fn run_channel_capture(
    sn: u64,
    output_root: &Path,
    channel: CalibrationChannel,
    calibrator_cold_junction_temperature_k: Option<f64>,
) -> Result<ChannelCapture, String> {
    let op_name = op_name_for_channel(sn, channel)?;
    let op_dir = output_root.join(&op_name);
    create_dir_all(&op_dir).map_err(|e| format!("Failed to create {}: {e}", op_dir.display()))?;

    let mut controller =
        Controller::new(controller_context(op_name.clone(), op_dir.clone(), channel));
    controller.add_peripheral(
        PERIPHERAL_NAME,
        Box::new(DeimosDaqRev7 { serial_number: sn }),
    )?;

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

    let mut run_handle = controller.run_nonblocking(None, None, true)?;
    let run_start = Instant::now();
    let manual_reference_windows = if matches!(channel.kind, CalibrationKind::Voltage) {
        collect_voltage_reference_windows(channel, run_start)?
    } else {
        println!(
            "Recording {} for up to {} seconds. Perform the calibration stepping now. Press Enter to stop this run early.",
            channel.label,
            channel.capture_seconds(),
        );
        let mut line = String::new();
        stdin()
            .read_line(&mut line)
            .map_err(|e| format!("Failed to read early-stop prompt: {e}"))?;
        Vec::new()
    };
    if run_handle.is_running() {
        run_handle.stop();
    }
    let stop_reason = run_handle.join()?;
    println!("Stopped {}: {stop_reason}", channel.label);

    let dropped = dropped_frames.load(Ordering::Relaxed);
    if dropped != 0 {
        println!(
            "ReportingDispatcher dropped_frames for {}: {dropped}",
            channel.label
        );
    }

    extract_capture(
        sn,
        channel,
        op_name,
        op_dir,
        df_handle,
        calibrator_cold_junction_temperature_k,
        manual_reference_windows,
    )
}

fn collect_voltage_reference_windows(
    channel: CalibrationChannel,
    run_start: Instant,
) -> Result<Vec<ManualReferenceWindow>, String> {
    let (min_v, max_v) = channel
        .voltage_reference_range_v()
        .ok_or_else(|| format!("Missing voltage reference range for {}", channel.label))?;
    let mut windows = Vec::with_capacity(VOLTAGE_HOLD_COUNT);

    for hold_idx in 0..VOLTAGE_HOLD_COUNT {
        let frac = hold_idx as f64 / (VOLTAGE_HOLD_COUNT.saturating_sub(1) as f64);
        let target_v = min_v + frac * (max_v - min_v);
        println!(
            "Hold {}/{} for {}: tune the signal generator to about {:.6} V. When stable, enter the Fluke 707 measured voltage in V and press Enter.",
            hold_idx + 1,
            VOLTAGE_HOLD_COUNT,
            channel.label,
            target_v,
        );
        let line = read_operator_line()?;
        let reference_a = line.trim().parse::<f64>().map_err(|e| {
            format!(
                "Expected Fluke voltage in V for {}, got '{}': {e}",
                channel.label,
                line.trim()
            )
        })?;
        let start_time_s = run_start.elapsed().as_secs_f64();
        println!(
            "Recording hold {}/{} for {:.1} s at reference {:.9} V.",
            hold_idx + 1,
            VOLTAGE_HOLD_COUNT,
            VOLTAGE_HOLD_SECONDS,
            reference_a,
        );
        sleep(Duration::from_secs_f64(VOLTAGE_HOLD_SECONDS));
        let stop_time_s = run_start.elapsed().as_secs_f64();
        windows.push(ManualReferenceWindow {
            reference_a,
            start_time_s,
            stop_time_s,
        });
    }

    Ok(windows)
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
    serial_number: u64,
    channel: CalibrationChannel,
    op_name: String,
    op_dir: PathBuf,
    df_handle: DataFrameHandle,
    calibrator_cold_junction_temperature_k: Option<f64>,
    manual_reference_windows: Vec<ManualReferenceWindow>,
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
    let thermocouple_voltage = match channel.tc_voltage_signal_name() {
        Some(signal_name) => Some(columns.get(signal_name).ok_or_else(|| {
            let mut available = columns.keys().cloned().collect::<Vec<_>>();
            available.sort();
            format!("Signal '{signal_name}' was not found in dataframe columns: {available:?}")
        })?),
        None => None,
    };
    let board_cold_junction_temperature = if matches!(channel.kind, CalibrationKind::Thermocouple) {
        Some(columns.get(BOARD_COLD_JUNCTION_SIGNAL_NAME).ok_or_else(|| {
                let mut available = columns.keys().cloned().collect::<Vec<_>>();
                available.sort();
                format!(
                    "Signal '{BOARD_COLD_JUNCTION_SIGNAL_NAME}' was not found in dataframe columns: {available:?}"
                )
            })?)
    } else {
        None
    };

    let first_timestamp = *timestamps
        .first()
        .ok_or_else(|| format!("No samples were captured for {}", channel.label))?;

    let mut timestamps_ns = Vec::with_capacity(timestamps.len());
    let mut times_s = Vec::with_capacity(timestamps.len());
    let mut measured_a = Vec::with_capacity(timestamps.len());
    let mut reference_a = Vec::with_capacity(timestamps.len());
    let mut board_cold_junction_temperature_k = Vec::with_capacity(timestamps.len());
    let mut thermocouple_voltage_v = Vec::with_capacity(timestamps.len());
    let mut last_time_s = None;

    for (idx, (&timestamp, &measurement)) in timestamps.iter().zip(measurements.iter()).enumerate()
    {
        let time_s = (timestamp - first_timestamp) as f64 * 1e-9;
        let board_cold_junction_k = board_cold_junction_temperature.map(|column| column[idx]);
        let thermocouple_voltage_v_sample = thermocouple_voltage.map(|column| column[idx]);

        if !time_s.is_finite() || !measurement.is_finite() {
            continue;
        }
        if board_cold_junction_k.is_some_and(|value| !value.is_finite())
            || thermocouple_voltage_v_sample.is_some_and(|value| !value.is_finite())
        {
            continue;
        }
        if last_time_s.is_some_and(|last| time_s <= last) {
            continue;
        }

        timestamps_ns.push(timestamp);
        times_s.push(time_s);
        measured_a.push(measurement);
        reference_a.push(
            manual_reference_windows
                .iter()
                .find(|window| time_s >= window.start_time_s && time_s <= window.stop_time_s)
                .map(|window| window.reference_a),
        );
        if let Some(board_cold_junction_k) = board_cold_junction_k {
            board_cold_junction_temperature_k.push(board_cold_junction_k);
        }
        if let Some(thermocouple_voltage_v_sample) = thermocouple_voltage_v_sample {
            thermocouple_voltage_v.push(thermocouple_voltage_v_sample);
        }
        last_time_s = Some(time_s);
    }

    if times_s.len() < 2 {
        return Err(format!(
            "Need at least two finite monotonic samples for {}, got {}",
            channel.label,
            times_s.len()
        ));
    }
    let board_cold_junction_offset_k = if matches!(channel.kind, CalibrationKind::Thermocouple) {
        let calibrator_cold_junction_temperature_k = calibrator_cold_junction_temperature_k
            .ok_or_else(|| {
                format!(
                    "Missing VA710 cold-junction temperature for {}",
                    channel.label
                )
            })?;
        if board_cold_junction_temperature_k.len() != measured_a.len()
            || thermocouple_voltage_v.len() != measured_a.len()
        {
            return Err(format!(
                "Thermocouple capture for {} is missing voltage or board cold-junction samples",
                channel.label
            ));
        }
        let mean_board_cold_junction_temperature_k =
            board_cold_junction_temperature_k.iter().sum::<f64>()
                / board_cold_junction_temperature_k.len() as f64;
        let offset_k =
            calibrator_cold_junction_temperature_k - mean_board_cold_junction_temperature_k;
        measured_a = thermocouple_voltage_v
            .iter()
            .zip(board_cold_junction_temperature_k.iter())
            .map(|(&voltage_v, &board_temperature_k)| {
                ktype_corrected_temp_k(voltage_v, board_temperature_k + offset_k)
            })
            .collect::<Vec<_>>();
        Some(offset_k)
    } else {
        None
    };

    Ok(ChannelCapture {
        serial_number,
        channel,
        op_name,
        op_dir,
        timestamps_ns,
        times_s,
        measured_a,
        reference_a,
        calibrator_cold_junction_temperature_k,
        board_cold_junction_temperature_k,
        board_cold_junction_offset_k,
        thermocouple_voltage_v,
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

    for (idx, ((&timestamp_ns, &time_s), &measured_a)) in capture
        .timestamps_ns
        .iter()
        .zip(capture.times_s.iter())
        .zip(capture.measured_a.iter())
        .enumerate()
    {
        writer
            .serialize(CalibrationRecord {
                model: MODEL_NAME.to_owned(),
                serial_number: capture.serial_number,
                channel: capture.channel.slug.to_owned(),
                signal_name: capture.channel.signal_name.to_owned(),
                timestamp_ns,
                time_s,
                measured_a,
                reference_a: capture.reference_a.get(idx).copied().flatten(),
                calibrator_cold_junction_temperature_k: capture
                    .calibrator_cold_junction_temperature_k,
                board_cold_junction_temperature_k: capture
                    .board_cold_junction_temperature_k
                    .get(idx)
                    .copied(),
                board_cold_junction_offset_k: capture.board_cold_junction_offset_k,
                thermocouple_voltage_v: capture.thermocouple_voltage_v.get(idx).copied(),
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

fn process_calibration_files(sn: u64, paths: &[PathBuf], summary_dir: &Path) -> Result<(), String> {
    create_dir_all(summary_dir)
        .map_err(|e| format!("Failed to create {}: {e}", summary_dir.display()))?;
    let datestamp_utc = utc_datestamp();
    let summary_path = summary_dir.join(format!("rev7_calibration_summary_{datestamp_utc}.json"));
    let mut summary_records = Vec::new();

    for path in paths {
        let capture = read_calibration_data(path)?;
        if capture.serial_number != sn {
            return Err(format!(
                "{} has serial number {}, expected {sn}",
                path.display(),
                capture.serial_number
            ));
        }
        let analysis = analyze_capture(&capture)?;
        let samples_path = write_error_samples(&capture, &analysis)?;
        let plot_paths = write_analysis_plots(&capture, &analysis)?;
        let record_path = write_calibration_json_record(&capture, &analysis)?;

        let (error_scale, error_units) = match capture.channel.kind {
            CalibrationKind::Current4To20 => (1e6, "uA"),
            CalibrationKind::Rtd => (1.0, "K"),
            CalibrationKind::Thermocouple => (1.0, "K"),
            CalibrationKind::Voltage => (1.0, "V"),
        };
        summary_records.push(CalibrationSummaryRecord {
            source: path.display().to_string(),
            channel_label: capture.channel.label.to_owned(),
            channel: capture.channel.analog_input_name.to_owned(),
            signal_name: capture.channel.calibration_signal_name().to_owned(),
            detected_step_count: analysis.segments.len(),
            accepted_sample_count: analysis.samples.len(),
            error_units: error_units.to_owned(),
            rmse: analysis.rmse_a * error_scale,
            mean_error: analysis.mean_error_a * error_scale,
            max_abs_error: analysis.max_abs_error_a * error_scale,
            polynomial_coefficients: analysis.voltage_fit.coefficients.clone(),
            r2: analysis.voltage_fit.r2,
            temperature_error_fit: analysis.voltage_fit.temperature_error_fit.clone(),
            input_units: capture.channel.fit_units().to_owned(),
            output_units: capture.channel.fit_units().to_owned(),
            samples_path: samples_path.display().to_string(),
            plot_light_path: plot_paths.light.display().to_string(),
            plot_dark_path: plot_paths.dark.display().to_string(),
            calibration_record_path: record_path.display().to_string(),
        });
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
        println!("  wrote {}", plot_paths.light.display());
        println!("  wrote {}", plot_paths.dark.display());
        println!("  wrote {}", record_path.display());
    }

    let summary = CalibrationSummary {
        model: MODEL_NAME.to_owned(),
        serial_number: sn,
        datestamp_utc,
        records: summary_records,
    };
    let json = serde_json::to_string_pretty(&summary)
        .map_err(|e| format!("Failed to serialize calibration summary: {e}"))?;
    fs::write(&summary_path, json)
        .map_err(|e| format!("Failed to write {}: {e}", summary_path.display()))?;

    println!("Summary written to {}", summary_path.display());
    println!(
        "Calibration polynomial records and light/dark plots were written next to each source file."
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
    let serial_number = first.serial_number;
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
    let mut reference_a = Vec::with_capacity(records.len());
    let mut board_cold_junction_temperature_k = Vec::with_capacity(records.len());
    let mut thermocouple_voltage_v = Vec::with_capacity(records.len());
    let mut calibrator_cold_junction_temperature_k = None;
    let mut board_cold_junction_offset_k = None;
    let mut last_time_s = None;

    for record in records {
        if record.model != MODEL_NAME {
            return Err(format!(
                "{} has model '{}', expected '{MODEL_NAME}'",
                path.display(),
                record.model
            ));
        }
        if record.serial_number != serial_number {
            return Err(format!(
                "{} contains mixed serial numbers: {} and {}",
                path.display(),
                serial_number,
                record.serial_number
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
        if !record.time_s.is_finite() {
            continue;
        }
        if last_time_s.is_some_and(|last| record.time_s <= last) {
            continue;
        }

        let measurement = if matches!(channel.kind, CalibrationKind::Thermocouple) {
            calibrator_cold_junction_temperature_k = calibrator_cold_junction_temperature_k
                .or(record.calibrator_cold_junction_temperature_k);
            let offset_k = record
                .board_cold_junction_offset_k
                .or(board_cold_junction_offset_k)
                .ok_or_else(|| {
                    format!(
                        "{} is missing board_cold_junction_offset_k for thermocouple signal '{}'",
                        path.display(),
                        channel.signal_name
                    )
                })?;
            board_cold_junction_offset_k = Some(offset_k);
            let board_temperature_k = record.board_cold_junction_temperature_k.ok_or_else(|| {
                format!(
                    "{} is missing board_cold_junction_temperature_k for thermocouple signal '{}'",
                    path.display(),
                    channel.signal_name
                )
            })?;
            let voltage_v = record.thermocouple_voltage_v.ok_or_else(|| {
                format!(
                    "{} is missing thermocouple_voltage_v for thermocouple signal '{}'",
                    path.display(),
                    channel.signal_name
                )
            })?;
            if !board_temperature_k.is_finite() || !voltage_v.is_finite() {
                continue;
            }
            let measurement = ktype_corrected_temp_k(voltage_v, board_temperature_k + offset_k);
            if !measurement.is_finite() {
                continue;
            }
            board_cold_junction_temperature_k.push(board_temperature_k);
            thermocouple_voltage_v.push(voltage_v);
            measurement
        } else {
            if !record.measured_a.is_finite() {
                continue;
            }
            record.measured_a
        };

        timestamps_ns.push(record.timestamp_ns);
        times_s.push(record.time_s);
        measured_a.push(measurement);
        reference_a.push(record.reference_a);
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
        serial_number,
        channel,
        op_name,
        op_dir,
        timestamps_ns,
        times_s,
        measured_a,
        reference_a,
        calibrator_cold_junction_temperature_k,
        board_cold_junction_temperature_k,
        board_cold_junction_offset_k,
        thermocouple_voltage_v,
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
    let voltage_fit = fit_expected_value(&samples, &segments, capture.channel.kind)?;

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
    segments: &[StepSegment],
    kind: CalibrationKind,
) -> Result<VoltageFit, String> {
    match kind {
        CalibrationKind::Current4To20 => fit_current_voltage(samples),
        CalibrationKind::Rtd => fit_rtd_voltage_from_temperature_error(samples),
        CalibrationKind::Thermocouple => fit_thermocouple_voltage(samples),
        CalibrationKind::Voltage => fit_voltage(samples, segments),
    }
}

fn fit_current_voltage(samples: &[ErrorSample]) -> Result<VoltageFit, String> {
    let points = samples
        .iter()
        .map(|sample| {
            (
                measured_voltage_v(sample),
                expected_voltage_v(sample.reference_a),
            )
        })
        .collect::<Vec<_>>();
    let (coefficients, r2, residuals) = fit_linear_points(&points)?;

    Ok(VoltageFit {
        coefficients,
        r2,
        measured_x: points.iter().map(|(x, _)| *x).collect(),
        expected_y: points.iter().map(|(_, y)| *y).collect(),
        residuals,
        temperature_error_fit: None,
    })
}

fn fit_voltage(samples: &[ErrorSample], segments: &[StepSegment]) -> Result<VoltageFit, String> {
    let points = samples
        .iter()
        .map(|sample| (sample.measured_a, sample.reference_a))
        .collect::<Vec<_>>();

    if voltage_hold_means_within_calibrator_accuracy(samples, segments) {
        return Ok(identity_voltage_fit(&points));
    }

    let (coefficients, r2, residuals) = fit_linear_points(&points)?;

    Ok(VoltageFit {
        coefficients,
        r2,
        measured_x: points.iter().map(|(x, _)| *x).collect(),
        expected_y: points.iter().map(|(_, y)| *y).collect(),
        residuals,
        temperature_error_fit: None,
    })
}

fn voltage_hold_means_within_calibrator_accuracy(
    samples: &[ErrorSample],
    segments: &[StepSegment],
) -> bool {
    let mut sample_offset = 0;

    for segment in segments {
        let sample_count = segment.accept_end_idx - segment.accept_start_idx;
        let Some(hold_samples) = samples.get(sample_offset..sample_offset + sample_count) else {
            return false;
        };
        sample_offset += sample_count;

        if hold_samples.is_empty() {
            return false;
        }

        let mean_measured_v = hold_samples
            .iter()
            .map(|sample| sample.measured_a)
            .sum::<f64>()
            / hold_samples.len() as f64;
        let accuracy_v = fluke_707_voltage_accuracy_v(segment.reference_a);

        if (mean_measured_v - segment.reference_a).abs() > accuracy_v {
            return false;
        }
    }

    sample_offset == samples.len()
}

fn identity_voltage_fit(points: &[(f64, f64)]) -> VoltageFit {
    let residuals = points
        .iter()
        .map(|(measured_v, reference_v)| reference_v - measured_v)
        .collect::<Vec<_>>();
    let r2 = r2_for_residuals(
        points.iter().map(|(_, reference_v)| *reference_v),
        &residuals,
    );

    VoltageFit {
        coefficients: vec![0.0, 1.0],
        r2,
        measured_x: points.iter().map(|(x, _)| *x).collect(),
        expected_y: points.iter().map(|(_, y)| *y).collect(),
        residuals,
        temperature_error_fit: None,
    }
}

fn r2_for_residuals(expected_values: impl Iterator<Item = f64>, residuals: &[f64]) -> f64 {
    let expected = expected_values.collect::<Vec<_>>();
    let mean_expected = expected.iter().sum::<f64>() / expected.len() as f64;
    let ss_residual = residuals
        .iter()
        .map(|residual| residual * residual)
        .sum::<f64>();
    let ss_total = expected
        .iter()
        .map(|expected| {
            let delta = expected - mean_expected;
            delta * delta
        })
        .sum::<f64>();

    if ss_total > 0.0 {
        1.0 - ss_residual / ss_total
    } else if ss_residual == 0.0 {
        1.0
    } else {
        0.0
    }
}

fn fit_rtd_voltage_from_temperature_error(samples: &[ErrorSample]) -> Result<VoltageFit, String> {
    let temperature_error_fit = fit_normalized_temperature_error(samples)?;
    let points = samples
        .iter()
        .map(|sample| {
            let measured_temperature_k = sample.measured_a;
            let fitted_error_k = normalized_fit_value(
                measured_temperature_k,
                &temperature_error_fit.coefficients,
                temperature_error_fit.x_center,
                temperature_error_fit.x_scale,
            );
            let corrected_temperature_k = measured_temperature_k + fitted_error_k;
            (
                rtd_voltage_from_temperature_k(measured_temperature_k),
                rtd_voltage_from_temperature_k(corrected_temperature_k),
            )
        })
        .collect::<Vec<_>>();
    let (coefficients, r2, _) = fit_linear_points(&points)?;

    let residuals = samples
        .iter()
        .zip(points.iter())
        .map(|(sample, (measured_voltage_v, _))| {
            let corrected_voltage_v = polyval(*measured_voltage_v, &coefficients);
            let corrected_temperature_k = rtd_temperature_from_voltage_v(corrected_voltage_v);
            sample.reference_a - corrected_temperature_k
        })
        .collect::<Vec<_>>();

    Ok(VoltageFit {
        coefficients,
        r2,
        measured_x: points.iter().map(|(x, _)| *x).collect(),
        expected_y: points.iter().map(|(_, y)| *y).collect(),
        residuals,
        temperature_error_fit: Some(temperature_error_fit),
    })
}

fn fit_thermocouple_voltage(samples: &[ErrorSample]) -> Result<VoltageFit, String> {
    let points = samples
        .iter()
        .map(|sample| {
            let measured_voltage_v = sample.thermocouple_voltage_v.ok_or_else(|| {
                "Thermocouple voltage fit requires thermocouple_voltage_v samples".to_owned()
            })?;
            let cold_junction_temperature_k = sample
                .thermocouple_cold_junction_temperature_k
                .ok_or_else(|| {
                    "Thermocouple voltage fit requires cold-junction temperature samples".to_owned()
                })?;
            let expected_voltage_v =
                ktype_voltage_v(sample.reference_a) - ktype_voltage_v(cold_junction_temperature_k);
            Ok((measured_voltage_v, expected_voltage_v))
        })
        .collect::<Result<Vec<_>, String>>()?;
    let min_x = points.iter().map(|(x, _)| *x).fold(f64::INFINITY, f64::min);
    let max_x = points
        .iter()
        .map(|(x, _)| *x)
        .fold(f64::NEG_INFINITY, f64::max);
    if (max_x - min_x).abs() <= f64::EPSILON {
        return Err(
            "Thermocouple voltage fit requires variation in measured thermocouple voltage"
                .to_owned(),
        );
    }

    let (coefficients, r2, _) = fit_linear_points(&points)?;
    let residuals = samples
        .iter()
        .map(|sample| {
            let measured_voltage_v = sample.thermocouple_voltage_v.ok_or_else(|| {
                "Thermocouple voltage fit requires thermocouple_voltage_v samples".to_owned()
            })?;
            let cold_junction_temperature_k = sample
                .thermocouple_cold_junction_temperature_k
                .ok_or_else(|| {
                    "Thermocouple voltage fit requires cold-junction temperature samples".to_owned()
                })?;
            let corrected_voltage_v = polyval(measured_voltage_v, &coefficients);
            let corrected_temperature_k =
                ktype_corrected_temp_k(corrected_voltage_v, cold_junction_temperature_k);
            Ok(sample.reference_a - corrected_temperature_k)
        })
        .collect::<Result<Vec<_>, String>>()?;

    Ok(VoltageFit {
        coefficients,
        r2,
        measured_x: points.iter().map(|(x, _)| *x).collect(),
        expected_y: points.iter().map(|(_, y)| *y).collect(),
        residuals,
        temperature_error_fit: None,
    })
}

fn fit_normalized_temperature_error(
    samples: &[ErrorSample],
) -> Result<NormalizedLinearFit, String> {
    let x_center =
        samples.iter().map(|sample| sample.measured_a).sum::<f64>() / samples.len() as f64;
    let x_scale = samples
        .iter()
        .map(|sample| (sample.measured_a - x_center).abs())
        .fold(0.0, f64::max)
        .max(1.0);
    let points = samples
        .iter()
        .map(|sample| {
            (
                normalized_x(sample.measured_a, x_center, x_scale),
                sample.error_a,
            )
        })
        .collect::<Vec<_>>();
    let (coefficients, r2, _) = fit_linear_points(&points)?;

    Ok(NormalizedLinearFit {
        coefficients,
        r2,
        x_center,
        x_scale,
    })
}

fn fit_linear_points(points: &[(f64, f64)]) -> Result<(Vec<f64>, f64, Vec<f64>), String> {
    let coefficients = polyfit(points, VOLTAGE_FIT_ORDER)?;
    let expected_mean = points.iter().map(|(_, y)| *y).sum::<f64>() / points.len() as f64;
    let mut ss_res = 0.0;
    let mut ss_tot = 0.0;
    let mut residuals = Vec::with_capacity(points.len());
    for &(measured, expected) in points {
        let fitted = polyval(measured, &coefficients);
        let residual = expected - fitted;
        residuals.push(residual);
        ss_res += residual * residual;
        let centered_expected = expected - expected_mean;
        ss_tot += centered_expected * centered_expected;
    }
    let r2 = if ss_tot > 0.0 {
        1.0 - ss_res / ss_tot
    } else if ss_res == 0.0 {
        1.0
    } else {
        0.0
    };

    Ok((coefficients, r2, residuals))
}

fn normalized_x(x: f64, center: f64, scale: f64) -> f64 {
    (x - center) / scale
}

fn normalized_fit_value(x: f64, coefficients: &[f64], center: f64, scale: f64) -> f64 {
    polyval(normalized_x(x, center, scale), coefficients)
}

fn detect_step_segments(capture: &ChannelCapture) -> Result<Vec<StepSegment>, String> {
    if matches!(capture.channel.kind, CalibrationKind::Voltage) {
        return manual_reference_segments(capture);
    }

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
        CalibrationKind::Thermocouple => nearest_thermocouple_reference_k(measured),
        CalibrationKind::Voltage => None,
    }
}

fn manual_reference_segments(capture: &ChannelCapture) -> Result<Vec<StepSegment>, String> {
    let mut segments = Vec::new();
    let mut active_reference = None;
    let mut active_start_idx = 0;

    for (idx, &reference) in capture.reference_a.iter().enumerate() {
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
            capture.reference_a.len(),
            &mut segments,
        );
    }

    if segments.is_empty() {
        return Err(no_segments_error(capture));
    }

    Ok(segments)
}

fn nearest_rtd_reference_k(measured_k: f64) -> Option<f64> {
    if !(RTD_MIN_REFERENCE_K..=RTD_MAX_REFERENCE_K).contains(&measured_k) {
        return None;
    }

    let reference_k = ZERO_C_K + ((measured_k - ZERO_C_K) / RTD_STEP_K).round() * RTD_STEP_K;

    if (RTD_MIN_REFERENCE_K..=RTD_MAX_REFERENCE_K).contains(&reference_k)
        && (measured_k - reference_k).abs() <= RTD_STEP_DETECTION_TOLERANCE_K
    {
        Some(reference_k)
    } else {
        None
    }
}

fn nearest_thermocouple_reference_k(measured_k: f64) -> Option<f64> {
    if !(TC_MIN_REFERENCE_K..=TC_MAX_REFERENCE_K).contains(&measured_k) {
        return None;
    }

    let reference_k = ZERO_C_K + ((measured_k - ZERO_C_K) / TC_STEP_K).round() * TC_STEP_K;

    if (TC_MIN_REFERENCE_K..=TC_MAX_REFERENCE_K).contains(&reference_k)
        && (measured_k - reference_k).abs() <= TC_STEP_DETECTION_TOLERANCE_K
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
        CalibrationKind::Thermocouple => format!(
            "No stable thermocouple temperature holds found for {} using +/- {:.1} K tolerance and {:.1} s minimum duration",
            capture.channel.label, TC_STEP_DETECTION_TOLERANCE_K, MIN_TC_STEP_DURATION_S,
        ),
        CalibrationKind::Voltage => format!(
            "No manual voltage reference holds found for {}. The CSV must contain reference_a values from the voltage hold prompts.",
            capture.channel.label,
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
            let thermocouple_voltage_v = capture.thermocouple_voltage_v.get(idx).copied();
            let thermocouple_cold_junction_temperature_k = capture
                .board_cold_junction_temperature_k
                .get(idx)
                .copied()
                .zip(capture.board_cold_junction_offset_k)
                .map(|(board_temperature_k, offset_k)| board_temperature_k + offset_k);
            samples.push(ErrorSample {
                time_s: capture.times_s[idx],
                reference_a: segment.reference_a,
                measured_a,
                error_a: segment.reference_a - measured_a,
                thermocouple_voltage_v,
                thermocouple_cold_junction_temperature_k,
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

fn rtd_voltage_from_temperature_k(temperature_k: f64) -> f64 {
    pt100_resistance_ohm(temperature_k) * RTD_REFERENCE_CURRENT_A * RTD_FRONTEND_GAIN
}

fn rtd_temperature_from_voltage_v(voltage_v: f64) -> f64 {
    let resistance_ohm = voltage_v / (RTD_REFERENCE_CURRENT_A * RTD_FRONTEND_GAIN);
    pt100_temp_k(resistance_ohm)
}

fn fluke_707_voltage_accuracy_v(reading_v: f64) -> f64 {
    FLUKE_707_VOLTAGE_ACCURACY_READING_FRAC * reading_v.abs() + FLUKE_707_VOLTAGE_ACCURACY_V
}

fn accuracy_band_xy(
    min_reference: f64,
    max_reference: f64,
    accuracy_limit: impl Fn(f64) -> f64,
) -> (Vec<f64>, Vec<f64>) {
    const POINTS: usize = 64;
    let count = if min_reference == max_reference {
        2
    } else {
        POINTS
    };
    let mut upper = Vec::with_capacity(count);
    for idx in 0..count {
        let frac = idx as f64 / (count - 1) as f64;
        let reference = min_reference + frac * (max_reference - min_reference);
        upper.push((reference, accuracy_limit(reference)));
    }

    let mut x = Vec::with_capacity(2 * count + 1);
    let mut y = Vec::with_capacity(2 * count + 1);
    for &(reference, limit) in &upper {
        x.push(reference);
        y.push(limit);
    }
    for &(reference, limit) in upper.iter().rev() {
        x.push(reference);
        y.push(-limit);
    }
    if let Some(&(reference, limit)) = upper.first() {
        x.push(reference);
        y.push(limit);
    }

    (x, y)
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

fn thermocouple_cold_junction_json_record(
    capture: &ChannelCapture,
) -> Result<ThermocoupleColdJunctionJsonRecord, String> {
    let calibrator_cold_junction_temperature_k = capture
        .calibrator_cold_junction_temperature_k
        .ok_or_else(|| {
            format!(
                "Thermocouple capture for {} is missing calibrator cold-junction temperature",
                capture.channel.label
            )
        })?;
    let offset_k = capture.board_cold_junction_offset_k.ok_or_else(|| {
        format!(
            "Thermocouple capture for {} is missing cold-junction temperature offset",
            capture.channel.label
        )
    })?;
    if capture.board_cold_junction_temperature_k.is_empty() {
        return Err(format!(
            "Thermocouple capture for {} is missing board cold-junction temperature samples",
            capture.channel.label
        ));
    }

    let mut offset_sum_v = 0.0;
    for &board_temperature_k in &capture.board_cold_junction_temperature_k {
        offset_sum_v += rtd_voltage_from_temperature_k(board_temperature_k + offset_k)
            - rtd_voltage_from_temperature_k(board_temperature_k);
    }

    Ok(ThermocoupleColdJunctionJsonRecord {
        calibrator_cold_junction_temperature_k,
        cold_junction_temperature_offset_k: offset_k,
        cold_junction_rtd_voltage_offset_v: offset_sum_v
            / capture.board_cold_junction_temperature_k.len() as f64,
        board_cold_junction_offset_k: offset_k,
    })
}

fn write_calibration_json_record(
    capture: &ChannelCapture,
    analysis: &ChannelAnalysis,
) -> Result<PathBuf, String> {
    let path = capture
        .op_dir
        .join(format!("{}_calibration.json", capture.op_name));
    let core = CalibrationJsonCore {
        model: MODEL_NAME.to_owned(),
        serial_number: capture.serial_number,
        channel: capture.channel.analog_input_name.to_owned(),
        signal_name: capture.channel.calibration_signal_name().to_owned(),
        datestamp_utc: utc_datestamp(),
        reference_resistor_ohm: match capture.channel.kind {
            CalibrationKind::Current4To20 => Some(REFERENCE_RESISTOR_OHM),
            CalibrationKind::Rtd => None,
            CalibrationKind::Thermocouple => None,
            CalibrationKind::Voltage => None,
        },
        rtd_reference_current_a: match capture.channel.kind {
            CalibrationKind::Current4To20 | CalibrationKind::Voltage => None,
            CalibrationKind::Rtd | CalibrationKind::Thermocouple => Some(RTD_REFERENCE_CURRENT_A),
        },
        rtd_frontend_gain: match capture.channel.kind {
            CalibrationKind::Current4To20 | CalibrationKind::Voltage => None,
            CalibrationKind::Rtd | CalibrationKind::Thermocouple => Some(RTD_FRONTEND_GAIN),
        },
        temperature_error_fit: analysis.voltage_fit.temperature_error_fit.clone(),
        polynomial_order: VOLTAGE_FIT_ORDER,
        polynomial_coefficients: analysis.voltage_fit.coefficients.clone(),
        r2: analysis.voltage_fit.r2,
        input_units: capture.channel.fit_units().to_owned(),
        output_units: capture.channel.fit_units().to_owned(),
        accepted_sample_count: analysis.samples.len(),
        detected_step_count: analysis.segments.len(),
    };

    let json = if matches!(capture.channel.kind, CalibrationKind::Thermocouple) {
        serde_json::to_string_pretty(&ThermocoupleCalibrationJsonRecord {
            core,
            thermocouple_cold_junction: thermocouple_cold_junction_json_record(capture)?,
        })
    } else {
        serde_json::to_string_pretty(&CalibrationJsonRecord { core })
    }
    .map_err(|e| format!("Failed to serialize calibration record: {e}"))?;
    fs::write(&path, json).map_err(|e| format!("Failed to write {}: {e}", path.display()))?;

    Ok(path)
}

fn fit_label(channel: CalibrationChannel, fit: &VoltageFit) -> String {
    let voltage_fit = format!(
        "{}: expected = {:.9} + {:.9} * measured, R^2 = {:.9}",
        channel.fit_heading(),
        fit.coefficients[0],
        fit.coefficients[1],
        fit.r2,
    );

    if let Some(temperature_error_fit) = &fit.temperature_error_fit {
        format!(
            "Temperature error fit: error_K = {:.9} + {:.9} * ((T_K - {:.6}) / {:.6}), R^2 = {:.9}; {voltage_fit}",
            temperature_error_fit.coefficients[0],
            temperature_error_fit.coefficients[1],
            temperature_error_fit.x_center,
            temperature_error_fit.x_scale,
            temperature_error_fit.r2,
        )
    } else {
        voltage_fit
    }
}

fn cold_junction_label(capture: &ChannelCapture) -> Result<Option<String>, String> {
    if matches!(capture.channel.kind, CalibrationKind::Thermocouple) {
        let cold_junction = thermocouple_cold_junction_json_record(capture)?;
        Ok(Some(format!(
            "Cold junction: calibrator = {:.3} K, temperature offset = {:.6} K, RTD voltage offset = {:.9} V",
            cold_junction.calibrator_cold_junction_temperature_k,
            cold_junction.cold_junction_temperature_offset_k,
            cold_junction.cold_junction_rtd_voltage_offset_v,
        )))
    } else {
        Ok(None)
    }
}

fn write_analysis_plots(
    capture: &ChannelCapture,
    analysis: &ChannelAnalysis,
) -> Result<PlotPaths, String> {
    Ok(PlotPaths {
        light: write_analysis_plot(capture, analysis, PlotTheme::Light)?,
        dark: write_analysis_plot(capture, analysis, PlotTheme::Dark)?,
    })
}

fn write_analysis_plot(
    capture: &ChannelCapture,
    analysis: &ChannelAnalysis,
    plot_theme: PlotTheme,
) -> Result<PathBuf, String> {
    let path = capture.op_dir.join(format!(
        "{}_plots_{}.html",
        capture.op_name,
        plot_theme.file_suffix()
    ));
    let display_scale = capture.channel.display_scale();

    let raw_time_s = capture.times_s.clone();
    let raw_measured_ma = capture
        .measured_a
        .iter()
        .map(|measured_a| measured_a * display_scale)
        .collect::<Vec<_>>();
    let accepted_reference_display = analysis
        .samples
        .iter()
        .map(|sample| sample.reference_a * display_scale)
        .collect::<Vec<_>>();
    let error_plot_y = analysis
        .samples
        .iter()
        .map(|sample| match capture.channel.kind {
            CalibrationKind::Current4To20 => sample.error_a * 1e3,
            CalibrationKind::Rtd => sample.error_a,
            CalibrationKind::Thermocouple => sample.error_a,
            CalibrationKind::Voltage => sample.error_a,
        })
        .collect::<Vec<_>>();
    let min_error_reference = accepted_reference_display
        .iter()
        .copied()
        .fold(f64::INFINITY, f64::min);
    let max_error_reference = accepted_reference_display
        .iter()
        .copied()
        .fold(f64::NEG_INFINITY, f64::max);
    let (min_error_reference, max_error_reference) = if min_error_reference == max_error_reference {
        let padding = (min_error_reference.abs() * 0.05).max(1.0);
        (min_error_reference - padding, max_error_reference + padding)
    } else {
        (min_error_reference, max_error_reference)
    };
    let (error_accuracy_x, error_accuracy_y) =
        accuracy_band_xy(min_error_reference, max_error_reference, |reference| {
            capture.channel.error_accuracy_limit_at_reference(reference)
        });
    let max_abs_error = error_plot_y
        .iter()
        .map(|error| error.abs())
        .fold(0.0, f64::max);
    let max_abs_error_accuracy = error_accuracy_y
        .iter()
        .map(|accuracy| accuracy.abs())
        .fold(0.0, f64::max);
    let error_y_limit = (2.0 * max_abs_error_accuracy).max(max_abs_error).max(1e-12);
    let error_y_axis_range = vec![-error_y_limit, error_y_limit];
    let fit_measured_x = analysis.voltage_fit.measured_x.clone();
    let fit_expected_y = analysis.voltage_fit.expected_y.clone();
    let min_fit_x = fit_measured_x.iter().copied().fold(f64::INFINITY, f64::min);
    let max_fit_x = fit_measured_x
        .iter()
        .copied()
        .fold(f64::NEG_INFINITY, f64::max);
    let (min_fit_x, max_fit_x) = if min_fit_x == max_fit_x {
        let padding = (min_fit_x.abs() * 0.05).max(1.0);
        (min_fit_x - padding, max_fit_x + padding)
    } else {
        (min_fit_x, max_fit_x)
    };
    let fit_line_x = vec![min_fit_x, max_fit_x];
    let fit_line_y = fit_line_x
        .iter()
        .map(|&measured| polyval(measured, &analysis.voltage_fit.coefficients))
        .collect::<Vec<_>>();
    let fit_residual_y = analysis
        .voltage_fit
        .residuals
        .iter()
        .map(|residual| match capture.channel.kind {
            CalibrationKind::Current4To20 => residual / REFERENCE_RESISTOR_OHM * 1e6,
            CalibrationKind::Rtd => *residual,
            CalibrationKind::Thermocouple => *residual,
            CalibrationKind::Voltage => *residual,
        })
        .collect::<Vec<_>>();
    let fit_residual_reference = analysis
        .samples
        .iter()
        .map(|sample| sample.reference_a * display_scale)
        .collect::<Vec<_>>();
    let voltage_fit_label = fit_label(capture.channel, &analysis.voltage_fit);
    let cold_junction_label_html = if let Some(cold_junction_label) = cold_junction_label(capture)?
    {
        format!(
            r#"<p class="plot-note">{}</p>"#,
            html_escape(&cold_junction_label)
        )
    } else {
        String::new()
    };
    let residual_trace_name = match capture.channel.kind {
        CalibrationKind::Current4To20 => "Fit residual as current",
        CalibrationKind::Rtd => "Propagated temperature residual",
        CalibrationKind::Thermocouple => "Propagated temperature residual",
        CalibrationKind::Voltage => "Voltage fit residual",
    };

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
    let accepted_fill_color = plot_theme.accepted_fill_color();
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
                "fillcolor": accepted_fill_color,
                "line": { "width": 0 },
                "layer": "below",
            })
        })
        .collect::<Vec<_>>();
    let min_residual_reference = fit_residual_reference
        .iter()
        .copied()
        .fold(f64::INFINITY, f64::min);
    let max_residual_reference = fit_residual_reference
        .iter()
        .copied()
        .fold(f64::NEG_INFINITY, f64::max);
    let (min_residual_reference, max_residual_reference) =
        if min_residual_reference == max_residual_reference {
            let padding = (min_residual_reference.abs() * 0.05).max(1.0);
            (
                min_residual_reference - padding,
                max_residual_reference + padding,
            )
        } else {
            (min_residual_reference, max_residual_reference)
        };
    let (residual_accuracy_x, residual_accuracy_y) = accuracy_band_xy(
        min_residual_reference,
        max_residual_reference,
        |reference| {
            capture
                .channel
                .residual_accuracy_limit_at_reference(reference)
        },
    );
    let max_abs_residual = fit_residual_y
        .iter()
        .map(|residual| residual.abs())
        .fold(0.0, f64::max);
    let max_abs_residual_accuracy = residual_accuracy_y
        .iter()
        .map(|accuracy| accuracy.abs())
        .fold(0.0, f64::max);
    let residual_y_limit = (2.0 * max_abs_residual_accuracy)
        .max(max_abs_residual)
        .max(1e-12);
    let residual_y_axis_range = vec![-residual_y_limit, residual_y_limit];

    let title = format!(
        "{} SN{} {} calibration",
        MODEL_NAME, capture.serial_number, capture.channel.slug
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
            background: {page_background};
            color: {text_color};
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
            color: {text_color};
        }}
        .plot-note {{
            margin: -2px 0 8px;
            font-size: 14px;
            color: {text_color};
        }}
    </style>
</head>
<body>
<main>
    <h1>{title}</h1>
    <div id="time-overlay" class="plot"></div>
    <div id="relative-error" class="plot"></div>
    <h2 class="plot-heading">{voltage_fit_label_html}</h2>
    {cold_junction_label_html}
    <div id="voltage-fit" class="plot"></div>
    <div id="voltage-fit-residual" class="plot"></div>
</main>
<script>
const rawTimeS = {raw_time_s};
const rawMeasuredMA = {raw_measured_ma};
const acceptedReference = {accepted_reference_display};
const referenceStepTimeS = {reference_step_time_s};
const referenceStepMA = {reference_step_ma};
const errorPlotY = {error_plot_y};
const errorAccuracyX = {error_accuracy_x};
const errorAccuracyY = {error_accuracy_y};
const errorYAxisRange = {error_y_axis_range};
const acceptedShapes = {accepted_shapes};
const fitMeasuredX = {fit_measured_x};
const fitExpectedY = {fit_expected_y};
const fitLineX = {fit_line_x};
const fitLineY = {fit_line_y};
const residualAccuracyX = {residual_accuracy_x};
const residualAccuracyY = {residual_accuracy_y};
const residualYAxisRange = {residual_y_axis_range};
const fitResidualReference = {fit_residual_reference};
const fitResidualY = {fit_residual_y};
const voltageFitLabel = {voltage_fit_label};
const textColor = {text_color_js};
const traceColor = {trace_color};
const plotBackground = {plot_background};
const gridColor = {grid_color};
const boxFillColor = {box_fill_color};
const accuracyFillColor = {accuracy_fill_color};

const baseLayout = {{
    paper_bgcolor: plotBackground,
    plot_bgcolor: plotBackground,
    font: {{ color: textColor }},
    legend: {{ font: {{ color: textColor }} }}
}};

function themedLayout(layout) {{
    return {{
        ...baseLayout,
        ...layout,
        xaxis: {{
            gridcolor: gridColor,
            zerolinecolor: gridColor,
            linecolor: gridColor,
            tickcolor: gridColor,
            ...layout.xaxis
        }},
        yaxis: {{
            gridcolor: gridColor,
            zerolinecolor: gridColor,
            linecolor: gridColor,
            tickcolor: gridColor,
            ...layout.yaxis
        }}
    }};
}}

Plotly.newPlot("time-overlay", [
    {{
        x: rawTimeS,
        y: rawMeasuredMA,
        mode: "lines",
        type: "scatter",
        name: "Measured",
        line: {{ width: 1, color: traceColor }}
    }},
    {{
        x: referenceStepTimeS,
        y: referenceStepMA,
        mode: "lines",
        type: "scatter",
        name: "Detected reference step",
        connectgaps: false,
        line: {{ width: 2, color: traceColor, dash: "dash" }}
    }}
], themedLayout({{
    title: {{ text: "Detected steps and accepted calibration regions" }},
    xaxis: {{ title: {{ text: "Time (s)", standoff: 16 }}, automargin: true }},
    yaxis: {{ title: {{ text: {value_axis_label}, standoff: 18 }}, automargin: true }},
    shapes: acceptedShapes,
    margin: {{ l: 88, r: 24, t: 64, b: 78 }}
}}), {{ responsive: true }});

Plotly.newPlot("relative-error", [
    {{
        x: errorAccuracyX,
        y: errorAccuracyY,
        mode: "lines",
        type: "scatter",
        fill: "toself",
        name: {error_accuracy_label},
        line: {{ width: 0, color: accuracyFillColor }},
        fillcolor: accuracyFillColor,
        hoverinfo: "skip"
    }},
    {{
        x: acceptedReference,
        y: errorPlotY,
        type: "box",
        name: "Error distribution",
        boxpoints: false,
        boxmean: "sd",
        line: {{ width: 2, color: traceColor }},
        fillcolor: boxFillColor,
        marker: {{ color: traceColor }}
    }},
    {{
        x: acceptedReference,
        y: errorPlotY,
        mode: "markers",
        type: "scatter",
        name: "Accepted samples",
        marker: {{ size: 4, color: traceColor }}
    }}
], themedLayout({{
    title: {{ text: {error_plot_title} }},
    xaxis: {{ title: {{ text: {reference_axis_label}, standoff: 16 }}, automargin: true }},
    yaxis: {{ title: {{ text: {error_axis_label}, standoff: 18 }}, range: errorYAxisRange, automargin: true }},
    boxmode: "group",
    margin: {{ l: 88, r: 24, t: 64, b: 78 }}
}}), {{ responsive: true }});

Plotly.newPlot("voltage-fit", [
    {{
        x: fitMeasuredX,
        y: fitExpectedY,
        mode: "markers",
        type: "scatter",
        name: "Accepted calibration samples",
        marker: {{ size: 5, color: traceColor }}
    }},
    {{
        x: fitLineX,
        y: fitLineY,
        mode: "lines",
        type: "scatter",
        name: "Linear fit",
        line: {{ width: 2, color: traceColor }}
    }}
], themedLayout({{
    title: {{ text: {fit_plot_title} }},
    xaxis: {{ title: {{ text: {fit_x_axis_label}, standoff: 16 }}, automargin: true }},
    yaxis: {{ title: {{ text: {fit_y_axis_label}, standoff: 18 }}, automargin: true }},
    margin: {{ l: 88, r: 24, t: 100, b: 78 }}
}}), {{ responsive: true }});

Plotly.newPlot("voltage-fit-residual", [
    {{
        x: residualAccuracyX,
        y: residualAccuracyY,
        mode: "lines",
        type: "scatter",
        fill: "toself",
        name: {residual_accuracy_label},
        line: {{ width: 0, color: accuracyFillColor }},
        fillcolor: accuracyFillColor,
        hoverinfo: "skip"
    }},
    {{
        x: fitResidualReference,
        y: fitResidualY,
        type: "box",
        name: "Residual distribution",
        boxpoints: false,
        boxmean: "sd",
        line: {{ width: 2, color: traceColor }},
        fillcolor: boxFillColor,
        marker: {{ color: traceColor }}
    }},
    {{
        x: fitResidualReference,
        y: fitResidualY,
        mode: "markers",
        type: "scatter",
        name: {residual_trace_name},
        marker: {{ size: 5, color: traceColor }}
    }}
], themedLayout({{
    title: {{ text: {residual_plot_title} }},
    xaxis: {{ title: {{ text: {residual_x_axis_label}, standoff: 16 }}, automargin: true }},
    yaxis: {{ title: {{ text: {residual_axis_label}, standoff: 18 }}, range: residualYAxisRange, automargin: true }},
    boxmode: "group",
    margin: {{ l: 88, r: 24, t: 64, b: 78 }}
}}), {{ responsive: true }});
</script>
</body>
</html>
"##,
        title = html_escape(&title),
        voltage_fit_label_html = html_escape(&voltage_fit_label),
        cold_junction_label_html = cold_junction_label_html,
        page_background = plot_theme.page_background(),
        text_color = plot_theme.text_color(),
        text_color_js = serde_json::to_string(plot_theme.text_color())
            .map_err(|e| format!("Failed to serialize plot text color: {e}"))?,
        trace_color = serde_json::to_string(plot_theme.trace_color())
            .map_err(|e| format!("Failed to serialize plot trace color: {e}"))?,
        plot_background = serde_json::to_string(plot_theme.plot_background())
            .map_err(|e| format!("Failed to serialize plot background: {e}"))?,
        grid_color = serde_json::to_string(plot_theme.grid_color())
            .map_err(|e| format!("Failed to serialize plot grid color: {e}"))?,
        box_fill_color = serde_json::to_string(plot_theme.box_fill_color())
            .map_err(|e| format!("Failed to serialize plot box fill color: {e}"))?,
        accuracy_fill_color = serde_json::to_string(plot_theme.accuracy_fill_color())
            .map_err(|e| format!("Failed to serialize plot accuracy fill color: {e}"))?,
        value_axis_label = serde_json::to_string(capture.channel.value_axis_label())
            .map_err(|e| format!("Failed to serialize value axis label: {e}"))?,
        raw_time_s = serde_json::to_string(&raw_time_s)
            .map_err(|e| format!("Failed to serialize plot raw time data: {e}"))?,
        raw_measured_ma = serde_json::to_string(&raw_measured_ma)
            .map_err(|e| format!("Failed to serialize plot raw measurement data: {e}"))?,
        accepted_reference_display = serde_json::to_string(&accepted_reference_display)
            .map_err(|e| format!("Failed to serialize plot accepted reference data: {e}"))?,
        reference_step_time_s = serde_json::to_string(&reference_step_time_s)
            .map_err(|e| format!("Failed to serialize plot reference-step time data: {e}"))?,
        reference_step_ma = serde_json::to_string(&reference_step_ma)
            .map_err(|e| format!("Failed to serialize plot reference-step data: {e}"))?,
        error_plot_y = serde_json::to_string(&error_plot_y)
            .map_err(|e| format!("Failed to serialize plot relative-error y data: {e}"))?,
        error_accuracy_x = serde_json::to_string(&error_accuracy_x)
            .map_err(|e| format!("Failed to serialize plot error accuracy x data: {e}"))?,
        error_accuracy_y = serde_json::to_string(&error_accuracy_y)
            .map_err(|e| format!("Failed to serialize plot error accuracy y data: {e}"))?,
        error_y_axis_range = serde_json::to_string(&error_y_axis_range)
            .map_err(|e| format!("Failed to serialize plot error y-axis range: {e}"))?,
        error_accuracy_label = serde_json::to_string(capture.channel.error_accuracy_label())
            .map_err(|e| format!("Failed to serialize error accuracy label: {e}"))?,
        accepted_shapes = serde_json::to_string(&accepted_shapes)
            .map_err(|e| format!("Failed to serialize plot accepted-region shapes: {e}"))?,
        residual_accuracy_x = serde_json::to_string(&residual_accuracy_x)
            .map_err(|e| format!("Failed to serialize plot residual accuracy x data: {e}"))?,
        residual_accuracy_y = serde_json::to_string(&residual_accuracy_y)
            .map_err(|e| format!("Failed to serialize plot residual accuracy y data: {e}"))?,
        residual_y_axis_range = serde_json::to_string(&residual_y_axis_range)
            .map_err(|e| format!("Failed to serialize plot residual y-axis range: {e}"))?,
        residual_accuracy_label = serde_json::to_string(capture.channel.residual_accuracy_label())
            .map_err(|e| format!("Failed to serialize residual accuracy label: {e}"))?,
        fit_measured_x = serde_json::to_string(&fit_measured_x)
            .map_err(|e| format!("Failed to serialize plot measured-voltage data: {e}"))?,
        fit_expected_y = serde_json::to_string(&fit_expected_y)
            .map_err(|e| format!("Failed to serialize plot expected-voltage data: {e}"))?,
        fit_line_x = serde_json::to_string(&fit_line_x)
            .map_err(|e| format!("Failed to serialize plot fit-line measured-voltage data: {e}"))?,
        fit_line_y = serde_json::to_string(&fit_line_y)
            .map_err(|e| format!("Failed to serialize plot fit-line expected-voltage data: {e}"))?,
        fit_residual_reference = serde_json::to_string(&fit_residual_reference)
            .map_err(|e| format!("Failed to serialize plot fit residual x data: {e}"))?,
        fit_residual_y = serde_json::to_string(&fit_residual_y)
            .map_err(|e| format!("Failed to serialize plot fit residual current data: {e}"))?,
        voltage_fit_label = serde_json::to_string(&voltage_fit_label)
            .map_err(|e| format!("Failed to serialize voltage-fit label: {e}"))?,
        residual_trace_name = serde_json::to_string(residual_trace_name)
            .map_err(|e| format!("Failed to serialize residual trace name: {e}"))?,
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
        residual_x_axis_label = serde_json::to_string(capture.channel.residual_x_axis_label())
            .map_err(|e| format!("Failed to serialize residual x-axis label: {e}"))?,
        residual_axis_label = serde_json::to_string(capture.channel.residual_axis_label())
            .map_err(|e| format!("Failed to serialize residual axis label: {e}"))?,
    );

    fs::write(&path, html).map_err(|e| format!("Failed to write {}: {e}", path.display()))?;
    Ok(path)
}

fn op_name_for_channel(sn: u64, channel: CalibrationChannel) -> Result<String, String> {
    Ok(format!(
        "{MODEL_NAME}_sn{sn}_{}_{}",
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
