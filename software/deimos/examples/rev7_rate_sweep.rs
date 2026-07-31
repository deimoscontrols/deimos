//! Sweep rev7 publishing rate in Performant and Efficient controller modes.
//!
//! The default run takes about 11 minutes: 20 logarithmically spaced
//! Performant points from 5 Hz through 8 kHz, plus Efficient comparisons for
//! every requested point below 500 Hz. Each point runs for 20 seconds.
//!
//! Run from the repository root with
//! `cargo run -p deimos --release --example rev7_rate_sweep`. Raw point CSVs,
//! the aggregate summary, and the HTML plot are written under
//! `target/rev7_rate_sweep/`.
//!
//! Host CPU is measured with process-wide Unix `getrusage` samples over the
//! final five seconds. It includes controller worker threads and is expressed
//! as percent of one logical CPU, so values above 100% are possible.

mod rev7_rate_benchmark_common;

use std::{
    env, fs,
    path::{Path, PathBuf},
};

use deimos_shared::peripherals::deimos_daq_rev7::{
    DEIMOS_MAX_CYCLE_RATE_HZ, REV7_MIN_CYCLE_RATE_HZ,
};
use plotly::{
    Configuration, Layout, Plot, Scatter,
    common::{DashType, Line, Mode, Title},
    layout::{Axis, AxisType, Legend, Margin},
};
use rev7_rate_benchmark_common::{BenchmarkConfig, BenchmarkMode, BenchmarkResult, run_benchmark};

const RATE_COUNT: usize = 20;
const DEFAULT_RUN_SECONDS: u64 = 20;
const EFFICIENT_MAX_EXCLUSIVE_HZ: f64 = 500.0;
const OP_NAME_PREFIX: &str = "rev7_rate_sweep";

fn main() -> Result<(), String> {
    let run_seconds = env_value("DEIMOS_SWEEP_SECONDS", DEFAULT_RUN_SECONDS)?;
    let output_dir = PathBuf::from("./target/rev7_rate_sweep");
    fs::create_dir_all(&output_dir)
        .map_err(|e| format!("Failed to create sweep output directory: {e}"))?;
    let summary_path = output_dir.join("rev7_rate_sweep_summary.csv");
    let plot_path = output_dir.join("rev7_rate_sweep.html");
    let rates = logspace(
        f64::from(REV7_MIN_CYCLE_RATE_HZ),
        f64::from(DEIMOS_MAX_CYCLE_RATE_HZ),
        RATE_COUNT,
    );
    let mut results = Vec::with_capacity(
        rates.len()
            + rates
                .iter()
                .filter(|rate| **rate < EFFICIENT_MAX_EXCLUSIVE_HZ)
                .count(),
    );

    for (point, rate_hz) in rates.into_iter().enumerate() {
        run_point(
            &mut results,
            point,
            rate_hz,
            run_seconds,
            BenchmarkMode::Performant,
            &output_dir,
        )?;
        if rate_hz < EFFICIENT_MAX_EXCLUSIVE_HZ {
            run_point(
                &mut results,
                point,
                rate_hz,
                run_seconds,
                BenchmarkMode::Efficient,
                &output_dir,
            )?;
        }
    }

    println!("wrote {}", summary_path.display());
    println!("wrote {}", plot_path.display());
    Ok(())
}

/// Acquire one point and checkpoint the updated aggregate artifacts.
fn run_point(
    results: &mut Vec<BenchmarkResult>,
    point: usize,
    rate_hz: f64,
    run_seconds: u64,
    mode: BenchmarkMode,
    output_dir: &Path,
) -> Result<(), String> {
    let result = run_benchmark(&BenchmarkConfig {
        rate_hz,
        run_seconds,
        mode,
        op_name: format!("{OP_NAME_PREFIX}_{point:02}_{}", mode.name()),
        output_dir: output_dir.to_owned(),
    })?;
    result.print_summary();
    results.push(result);
    // Update both aggregate artifacts after every point so an interrupted
    // eleven-minute sweep still leaves a usable partial result.
    write_summary(results, &output_dir.join("rev7_rate_sweep_summary.csv"))?;
    write_plot(results, &output_dir.join("rev7_rate_sweep.html"))
}

/// Construct an inclusive logarithmically spaced rate grid.
///
/// Args:
///   start: Positive first rate in `cycle/s`.
///   stop: Positive final rate in `cycle/s`.
///   count: Number of points in the returned grid.
///
/// Returns:
///   Monotonically increasing rates with shape `(count,)`, including both
///   endpoints. A singleton grid contains `start`.
fn logspace(start: f64, stop: f64, count: usize) -> Vec<f64> {
    match count {
        0 => Vec::new(),
        1 => vec![start],
        _ => {
            let log_start = start.ln();
            let log_span = stop.ln() - log_start;
            (0..count)
                .map(|index| match index {
                    0 => start,
                    index if index == count - 1 => stop,
                    _ => (log_start + log_span * index as f64 / (count - 1) as f64).exp(),
                })
                .collect()
        }
    }
}

/// Read one typed sweep override from the environment.
fn env_value<T>(name: &str, default: T) -> Result<T, String>
where
    T: std::str::FromStr,
    T::Err: std::fmt::Display,
{
    match env::var(name) {
        Ok(value) => value
            .parse()
            .map_err(|error| format!("Invalid {name}={value:?}: {error}")),
        Err(env::VarError::NotPresent) => Ok(default),
        Err(error) => Err(format!("Could not read {name}: {error}")),
    }
}

/// Rewrite the aggregate point table so partial sweeps remain self-contained.
fn write_summary(results: &[BenchmarkResult], path: &Path) -> Result<(), String> {
    let mut writer = csv::Writer::from_path(path)
        .map_err(|e| format!("Failed to create summary CSV {}: {e}", path.display()))?;
    for result in results {
        writer
            .serialize(result)
            .map_err(|e| format!("Failed to write summary CSV {}: {e}", path.display()))?;
    }
    writer
        .flush()
        .map_err(|e| format!("Failed to flush summary CSV {}: {e}", path.display()))
}

/// Select one finite metric series for a controller mode.
///
/// Returns:
///   `(rate [cycle/s], metric)` vectors, each with shape `(n_mode_points,)`.
fn mode_values(
    results: &[BenchmarkResult],
    mode: BenchmarkMode,
    value: impl Fn(&BenchmarkResult) -> Option<f64>,
) -> (Vec<f64>, Vec<f64>) {
    results
        .iter()
        .filter(|result| result.mode == mode.name())
        .filter_map(|result| value(result).map(|value| (result.rate_hz, value)))
        .unzip()
}

/// Build one consistently styled sweep trace on the selected subplot axes.
fn trace(
    results: &[BenchmarkResult],
    mode: BenchmarkMode,
    value: impl Fn(&BenchmarkResult) -> Option<f64>,
    name: &str,
    color: &'static str,
    dash: DashType,
    axes: (&str, &str),
) -> Box<Scatter<f64, f64>> {
    let (rate_hz, values) = mode_values(results, mode, value);
    Scatter::new(rate_hz, values)
        .mode(Mode::LinesMarkers)
        .name(name)
        .line(Line::new().color(color).width(2.0).dash(dash))
        .x_axis(axes.0)
        .y_axis(axes.1)
}

/// Build a logarithmic publishing-rate axis.
fn rate_axis(title: &str) -> Axis {
    Axis::new()
        .title(Title::with_text(title))
        .type_(AxisType::Log)
        .show_grid(true)
        .auto_margin(true)
}

/// Build one linear metric axis over a normalized vertical domain.
fn value_axis(title: &str, domain: &[f64]) -> Axis {
    Axis::new()
        .title(Title::with_text(title))
        .domain(domain)
        .show_grid(true)
        .auto_margin(true)
}

/// Rewrite the interactive aggregate plot from all completed points.
fn write_plot(results: &[BenchmarkResult], path: &Path) -> Result<(), String> {
    const PERFORMANT_COLOR: &str = "#0072B2";
    const EFFICIENT_COLOR: &str = "#D55E00";

    let mut plot = Plot::new();
    plot.set_configuration(Configuration::new().responsive(true).display_logo(false));
    for (mode, label, color) in [
        (BenchmarkMode::Performant, "Performant", PERFORMANT_COLOR),
        (BenchmarkMode::Efficient, "Efficient", EFFICIENT_COLOR),
    ] {
        plot.add_trace(trace(
            results,
            mode,
            |result| Some(result.steady_final_5s_drop_rate),
            &format!("{label}: final-5-s loss rate"),
            color,
            DashType::Solid,
            ("x", "y"),
        ));
        plot.add_trace(trace(
            results,
            mode,
            |result| Some(result.steady_min_board_margin_us),
            &format!("{label}: minimum board margin"),
            color,
            DashType::Solid,
            ("x2", "y2"),
        ));
        plot.add_trace(trace(
            results,
            mode,
            |result| Some(result.steady_p01_board_margin_us),
            &format!("{label}: p01 board margin"),
            color,
            DashType::Dash,
            ("x2", "y2"),
        ));
        plot.add_trace(trace(
            results,
            mode,
            |result| result.steady_host_cpu_percent,
            &format!("{label}: host-process CPU"),
            color,
            DashType::Solid,
            ("x3", "y3"),
        ));
    }

    plot.set_layout(
        Layout::new()
            .title(Title::with_text(
                "Rev7 rate sweep: final-five-second measurements",
            ))
            .height(1100)
            .margin(Margin::new().left(90).right(40).top(80).bottom(70))
            .legend(Legend::new().x(1.01).y(1.0))
            .x_axis(
                rate_axis("Cycle rate [Hz]")
                    .domain(&[0.0, 0.88])
                    .anchor("y"),
            )
            .y_axis(value_axis("Loss rate [dropped cycle / cycle]", &[0.70, 1.0]).anchor("x"))
            .x_axis2(
                rate_axis("Cycle rate [Hz]")
                    .domain(&[0.0, 0.88])
                    .anchor("y2"),
            )
            .y_axis2(value_axis("Board cycle margin [us]", &[0.35, 0.63]).anchor("x2"))
            .x_axis3(
                rate_axis("Cycle rate [Hz]")
                    .domain(&[0.0, 0.88])
                    .anchor("y3"),
            )
            .y_axis3(value_axis("Host-process CPU [% of one CPU]", &[0.0, 0.28]).anchor("x3")),
    );
    fs::write(path, plot.to_html())
        .map_err(|e| format!("Failed to write sweep plot {}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::{RATE_COUNT, logspace};

    #[test]
    fn rate_grid_is_inclusive_and_logarithmically_spaced() {
        let rates = logspace(5.0, 8_000.0, RATE_COUNT);
        assert_eq!(rates.len(), RATE_COUNT);
        assert!((rates[0] - 5.0).abs() < f64::EPSILON);
        assert!((rates[RATE_COUNT - 1] - 8_000.0).abs() < 1e-11);
        let ratios = rates
            .windows(2)
            .map(|pair| pair[1] / pair[0])
            .collect::<Vec<_>>();
        assert!(
            ratios
                .windows(2)
                .all(|pair| (pair[1] - pair[0]).abs() < 1e-12)
        );
        assert_eq!(rates.iter().filter(|rate| **rate < 500.0).count(), 12);
    }
}
