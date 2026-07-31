//! Repeatable high-rate rev7 hardware regression benchmark.

use std::{env, path::PathBuf, time::Duration};

use deimos::{
    ChannelFilter, Controller, CsvDispatcher, Dispatcher, LoopMethod, Overflow, Termination,
    controller::context::ControllerCtx, dispatcher::load_csv, peripheral::DeimosDaqRev7,
};
use deimos_shared::peripherals::deimos_daq_rev7::DEIMOS_MAX_CYCLE_RATE_HZ;

const DEFAULT_RATE_HZ: u32 = DEIMOS_MAX_CYCLE_RATE_HZ;
const DEFAULT_RUN_SECONDS: u64 = 10;
const DAQ_SERIAL: u64 = 3;
const OP_NAME_PREFIX: &str = "rev7_rate_benchmark";

fn main() -> Result<(), String> {
    let rate_hz = env_value("DEIMOS_BENCH_RATE_HZ", DEFAULT_RATE_HZ)?;
    let run_seconds = env_value("DEIMOS_BENCH_SECONDS", DEFAULT_RUN_SECONDS)?;
    if rate_hz == 0 || run_seconds == 0 {
        return Err("Benchmark rate and duration must both be nonzero".to_owned());
    }
    let op_name = format!("{OP_NAME_PREFIX}_{rate_hz}hz");
    let output_dir = PathBuf::from("./target/rev7_rate_benchmark");
    std::fs::create_dir_all(&output_dir)
        .map_err(|e| format!("Failed to create benchmark output directory: {e}"))?;

    let mut ctx = ControllerCtx::default();
    ctx.op_name = op_name.clone();
    ctx.op_dir = output_dir.clone();
    ctx.dt_ns = 1_000_000_000 / rate_hz;
    ctx.loop_method = LoopMethod::Performant;
    ctx.termination_criteria = Some(Termination::Timeout(Duration::from_secs(run_seconds)));
    // A loss burst is benchmark data, not a reason to terminate the run early.
    ctx.controller_loss_of_contact_limit = u16::MAX;
    // Return the board to discovery shortly after each standalone sweep point,
    // while retaining enough cycles that loss bursts remain benchmark data.
    ctx.peripheral_loss_of_contact_limit =
        rate_hz.saturating_mul(2).min(u32::from(u16::MAX)) as u16;
    // Exercise the normal operating path, which requires and applies the
    // calibration embedded in rev7 firmware.
    ctx.use_no_calibrations = false;

    let mut controller = Controller::new(ctx);
    controller
        .add_peripheral(
            "p1",
            Box::new(DeimosDaqRev7 {
                serial_number: DAQ_SERIAL,
            }),
        )
        .map_err(|e| format!("Failed to add rev7 peripheral: {e}"))?;

    let channels = vec![
        "ctrl.cycle_time_margin_ns".to_owned(),
        "p1.metrics.cycle_time_ns".to_owned(),
        "p1.metrics.cycle_time_margin_ns".to_owned(),
        "p1.metrics.loss_of_contact_counter".to_owned(),
        "p1.sample_time_ns".to_owned(),
    ];
    let csv: Box<dyn Dispatcher> = CsvDispatcher::new(16, Overflow::Error);
    controller.add_dispatcher("benchmark", ChannelFilter::new(csv, channels));

    controller.run(&None, None)?;

    let csv_path = output_dir.join(format!("{op_name}.csv"));
    report(&csv_path, rate_hz, run_seconds)
}

/// Read one positive integer benchmark override from the environment.
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

/// Selects a lower-tail percentile without interpolation.
///
/// Args:
///   values: Finite benchmark samples with shape `(n_cycles,)`.
///   numerator: Percentile fraction numerator.
///   denominator: Nonzero percentile fraction denominator.
///
/// Returns:
///   The lower-tail order statistic, or `NaN` for empty input or a zero
///   denominator.
fn lower_percentile(mut values: Vec<f64>, numerator: usize, denominator: usize) -> f64 {
    if values.is_empty() || denominator == 0 {
        return f64::NAN;
    }
    values.sort_unstable_by(f64::total_cmp);
    let index = (values.len() - 1) * numerator / denominator;
    values[index]
}

fn report(path: &std::path::Path, rate_hz: u32, run_seconds: u64) -> Result<(), String> {
    let csv = load_csv(path)?;
    let indices = csv.required_channel_indices([
        "ctrl.cycle_time_margin_ns",
        "p1.metrics.cycle_time_ns",
        "p1.metrics.cycle_time_margin_ns",
        "p1.metrics.loss_of_contact_counter",
        "p1.sample_time_ns",
    ])?;
    let [
        ctrl_margin_idx,
        cycle_time_idx,
        board_margin_idx,
        loss_idx,
        sample_time_idx,
    ]: [usize; 5] = indices
        .try_into()
        .map_err(|_| "Unexpected benchmark channel count".to_owned())?;

    let dt_ns = i64::from(1_000_000_000 / rate_hz);
    let expected_cycles = rate_hz as usize * run_seconds as usize;
    let rows = csv.rows();
    // Dispatch begins after the first in-order response, so a handful of setup
    // cycles may precede the measured window even though the timed loop is 10 s.
    if rows.len() < expected_cycles.saturating_sub(((rate_hz / 100) as usize).max(2)) {
        return Err(format!(
            "Benchmark produced {} rows; expected approximately {expected_cycles}",
            rows.len()
        ));
    }

    let mut buckets = vec![0usize; run_seconds as usize];
    let mut bucket_drops = vec![0usize; run_seconds as usize];
    let mut total_drops = 0usize;
    let mut max_burst = 0.0_f64;
    let mut min_ctrl_margin = f64::INFINITY;
    let mut min_board_margin = f64::INFINITY;
    let mut min_cycle_time = f64::INFINITY;
    let mut sample_time_regressions = 0_usize;
    let mut stale_sample_times_on_fresh_snapshots = 0_usize;
    let mut min_sample_step_ns = f64::INFINITY;
    let mut max_sample_step_ns = f64::NEG_INFINITY;
    let mut previous_snapshot = None;
    let start_timestamp = rows.first().map(|row| row.timestamp).unwrap_or(0);

    for (row_index, row) in rows.iter().enumerate() {
        let values = &row.channel_values;
        let loss = values[loss_idx];
        let dropped = loss > 0.0;
        total_drops += usize::from(dropped);
        max_burst = max_burst.max(loss);
        min_ctrl_margin = min_ctrl_margin.min(values[ctrl_margin_idx]);
        // The first snapshot has no completed predecessor cycle and may carry
        // the packet-default zero. Depending on synchronization, that snapshot
        // can be consumed before dispatch begins; retain a nonzero first row.
        let board_margin = values[board_margin_idx];
        if row_index != 0 || board_margin != 0.0 {
            min_board_margin = min_board_margin.min(board_margin);
        }
        min_cycle_time = min_cycle_time.min(values[cycle_time_idx]);
        let sample_time_ns = values[sample_time_idx];
        if let Some((previous_cycle_time_ns, previous_sample_time_ns)) = previous_snapshot {
            let step_ns = sample_time_ns - previous_sample_time_ns;
            sample_time_regressions += usize::from(step_ns < 0.0);
            stale_sample_times_on_fresh_snapshots +=
                usize::from(values[cycle_time_idx] > previous_cycle_time_ns && step_ns <= 0.0);
            if step_ns > 0.0 {
                min_sample_step_ns = min_sample_step_ns.min(step_ns);
            }
            max_sample_step_ns = max_sample_step_ns.max(step_ns);
        }
        previous_snapshot = Some((values[cycle_time_idx], sample_time_ns));

        let elapsed = row.timestamp.saturating_sub(start_timestamp);
        let bucket = (elapsed / 1_000_000_000).clamp(0, run_seconds as i64 - 1) as usize;
        buckets[bucket] += 1;
        bucket_drops[bucket] += usize::from(dropped);
    }

    let steady_seconds = run_seconds.min(5);
    let steady_start = rows
        .len()
        .saturating_sub((rate_hz as usize) * steady_seconds as usize);
    let steady_rows = &rows[steady_start..];
    let steady_drops = steady_rows
        .iter()
        .filter(|row| row.channel_values[loss_idx] > 0.0)
        .count();
    let board_margin_p01 = lower_percentile(
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
    let steady_board_margin_min = steady_rows
        .iter()
        .map(|row| row.channel_values[board_margin_idx])
        .fold(f64::INFINITY, f64::min);
    let steady_board_margin_p01 = lower_percentile(
        steady_rows
            .iter()
            .map(|row| row.channel_values[board_margin_idx])
            .collect(),
        1,
        100,
    );

    println!("rev7 SN{DAQ_SERIAL} {rate_hz} Hz / {run_seconds} s benchmark");
    println!(
        "period_ns={dt_ns}, rows={}, expected={expected_cycles}",
        rows.len()
    );
    for second in 0..run_seconds as usize {
        let rate = if buckets[second] == 0 {
            f64::NAN
        } else {
            bucket_drops[second] as f64 / buckets[second] as f64
        };
        println!(
            "second={second}, cycles={}, dropped={}, drop_rate={rate:.8}",
            buckets[second], bucket_drops[second]
        );
    }
    println!(
        "whole_drop_rate={:.8}, steady_final_5s_drop_rate={:.8}, max_burst={max_burst:.0}",
        total_drops as f64 / rows.len() as f64,
        steady_drops as f64 / steady_rows.len() as f64,
    );
    println!(
        "min_controller_margin_ns={min_ctrl_margin:.0}, min_board_margin_ns={min_board_margin:.0}, board_margin_p01_ns={board_margin_p01:.0}, steady_min_board_margin_ns={steady_board_margin_min:.0}, steady_board_margin_p01_ns={steady_board_margin_p01:.0}, min_board_cycle_time_ns={min_cycle_time:.0}"
    );
    println!(
        "sample_time_regressions={sample_time_regressions}, stale_sample_times_on_fresh_snapshots={stale_sample_times_on_fresh_snapshots}, min_positive_sample_step_ns={min_sample_step_ns:.0}, max_sample_step_ns={max_sample_step_ns:.0}"
    );

    if sample_time_regressions != 0 || stale_sample_times_on_fresh_snapshots != 0 {
        return Err(format!(
            "Observed {sample_time_regressions} ADC timestamp regressions and \
             {stale_sample_times_on_fresh_snapshots} stale timestamps on fresh snapshots"
        ));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::lower_percentile;

    #[test]
    fn lower_percentile_selects_the_lower_tail_order_statistic() {
        let values = (0..=100).rev().map(|value| value as f64).collect();
        assert_eq!(lower_percentile(values, 1, 100), 1.0);
        assert!(lower_percentile(Vec::new(), 1, 100).is_nan());
    }
}
