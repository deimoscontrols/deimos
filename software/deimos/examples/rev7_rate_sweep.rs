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
    thread,
    time::Duration,
};

use deimos_shared::peripherals::deimos_daq_rev7::{DEIMOS_MAX_CYCLE_RATE_HZ, MIN_CYCLE_RATE_HZ};
use plotly::{
    Configuration, Layout, Plot, Scatter,
    common::{DashType, Font, Line, Mode, Title},
    layout::{Axis, AxisType, Legend, Margin},
};
use rev7_rate_benchmark_common::{BenchmarkConfig, BenchmarkMode, BenchmarkResult, run_benchmark};

const RATE_COUNT: usize = 20;
const DEFAULT_RUN_SECONDS: u64 = 20;
const EFFICIENT_MAX_EXCLUSIVE_HZ: f64 = 500.0;
const OP_NAME_PREFIX: &str = "rev7_rate_sweep";
const SUMMARY_FILENAME: &str = "rev7_rate_sweep_summary.csv";
const STATUS_FILENAME: &str = "rev7_rate_sweep_status.txt";
const WEBSITE_ASSET_DIR: &str = "./software/deimos_website/docs/assets";
const POINT_REENTRY_DELAY: Duration = Duration::from_millis(2_200);
const DISCOVERY_ATTEMPTS: usize = 2;

struct Theme {
    suffix: &'static str,
    color_scheme: &'static str,
    paper_background: &'static str,
    plot_background: &'static str,
    foreground: &'static str,
    grid: &'static str,
    performant: &'static str,
    efficient: &'static str,
}

fn themes() -> [Theme; 2] {
    [
        Theme {
            suffix: "light",
            color_scheme: "light",
            paper_background: "#ffffff",
            plot_background: "#ffffff",
            foreground: "#171922",
            grid: "#d8deea",
            performant: "#0072B2",
            efficient: "#D55E00",
        },
        Theme {
            suffix: "dark",
            color_scheme: "dark",
            paper_background: "#2b2f3a",
            plot_background: "#2b2f3a",
            foreground: "#f2f0f6",
            grid: "#47404f",
            performant: "#56B4E9",
            efficient: "#E69F00",
        },
    ]
}

fn main() -> Result<(), String> {
    let run_seconds = env_value("DEIMOS_SWEEP_SECONDS", DEFAULT_RUN_SECONDS)?;
    let output_dir = PathBuf::from("./target/rev7_rate_sweep");
    fs::create_dir_all(&output_dir)
        .map_err(|e| format!("Failed to create sweep output directory: {e}"))?;
    let rates = logspace(
        f64::from(MIN_CYCLE_RATE_HZ),
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

    // Complete the full firmware timing curve even if the later Efficient
    // characterization finds a host-loop limit.
    for (point, rate_hz) in rates.iter().copied().enumerate() {
        let result = acquire_point(
            point,
            rate_hz,
            run_seconds,
            BenchmarkMode::Performant,
            &output_dir,
            "primary",
        )?;
        record_result(&mut results, result, &output_dir)?;
    }

    let efficient_candidates = rates
        .iter()
        .copied()
        .enumerate()
        .filter(|(_, rate_hz)| *rate_hz < EFFICIENT_MAX_EXCLUSIVE_HZ)
        .collect::<Vec<_>>();
    let mut efficient_successes = Vec::with_capacity(efficient_candidates.len());
    let mut first_failed_rate_hz = None;
    for (point, rate_hz) in efficient_candidates {
        match acquire_point(
            point,
            rate_hz,
            run_seconds,
            BenchmarkMode::Efficient,
            &output_dir,
            "primary",
        ) {
            Ok(result) => {
                record_result(&mut results, result, &output_dir)?;
                efficient_successes.push((point, rate_hz));
            }
            Err(error) if rate_hz > 50.0 => {
                eprintln!(
                    "Efficient mode failed at {rate_hz:.6} Hz; lowering its maximum: {error}"
                );
                first_failed_rate_hz = Some(rate_hz);
                break;
            }
            Err(error) => return Err(error),
        }
    }

    let efficient_max_reliable_hz = if first_failed_rate_hz.is_some() {
        let mut confirmed = None;
        // Each candidate is attempted at most once more. A failed confirmation
        // removes that point from the published comparison before stepping
        // down to the next completed rate.
        for (point, rate_hz) in efficient_successes.into_iter().rev() {
            match acquire_point(
                point,
                rate_hz,
                run_seconds,
                BenchmarkMode::Efficient,
                &output_dir,
                "confirm",
            ) {
                Ok(result) => {
                    record_result(&mut results, result, &output_dir)?;
                    confirmed = Some(rate_hz);
                    break;
                }
                Err(error) => {
                    eprintln!(
                        "Efficient confirmation failed at {rate_hz:.6} Hz; stepping down: {error}"
                    );
                    remove_result(
                        &mut results,
                        BenchmarkMode::Efficient,
                        period_ns(rate_hz)?,
                        &output_dir,
                    )?;
                }
            }
        }
        confirmed
    } else {
        efficient_successes.last().map(|(_, rate_hz)| *rate_hz)
    };

    write_status(&output_dir, efficient_max_reliable_hz, first_failed_rate_hz)?;
    publish_website_assets(&output_dir)?;
    println!("wrote {}", output_dir.join(SUMMARY_FILENAME).display());
    for theme in themes() {
        println!(
            "wrote {}",
            output_dir
                .join(format!("{OP_NAME_PREFIX}_{}.html", theme.suffix))
                .display()
        );
    }
    println!("published sweep plots to {WEBSITE_ASSET_DIR}");
    Ok(())
}

/// Acquire one point without changing the aggregate result set.
fn acquire_point(
    point: usize,
    rate_hz: f64,
    run_seconds: u64,
    mode: BenchmarkMode,
    output_dir: &Path,
    attempt: &str,
) -> Result<BenchmarkResult, String> {
    let config = BenchmarkConfig {
        rate_hz,
        run_seconds,
        mode,
        op_name: format!("{OP_NAME_PREFIX}_{point:02}_{}_{attempt}", mode.name()),
        output_dir: output_dir.to_owned(),
    };
    for discovery_attempt in 0..DISCOVERY_ATTEMPTS {
        match run_benchmark(&config) {
            Ok(result) => {
                result.print_summary();
                // The firmware returns from Operating only after its two-second
                // peripheral loss-of-contact timeout. Keep the next discovery
                // scan from consuming the final operating response.
                thread::sleep(POINT_REENTRY_DELAY);
                return Ok(result);
            }
            Err(error)
                if discovery_attempt + 1 < DISCOVERY_ATTEMPTS
                    && error.contains("Required peripherals not found") =>
            {
                eprintln!(
                    "Discovery attempt {} failed at {rate_hz:.6} Hz; waiting for Binding: {error}",
                    discovery_attempt + 1
                );
                thread::sleep(POINT_REENTRY_DELAY);
            }
            Err(error) => return Err(error),
        }
    }
    unreachable!("bounded discovery loop either returns a result or an error")
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

fn period_ns(rate_hz: f64) -> Result<u32, String> {
    if !rate_hz.is_finite() || rate_hz <= 0.0 {
        return Err(format!("Invalid sweep rate {rate_hz}"));
    }
    let period_ns = (1e9_f64 / rate_hz).round() as u32;
    (period_ns != 0)
        .then_some(period_ns)
        .ok_or_else(|| format!("Sweep rate {rate_hz} Hz produces a zero period"))
}

/// Insert or replace one mode/rate result and checkpoint aggregate artifacts.
fn record_result(
    results: &mut Vec<BenchmarkResult>,
    result: BenchmarkResult,
    output_dir: &Path,
) -> Result<(), String> {
    if let Some(index) = results
        .iter()
        .position(|existing| existing.mode == result.mode && existing.period_ns == result.period_ns)
    {
        results[index] = result;
    } else {
        results.push(result);
    }
    results.sort_by(|left, right| {
        left.rate_hz
            .total_cmp(&right.rate_hz)
            .then_with(|| left.mode.cmp(&right.mode))
    });
    checkpoint_results(results, output_dir)
}

/// Remove an Efficient point that failed its reliability confirmation.
fn remove_result(
    results: &mut Vec<BenchmarkResult>,
    mode: BenchmarkMode,
    period_ns: u32,
    output_dir: &Path,
) -> Result<(), String> {
    results.retain(|result| !(result.mode == mode.name() && result.period_ns == period_ns));
    checkpoint_results(results, output_dir)
}

/// Rewrite all recoverable aggregate artifacts after a result-set change.
fn checkpoint_results(results: &[BenchmarkResult], output_dir: &Path) -> Result<(), String> {
    write_summary(results, &output_dir.join(SUMMARY_FILENAME))?;
    write_plots(results, output_dir)
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

/// Record the observed Efficient ceiling and the first failed probe.
fn write_status(
    output_dir: &Path,
    efficient_max_reliable_hz: Option<f64>,
    first_failed_rate_hz: Option<f64>,
) -> Result<(), String> {
    let max_rate = efficient_max_reliable_hz
        .map(|rate| format!("{rate:.6}"))
        .unwrap_or_else(|| "none".to_owned());
    let failed_rate = first_failed_rate_hz
        .map(|rate| format!("{rate:.6}"))
        .unwrap_or_else(|| "none".to_owned());
    fs::write(
        output_dir.join(STATUS_FILENAME),
        format!(
            "efficient_max_reliable_hz={max_rate}\nfirst_failed_efficient_rate_hz={failed_rate}\n"
        ),
    )
    .map_err(|error| format!("Failed to write sweep status: {error}"))
}

/// Copy final aggregate artifacts into the website's static asset directory.
fn publish_website_assets(output_dir: &Path) -> Result<(), String> {
    let asset_dir = Path::new(WEBSITE_ASSET_DIR);
    fs::create_dir_all(asset_dir)
        .map_err(|error| format!("Failed to create website asset directory: {error}"))?;
    for theme in themes() {
        let filename = format!("{OP_NAME_PREFIX}_{}.html", theme.suffix);
        fs::copy(output_dir.join(&filename), asset_dir.join(&filename))
            .map_err(|error| format!("Failed to publish {filename}: {error}"))?;
    }
    for filename in [SUMMARY_FILENAME, STATUS_FILENAME] {
        fs::copy(output_dir.join(filename), asset_dir.join(filename))
            .map_err(|error| format!("Failed to publish {filename}: {error}"))?;
    }
    Ok(())
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
fn rate_axis(theme: &Theme, title: &str) -> Axis {
    Axis::new()
        .title(Title::with_text(title))
        .type_(AxisType::Log)
        .color(theme.foreground)
        .show_line(true)
        .line_color(theme.foreground)
        .line_width(1)
        .show_grid(true)
        .grid_color(theme.grid)
        .grid_width(1)
        .zero_line(true)
        .zero_line_color(theme.grid)
        .auto_margin(true)
        .tick_font(Font::new().color(theme.foreground))
}

/// Build one linear metric axis over a normalized vertical domain.
fn value_axis(theme: &Theme, title: &str, domain: &[f64]) -> Axis {
    Axis::new()
        .title(Title::with_text(title))
        .domain(domain)
        .color(theme.foreground)
        .show_line(true)
        .line_color(theme.foreground)
        .line_width(1)
        .show_grid(true)
        .grid_color(theme.grid)
        .grid_width(1)
        .zero_line(true)
        .zero_line_color(theme.grid)
        .auto_margin(true)
        .tick_font(Font::new().color(theme.foreground))
}

/// Rewrite light and dark interactive plots from all completed points.
fn write_plots(results: &[BenchmarkResult], output_dir: &Path) -> Result<(), String> {
    for theme in themes() {
        let plot = build_plot(results, &theme);
        write_themed_html(
            &plot,
            &output_dir.join(format!("{OP_NAME_PREFIX}_{}.html", theme.suffix)),
            &theme,
        )?;
    }
    Ok(())
}

fn build_plot(results: &[BenchmarkResult], theme: &Theme) -> Plot {
    let mut plot = Plot::new();
    plot.set_configuration(Configuration::new().responsive(true).display_logo(false));
    for (mode, label, color) in [
        (BenchmarkMode::Performant, "Performant", theme.performant),
        (BenchmarkMode::Efficient, "Efficient", theme.efficient),
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
            .font(Font::new().color(theme.foreground))
            .paper_background_color(theme.paper_background)
            .plot_background_color(theme.plot_background)
            .margin(Margin::new().left(90).right(40).top(80).bottom(70))
            .legend(
                Legend::new()
                    .font(Font::new().color(theme.foreground))
                    .x(1.01)
                    .y(1.0),
            )
            .x_axis(
                rate_axis(theme, "Cycle rate [Hz]")
                    .domain(&[0.0, 0.88])
                    .anchor("y"),
            )
            .y_axis(
                value_axis(theme, "Loss rate [dropped cycle / cycle]", &[0.70, 1.0]).anchor("x"),
            )
            .x_axis2(
                rate_axis(theme, "Cycle rate [Hz]")
                    .domain(&[0.0, 0.88])
                    .anchor("y2"),
            )
            .y_axis2(value_axis(theme, "Board cycle margin [us]", &[0.35, 0.63]).anchor("x2"))
            .x_axis3(
                rate_axis(theme, "Cycle rate [Hz]")
                    .domain(&[0.0, 0.88])
                    .anchor("y3"),
            )
            .y_axis3(
                value_axis(theme, "Host-process CPU [% of one CPU]", &[0.0, 0.28]).anchor("x3"),
            ),
    );
    plot
}

fn write_themed_html(plot: &Plot, path: &Path, theme: &Theme) -> Result<(), String> {
    let page_style = format!(
        "<style>:root {{ color-scheme: {}; }} \
         html, body {{ margin: 0; background: {}; }}</style>",
        theme.color_scheme, theme.paper_background
    );
    let html = plot
        .to_html()
        .replace("</head>", &format!("{page_style}\n</head>"));
    fs::write(path, html)
        .map_err(|error| format!("Failed to write sweep plot {}: {error}", path.display()))
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
