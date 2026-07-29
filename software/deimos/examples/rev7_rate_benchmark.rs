//! Repeatable high-rate rev7 hardware regression benchmark.

use std::{path::PathBuf, time::Duration};

use deimos::{
    ChannelFilter, Controller, CsvDispatcher, Dispatcher, LoopMethod, Overflow, Termination,
    controller::context::ControllerCtx, dispatcher::load_csv, peripheral::DeimosDaqRev7,
};

const RATE_HZ: u32 = 5_000;
const RUN_SECONDS: u64 = 10;
const DAQ_SERIAL: u64 = 3;
const OP_NAME: &str = "rev7_rate_benchmark";

fn main() -> Result<(), String> {
    let output_dir = PathBuf::from("./target/rev7_rate_benchmark");
    std::fs::create_dir_all(&output_dir)
        .map_err(|e| format!("Failed to create benchmark output directory: {e}"))?;

    let mut ctx = ControllerCtx::default();
    ctx.op_name = OP_NAME.to_owned();
    ctx.op_dir = output_dir.clone();
    ctx.dt_ns = 1_000_000_000 / RATE_HZ;
    ctx.loop_method = LoopMethod::Performant;
    ctx.termination_criteria = Some(Termination::Timeout(Duration::from_secs(RUN_SECONDS)));
    // A loss burst is benchmark data, not a reason to terminate the run early.
    ctx.controller_loss_of_contact_limit = u16::MAX;
    ctx.peripheral_loss_of_contact_limit = u16::MAX;
    ctx.use_no_calibrations = true;

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
    ];
    let csv: Box<dyn Dispatcher> = CsvDispatcher::new(16, Overflow::Error);
    controller.add_dispatcher("benchmark", ChannelFilter::new(csv, channels));

    controller.run(&None, None)?;

    let csv_path = output_dir.join(format!("{OP_NAME}.csv"));
    report(&csv_path, ctx_dt_ns())
}

fn ctx_dt_ns() -> i64 {
    (1_000_000_000 / RATE_HZ) as i64
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

fn report(path: &std::path::Path, dt_ns: i64) -> Result<(), String> {
    let csv = load_csv(path)?;
    let indices = csv.required_channel_indices([
        "ctrl.cycle_time_margin_ns",
        "p1.metrics.cycle_time_ns",
        "p1.metrics.cycle_time_margin_ns",
        "p1.metrics.loss_of_contact_counter",
    ])?;
    let [ctrl_margin_idx, cycle_time_idx, board_margin_idx, loss_idx]: [usize; 4] = indices
        .try_into()
        .map_err(|_| "Unexpected benchmark channel count".to_owned())?;

    let expected_cycles = RATE_HZ as usize * RUN_SECONDS as usize;
    let rows = csv.rows();
    // Dispatch begins after the first in-order response, so a handful of setup
    // cycles may precede the measured window even though the timed loop is 10 s.
    if rows.len() < expected_cycles.saturating_sub((RATE_HZ / 100) as usize) {
        return Err(format!(
            "Benchmark produced {} rows; expected approximately {expected_cycles}",
            rows.len()
        ));
    }

    let mut buckets = [0usize; RUN_SECONDS as usize];
    let mut bucket_drops = [0usize; RUN_SECONDS as usize];
    let mut total_drops = 0usize;
    let mut max_burst = 0.0_f64;
    let mut min_ctrl_margin = f64::INFINITY;
    let mut min_board_margin = f64::INFINITY;
    let mut min_cycle_time = f64::INFINITY;
    let start_timestamp = rows.first().map(|row| row.timestamp).unwrap_or(0);

    for row in rows {
        let values = &row.channel_values;
        let loss = values[loss_idx];
        let dropped = loss > 0.0;
        total_drops += usize::from(dropped);
        max_burst = max_burst.max(loss);
        min_ctrl_margin = min_ctrl_margin.min(values[ctrl_margin_idx]);
        min_board_margin = min_board_margin.min(values[board_margin_idx]);
        min_cycle_time = min_cycle_time.min(values[cycle_time_idx]);

        let elapsed = row.timestamp.saturating_sub(start_timestamp);
        let bucket = (elapsed / 1_000_000_000).clamp(0, RUN_SECONDS as i64 - 1) as usize;
        buckets[bucket] += 1;
        bucket_drops[bucket] += usize::from(dropped);
    }

    let steady_start = rows.len().saturating_sub((RATE_HZ as usize) * 5);
    let steady_rows = &rows[steady_start..];
    let steady_drops = steady_rows
        .iter()
        .filter(|row| row.channel_values[loss_idx] > 0.0)
        .count();
    let board_margin_p01 = lower_percentile(
        rows.iter()
            .map(|row| row.channel_values[board_margin_idx])
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

    println!("rev7 SN{DAQ_SERIAL} {RATE_HZ} Hz / {RUN_SECONDS} s benchmark");
    println!(
        "period_ns={dt_ns}, rows={}, expected={expected_cycles}",
        rows.len()
    );
    for second in 0..RUN_SECONDS as usize {
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
