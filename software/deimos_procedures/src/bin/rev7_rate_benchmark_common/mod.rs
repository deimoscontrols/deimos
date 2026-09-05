//! Shared rev7 rate-benchmark acquisition and analysis.
// Each of the two benchmark binaries uses a different subset of this module.
#![allow(dead_code)]

use std::{
    path::{Path, PathBuf},
    sync::mpsc::{self, RecvTimeoutError, Sender},
    thread,
    time::{Duration, Instant},
};

use deimos::{
    ChannelFilter, Controller, CsvDispatcher, Dispatcher, LoopMethod, Overflow, Termination,
    controller::context::ControllerCtx, dispatcher::load_csv, peripheral::DeimosDaqRev7,
};

pub const DAQ_SERIAL: u64 = 3;
pub const STEADY_SECONDS: u64 = 5;
pub const WARMUP_SECONDS: u64 = 5;
pub const MIN_RUN_SECONDS: u64 = WARMUP_SECONDS + STEADY_SECONDS;

const CPU_SAMPLE_INTERVAL: Duration = Duration::from_millis(250);
const CHANNELS: [&str; 4] = [
    "ctrl.cycle_time_margin_ns",
    "p1.metrics.cycle_time_margin_ns",
    "p1.metrics.loss_of_contact_counter",
    "p1.sample_time_ns",
];

/// Controller polling strategy used for one benchmark point.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BenchmarkMode {
    Performant,
    Efficient,
}

impl BenchmarkMode {
    /// Stable lowercase name used in filenames and tabular output.
    pub fn name(self) -> &'static str {
        match self {
            Self::Performant => "performant",
            Self::Efficient => "efficient",
        }
    }

    fn loop_method(self) -> LoopMethod {
        match self {
            Self::Performant => LoopMethod::Performant,
            Self::Efficient => LoopMethod::Efficient,
        }
    }
}

/// Configuration for one independent hardware benchmark.
pub struct BenchmarkConfig {
    /// Requested publishing rate in `cycle/s`.
    pub rate_hz: f64,
    /// Operating duration in `s`.
    pub run_seconds: u64,
    /// Controller polling strategy.
    pub mode: BenchmarkMode,
    /// Filename stem for the raw controller CSV and log.
    pub op_name: String,
    /// Directory receiving raw point artifacts.
    pub output_dir: PathBuf,
}

/// Analyzed result from one hardware benchmark.
#[derive(Clone, Debug, serde::Serialize)]
pub struct BenchmarkResult {
    /// Controller polling strategy.
    pub mode: String,
    /// Actual publishing rate implied by the integer period in `cycle/s`.
    pub rate_hz: f64,
    /// Configured publishing period in `ns`.
    pub period_ns: u32,
    /// Requested operating duration in `s`.
    pub run_seconds: u64,
    /// Captured row count in `cycle`.
    pub row_count: usize,
    /// Nominal row count implied by rate and duration in `cycle`.
    pub expected_cycle_count: usize,
    /// Captured final-window row count in `cycle`.
    pub steady_row_count: usize,
    /// Whole-run loss rate in `dropped cycle / cycle`.
    pub whole_drop_rate: f64,
    /// Final-five-second loss rate in `dropped cycle / cycle`.
    pub steady_final_5s_drop_rate: f64,
    /// Largest loss-of-contact counter value in `cycle`.
    pub max_loss_burst: f64,
    /// Minimum controller cycle margin in `ns`.
    pub min_controller_margin_ns: f64,
    /// Minimum board-reported cycle margin in `ns`.
    pub min_board_margin_ns: f64,
    /// First-percentile board-reported cycle margin in `ns`.
    pub board_margin_p01_ns: f64,
    /// Minimum board-reported cycle margin in the final window, in `us`.
    pub steady_min_board_margin_us: f64,
    /// First-percentile board-reported cycle margin in the final window, in `us`.
    pub steady_p01_board_margin_us: f64,
    /// Host-process CPU use during the final window, as percent of one CPU.
    pub steady_host_cpu_percent: Option<f64>,
    /// Number of decreasing acquisition timestamps.
    pub sample_time_regressions: usize,
    /// Number of nonadvancing acquisition timestamps on fresh snapshots.
    pub stale_sample_times_on_fresh_snapshots: usize,
    /// Minimum positive acquisition-timestamp step in `ns`.
    pub min_positive_sample_step_ns: f64,
    /// Maximum acquisition-timestamp step in `ns`.
    pub max_sample_step_ns: f64,
    /// Path to the point's raw controller CSV.
    pub raw_csv: String,
    #[serde(skip_serializing)]
    second_cycle_counts: Vec<usize>,
    #[serde(skip_serializing)]
    second_drop_counts: Vec<usize>,
}

impl BenchmarkResult {
    /// Print the compact summary used while a long sweep is running.
    pub fn print_summary(&self) {
        let cpu = self
            .steady_host_cpu_percent
            .map(|value| format!("{value:.2}%"))
            .unwrap_or_else(|| "unavailable".to_owned());
        println!(
            "mode={}, rate_hz={:.6}, rows={}, final_5s_drop_rate={:.8}, \
             final_5s_min_board_margin_us={:.3}, final_5s_p01_board_margin_us={:.3}, \
             final_5s_host_cpu={cpu}",
            self.mode,
            self.rate_hz,
            self.row_count,
            self.steady_final_5s_drop_rate,
            self.steady_min_board_margin_us,
            self.steady_p01_board_margin_us,
        );
    }

    /// Print the full single-point report retained by `rev7_rate_benchmark`.
    pub fn print_detailed(&self) {
        println!(
            "rev7 SN{DAQ_SERIAL} {:.6} Hz / {} s benchmark ({})",
            self.rate_hz, self.run_seconds, self.mode
        );
        println!(
            "period_ns={}, rows={}, expected={}",
            self.period_ns, self.row_count, self.expected_cycle_count
        );
        for second in 0..self.second_cycle_counts.len() {
            let cycles = self.second_cycle_counts[second];
            let dropped = self.second_drop_counts[second];
            let rate = if cycles == 0 {
                f64::NAN
            } else {
                dropped as f64 / cycles as f64
            };
            println!("second={second}, cycles={cycles}, dropped={dropped}, drop_rate={rate:.8}");
        }
        println!(
            "whole_drop_rate={:.8}, steady_final_5s_drop_rate={:.8}, max_burst={:.0}",
            self.whole_drop_rate, self.steady_final_5s_drop_rate, self.max_loss_burst
        );
        println!(
            "min_controller_margin_ns={:.0}, min_board_margin_ns={:.0}, \
             board_margin_p01_ns={:.0}, steady_min_board_margin_ns={:.0}, \
             steady_board_margin_p01_ns={:.0}",
            self.min_controller_margin_ns,
            self.min_board_margin_ns,
            self.board_margin_p01_ns,
            self.steady_min_board_margin_us * 1e3,
            self.steady_p01_board_margin_us * 1e3,
        );
        println!(
            "sample_time_regressions={}, stale_sample_times_on_fresh_snapshots={}, \
             min_positive_sample_step_ns={:.0}, max_sample_step_ns={:.0}, \
             steady_final_5s_host_cpu_percent={}",
            self.sample_time_regressions,
            self.stale_sample_times_on_fresh_snapshots,
            self.min_positive_sample_step_ns,
            self.max_sample_step_ns,
            self.steady_host_cpu_percent
                .map(|value| format!("{value:.2}"))
                .unwrap_or_else(|| "unavailable".to_owned()),
        );
    }
}

#[derive(Clone, Copy)]
struct CpuSample {
    elapsed: Duration,
    process_time: Duration,
}

/// Samples process-wide CPU time without adding work to the control-loop thread.
struct CpuMonitor {
    stop: Sender<()>,
    worker: thread::JoinHandle<Result<Vec<CpuSample>, String>>,
}

impl CpuMonitor {
    fn start() -> Result<Self, String> {
        let (stop, stop_rx) = mpsc::channel();
        let worker = thread::Builder::new()
            .name("rev7-cpu-monitor".to_owned())
            .spawn(move || {
                let start = Instant::now();
                let mut samples = vec![CpuSample {
                    elapsed: Duration::ZERO,
                    process_time: process_cpu_time()?,
                }];
                loop {
                    match stop_rx.recv_timeout(CPU_SAMPLE_INTERVAL) {
                        Err(RecvTimeoutError::Timeout) => {}
                        Ok(()) | Err(RecvTimeoutError::Disconnected) => {
                            samples.push(CpuSample {
                                elapsed: start.elapsed(),
                                process_time: process_cpu_time()?,
                            });
                            break;
                        }
                    }
                    samples.push(CpuSample {
                        elapsed: start.elapsed(),
                        process_time: process_cpu_time()?,
                    });
                }
                Ok(samples)
            })
            .map_err(|error| format!("Failed to spawn CPU monitor: {error}"))?;
        Ok(Self { stop, worker })
    }

    fn finish(self) -> Result<Option<f64>, String> {
        self.stop
            .send(())
            .map_err(|_| "CPU monitor stopped before the benchmark completed".to_owned())?;
        let samples = self
            .worker
            .join()
            .map_err(|_| "CPU monitor thread panicked".to_owned())??;
        Ok(cpu_percent_in_final_window(&samples, STEADY_SECONDS))
    }
}

/// Read aggregate user and system CPU time for the current process.
///
/// Returns:
///   CPU time consumed by all process threads in `s`.
#[cfg(unix)]
fn process_cpu_time() -> Result<Duration, String> {
    let mut usage = std::mem::MaybeUninit::<libc::rusage>::zeroed();
    // SAFETY: `usage` points to writable storage for exactly one `rusage`.
    // `getrusage` initializes it before the value is assumed initialized.
    let status = unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) };
    if status != 0 {
        return Err(format!(
            "getrusage failed while sampling host CPU: {}",
            std::io::Error::last_os_error()
        ));
    }
    // SAFETY: A zero status from `getrusage` guarantees initialization.
    let usage = unsafe { usage.assume_init() };
    timeval_duration(usage.ru_utime)?
        .checked_add(timeval_duration(usage.ru_stime)?)
        .ok_or_else(|| "Process CPU duration overflowed".to_owned())
}

#[cfg(not(unix))]
fn process_cpu_time() -> Result<Duration, String> {
    Err("Host CPU measurement requires a Unix-like operating system".to_owned())
}

#[cfg(unix)]
fn timeval_duration(value: libc::timeval) -> Result<Duration, String> {
    if value.tv_sec < 0 || !(0..1_000_000).contains(&value.tv_usec) {
        return Err("getrusage returned an invalid timeval".to_owned());
    }
    Ok(Duration::from_secs(value.tv_sec as u64) + Duration::from_micros(value.tv_usec as u64))
}

/// Compute process utilization over the tail of a sampled interval.
///
/// Args:
///   samples: Monotonic `(wall_time [s], process_time [s])` samples with shape
///     `(n_samples,)`.
///   window_seconds: Desired tail-window duration in `s`.
///
/// Returns:
///   Process utilization as percent of one logical CPU, or `None` if fewer
///   than two distinct samples are available. Values may exceed 100% because
///   the controller owns auxiliary worker threads.
fn cpu_percent_in_final_window(samples: &[CpuSample], window_seconds: u64) -> Option<f64> {
    let end = *samples.last()?;
    let cutoff = end
        .elapsed
        .saturating_sub(Duration::from_secs(window_seconds));
    let start = samples
        .iter()
        .rev()
        .find(|sample| sample.elapsed <= cutoff)
        .copied()
        .unwrap_or(*samples.first()?);
    let wall = end.elapsed.checked_sub(start.elapsed)?.as_secs_f64();
    let cpu = end
        .process_time
        .checked_sub(start.process_time)?
        .as_secs_f64();
    (wall > 0.0).then_some(100.0 * cpu / wall)
}

/// Run and analyze one rev7 rate benchmark point.
///
/// Args:
///   config: Point rate, duration, loop mode, and artifact paths.
///
/// Returns:
///   Tail-window loss, board-margin, host-CPU, and timestamp results.
pub fn run_benchmark(config: &BenchmarkConfig) -> Result<BenchmarkResult, String> {
    if !config.rate_hz.is_finite() || config.rate_hz <= 0.0 {
        return Err("Benchmark rate must be finite and positive".to_owned());
    }
    if config.run_seconds < MIN_RUN_SECONDS {
        return Err(format!(
            "Benchmark duration must be at least {MIN_RUN_SECONDS} s"
        ));
    }
    let period_ns = (1e9_f64 / config.rate_hz).round() as u32;
    if period_ns == 0 {
        return Err("Benchmark rate produces a zero period".to_owned());
    }
    let actual_rate_hz = 1e9_f64 / f64::from(period_ns);
    std::fs::create_dir_all(&config.output_dir)
        .map_err(|e| format!("Failed to create benchmark output directory: {e}"))?;

    let mut ctx = ControllerCtx::default();
    ctx.op_name = config.op_name.clone();
    ctx.op_dir = config.output_dir.clone();
    ctx.dt_ns = period_ns;
    ctx.loop_method = config.mode.loop_method();
    ctx.termination_criteria = Some(Termination::Timeout(Duration::from_secs(
        config.run_seconds,
    )));
    // A loss burst is benchmark data, not a reason to terminate the run early.
    ctx.controller_loss_of_contact_limit = u16::MAX;
    // Return the board to discovery shortly after each standalone point while
    // retaining enough cycles that loss bursts remain benchmark data.
    ctx.peripheral_loss_of_contact_limit = (actual_rate_hz * 2.0)
        .round()
        .clamp(1.0, f64::from(u16::MAX)) as u16;

    let mut controller = Controller::new(ctx);
    controller
        .add_peripheral(
            "p1",
            Box::new(DeimosDaqRev7 {
                serial_number: DAQ_SERIAL,
            }),
        )
        .map_err(|e| format!("Failed to add rev7 peripheral: {e}"))?;
    let csv: Box<dyn Dispatcher> = CsvDispatcher::new(32, Overflow::Error);
    controller.add_dispatcher(
        "benchmark",
        ChannelFilter::new(csv, CHANNELS.iter().map(ToString::to_string).collect()),
    );

    println!(
        "starting mode={}, requested_rate_hz={:.6}, actual_rate_hz={actual_rate_hz:.6}, duration_s={}",
        config.mode.name(),
        config.rate_hz,
        config.run_seconds,
    );
    let cpu_monitor = CpuMonitor::start()?;
    let run_result = controller.run(&None, None);
    let steady_host_cpu_percent = cpu_monitor.finish()?;
    run_result?;

    let csv_path = config.output_dir.join(format!("{}.csv", config.op_name));
    analyze(
        &csv_path,
        actual_rate_hz,
        period_ns,
        config.run_seconds,
        config.mode,
        steady_host_cpu_percent,
    )
}

/// Select a lower-tail percentile without interpolation.
///
/// Args:
///   values: Finite samples with shape `(n_cycles,)`.
///   numerator: Percentile-fraction numerator.
///   denominator: Nonzero percentile-fraction denominator.
///
/// Returns:
///   Lower-tail order statistic, or `NaN` for empty input or zero denominator.
pub fn lower_percentile(mut values: Vec<f64>, numerator: usize, denominator: usize) -> f64 {
    if values.is_empty() || denominator == 0 {
        return f64::NAN;
    }
    values.sort_unstable_by(f64::total_cmp);
    let index = (values.len() - 1) * numerator / denominator;
    values[index]
}

/// Analyze one completed raw controller capture.
///
/// Args:
///   path: Raw controller CSV path.
///   rate_hz: Actual configured publishing rate in `cycle/s`.
///   period_ns: Configured publishing period in `ns`.
///   run_seconds: Requested operating duration in `s`.
///   mode: Controller polling strategy.
///   steady_host_cpu_percent: Process utilization over the final window as
///     percent of one logical CPU.
///
/// Returns:
///   Validated loss, timing-margin, CPU, and acquisition-timestamp metrics.
fn analyze(
    path: &Path,
    rate_hz: f64,
    period_ns: u32,
    run_seconds: u64,
    mode: BenchmarkMode,
    steady_host_cpu_percent: Option<f64>,
) -> Result<BenchmarkResult, String> {
    let csv = load_csv(path)?;
    let indices = csv.required_channel_indices(CHANNELS)?;
    let [ctrl_margin_idx, board_margin_idx, loss_idx, sample_time_idx]: [usize; 4] = indices
        .try_into()
        .map_err(|_| "Unexpected benchmark channel count".to_owned())?;
    let rows = csv.rows();
    let expected_cycles =
        (run_seconds as u128 * 1_000_000_000_u128 / u128::from(period_ns)) as usize;
    let tolerance = (expected_cycles / 100).max(2);
    if rows.len() < expected_cycles.saturating_sub(tolerance) {
        return Err(format!(
            "Benchmark produced {} rows; expected approximately {expected_cycles}",
            rows.len()
        ));
    }
    let first_timestamp = rows
        .first()
        .ok_or_else(|| "Benchmark CSV contains no rows".to_owned())?
        .timestamp;
    let final_timestamp = rows.last().unwrap().timestamp;
    let steady_cutoff = final_timestamp.saturating_sub(STEADY_SECONDS as i64 * 1_000_000_000);
    let warmup_cutoff = first_timestamp.saturating_add(WARMUP_SECONDS as i64 * 1_000_000_000);
    let steady_rows = rows
        .iter()
        .filter(|row| row.timestamp >= steady_cutoff.max(warmup_cutoff))
        .collect::<Vec<_>>();
    if steady_rows.len() < 2 {
        return Err("Benchmark final-five-second window contains too few rows".to_owned());
    }

    let whole_drops = rows
        .iter()
        .filter(|row| row.channel_values[loss_idx] > 0.0)
        .count();
    let steady_drops = steady_rows
        .iter()
        .filter(|row| row.channel_values[loss_idx] > 0.0)
        .count();
    let steady_margins = steady_rows
        .iter()
        .map(|row| row.channel_values[board_margin_idx])
        .collect::<Vec<_>>();
    let steady_min_board_margin_us =
        steady_margins.iter().copied().fold(f64::INFINITY, f64::min) / 1e3;
    let steady_p01_board_margin_us = lower_percentile(steady_margins, 1, 100) / 1e3;

    let mut second_cycle_counts = vec![0; run_seconds as usize];
    let mut second_drop_counts = vec![0; run_seconds as usize];
    let mut max_loss_burst = 0.0_f64;
    let mut min_controller_margin_ns = f64::INFINITY;
    let mut min_board_margin_ns = f64::INFINITY;
    let mut sample_time_regressions = 0;
    let mut stale_sample_times_on_fresh_snapshots = 0;
    let mut min_positive_sample_step_ns = f64::INFINITY;
    let mut max_sample_step_ns = f64::NEG_INFINITY;
    let mut previous_sample_time_ns = None;
    for (row_index, row) in rows.iter().enumerate() {
        let values = &row.channel_values;
        let loss = values[loss_idx];
        max_loss_burst = max_loss_burst.max(loss);
        min_controller_margin_ns = min_controller_margin_ns.min(values[ctrl_margin_idx]);
        let board_margin = values[board_margin_idx];
        if row_index != 0 || board_margin != 0.0 {
            min_board_margin_ns = min_board_margin_ns.min(board_margin);
        }
        let elapsed = row.timestamp.saturating_sub(first_timestamp);
        let second = (elapsed / 1_000_000_000).clamp(0, run_seconds as i64 - 1) as usize;
        second_cycle_counts[second] += 1;
        second_drop_counts[second] += usize::from(loss > 0.0);
        if let Some(previous_sample_time_ns) = previous_sample_time_ns {
            let sample_step_ns = values[sample_time_idx] - previous_sample_time_ns;
            sample_time_regressions += usize::from(sample_step_ns < 0.0);
            stale_sample_times_on_fresh_snapshots +=
                usize::from(loss == 0.0 && sample_step_ns <= 0.0);
            if sample_step_ns > 0.0 {
                min_positive_sample_step_ns = min_positive_sample_step_ns.min(sample_step_ns);
            }
            max_sample_step_ns = max_sample_step_ns.max(sample_step_ns);
        }
        previous_sample_time_ns = Some(values[sample_time_idx]);
    }
    if sample_time_regressions != 0 || stale_sample_times_on_fresh_snapshots != 0 {
        return Err(format!(
            "Observed {sample_time_regressions} ADC timestamp regressions and \
             {stale_sample_times_on_fresh_snapshots} stale timestamps on fresh snapshots"
        ));
    }

    let board_margin_p01_ns = lower_percentile(
        rows.iter()
            .enumerate()
            .filter_map(|(index, row)| {
                let margin = row.channel_values[board_margin_idx];
                (index != 0 || margin != 0.0).then_some(margin)
            })
            .collect(),
        1,
        100,
    );
    if final_timestamp <= first_timestamp {
        return Err("Benchmark timestamps did not advance".to_owned());
    }

    Ok(BenchmarkResult {
        mode: mode.name().to_owned(),
        rate_hz,
        period_ns,
        run_seconds,
        row_count: rows.len(),
        expected_cycle_count: expected_cycles,
        steady_row_count: steady_rows.len(),
        whole_drop_rate: whole_drops as f64 / rows.len() as f64,
        steady_final_5s_drop_rate: steady_drops as f64 / steady_rows.len() as f64,
        max_loss_burst,
        min_controller_margin_ns,
        min_board_margin_ns,
        board_margin_p01_ns,
        steady_min_board_margin_us,
        steady_p01_board_margin_us,
        steady_host_cpu_percent,
        sample_time_regressions,
        stale_sample_times_on_fresh_snapshots,
        min_positive_sample_step_ns,
        max_sample_step_ns,
        raw_csv: path.display().to_string(),
        second_cycle_counts,
        second_drop_counts,
    })
}

#[cfg(test)]
mod tests {
    use super::{
        BenchmarkConfig, BenchmarkMode, CpuSample, MIN_RUN_SECONDS, cpu_percent_in_final_window,
        lower_percentile, run_benchmark,
    };
    use std::{path::PathBuf, time::Duration};

    #[test]
    fn benchmark_requires_warmup_before_the_steady_window() {
        let error = run_benchmark(&BenchmarkConfig {
            rate_hz: 1_000.0,
            run_seconds: MIN_RUN_SECONDS - 1,
            mode: BenchmarkMode::Performant,
            op_name: "too_short".to_owned(),
            output_dir: PathBuf::from("unused"),
        })
        .unwrap_err();

        assert_eq!(
            error,
            format!("Benchmark duration must be at least {MIN_RUN_SECONDS} s")
        );
    }

    #[test]
    fn lower_percentile_selects_the_lower_tail_order_statistic() {
        let values = (0..=100).rev().map(|value| value as f64).collect();
        assert_eq!(lower_percentile(values, 1, 100), 1.0);
        assert!(lower_percentile(Vec::new(), 1, 100).is_nan());
    }

    #[test]
    fn cpu_percent_uses_only_the_requested_tail_window() {
        let samples = (0..=10)
            .map(|second| CpuSample {
                elapsed: Duration::from_secs(second),
                process_time: Duration::from_secs_f64(if second <= 5 {
                    second as f64
                } else {
                    5.0 + 0.25 * (second - 5) as f64
                }),
            })
            .collect::<Vec<_>>();
        assert_eq!(cpu_percent_in_final_window(&samples, 5), Some(25.0));
    }
}
