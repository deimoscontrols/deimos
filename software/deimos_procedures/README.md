# Deimos procedures

Production calibration, hardware validation, benchmarks, and uncertainty analysis
for Deimos DAQs. Library usage examples live in [deimos/examples](../deimos/examples).

Run procedures from the workspace root:

```sh
cargo run -p deimos_procedures --release --bin rev7_calibration -- [OPTIONS]
cargo run -p deimos_procedures --bin rev7_modbus_client -- IP[:PORT]
cargo run -p deimos_procedures --bin rev7_modbus_test -- [SUITE] [IP[:PORT]]
cargo run -p deimos_procedures --release --bin rev7_rate_benchmark
cargo run -p deimos_procedures --release --bin rev7_rate_sweep
cargo run -p deimos_procedures --bin rev7_uncertainty
```

Replace bracketed arguments with the desired values or omit optional arguments.
See each binary's source for its configuration and hardware setup. The uncertainty
analysis runs offline and writes plots to the website assets directory.

The calibration operator console configuration is
[rev7_calibration_console.toml](rev7_calibration_console.toml):

```sh
cargo run -p deimos_console -- --config software/deimos_procedures/rev7_calibration_console.toml
```

`cargo test -p deimos_procedures` runs the procedures' unit tests without running
their hardware entrypoints. `cargo build -p deimos_procedures --bins` builds all
procedure executables.
