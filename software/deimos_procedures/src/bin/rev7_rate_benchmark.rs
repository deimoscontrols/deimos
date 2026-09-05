//! Repeatable high-rate rev7 hardware regression benchmark.
//!
//! Run with `cargo run -p deimos_procedures --release --bin rev7_rate_benchmark`.

mod rev7_rate_benchmark_common;

use std::{env, path::PathBuf};

use deimos_shared::peripherals::deimos_daq_rev7::DEIMOS_MAX_CYCLE_RATE_HZ;
use rev7_rate_benchmark_common::{BenchmarkConfig, BenchmarkMode, run_benchmark};

const DEFAULT_RATE_HZ: u32 = DEIMOS_MAX_CYCLE_RATE_HZ;
const DEFAULT_RUN_SECONDS: u64 = 10;
const OP_NAME_PREFIX: &str = "rev7_rate_benchmark";

fn main() -> Result<(), String> {
    let rate_hz = env_value("DEIMOS_BENCH_RATE_HZ", DEFAULT_RATE_HZ)?;
    let run_seconds = env_value("DEIMOS_BENCH_SECONDS", DEFAULT_RUN_SECONDS)?;
    if rate_hz == 0 || run_seconds == 0 {
        return Err("Benchmark rate and duration must both be nonzero".to_owned());
    }
    let op_name = format!("{OP_NAME_PREFIX}_{rate_hz}hz");
    let result = run_benchmark(&BenchmarkConfig {
        rate_hz: f64::from(rate_hz),
        run_seconds,
        mode: BenchmarkMode::Performant,
        op_name,
        output_dir: PathBuf::from("./target/rev7_rate_benchmark"),
    })?;
    result.print_detailed();
    Ok(())
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
