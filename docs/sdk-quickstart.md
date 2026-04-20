# SDK quickstart

Deimos ships two SDKs with the same feature set:

- **Rust** — [`deimos`](https://crates.io/crates/deimos) on crates.io
- **Python** — [`deimos-daq`](https://pypi.org/project/deimos-daq/) on PyPI

The Python package wraps the Rust implementation, so formulating a controller in Python and running it gives the same performance and behavior as writing it in Rust directly.

You don't need any Deimos hardware to try either SDK. Deimos has a **HOOTL** (Hardware-Out-Of-The-Loop) mode that runs an end-to-end peripheral simulation in-process via a thread-channel socket.

## First run: HOOTL from Rust

Clone the repo and run:

```sh
just hootl
```

This builds and runs [`software/deimos/examples/hootl_lifecycle.rs`](../software/deimos/examples/hootl_lifecycle.rs), which spins up a simulated peripheral, runs it for 3 seconds, and writes CSV output to a temp directory. You should see log lines in this order:

```
INFO  Binding peripherals.
INFO  Configuring peripherals.
INFO  All peripherals acknowledged configuration.
INFO  Entering control loop.
```

Those four messages confirm the controller has walked through the states `Connecting → Binding → Configuring → OperatingRoundtrip`.

Two other HOOTL examples ship with the repo and are also exposed through the `Justfile`:

- `just hootl-butter-reset` — verifies that an IIR calc's filter state is reset between back-to-back runs (bit-exact leading output on both runs).
- `just hootl-csv-fallback` — exercises the CSV dispatcher as a fallback when a database dispatcher is misconfigured.

## First run: HOOTL from Python

The Python equivalent lives at [`software/deimos/examples/hootl.py`](../software/deimos/examples/hootl.py). It builds a controller, attaches three HOOTL drivers (thread-channel, unix socket where supported, UDP loopback), and runs for 2 seconds in both loop modes. After installing `deimos-daq`, `python hootl.py` from that directory reproduces the run.

## Loop modes

The controller can be configured for one of two loop strategies. Pick based on whether you're running a tight control loop or sharing the machine with other work.

| Mode | Max rate | Scheduling | CPU usage | When to use |
| --- | --- | --- | --- | --- |
| **Performant** | up to ~20 kHz | Busy-wait on sockets, nanosecond timing | Pins ~100 % of one core | Realtime control, tight roundtrip deadlines |
| **Efficient** | up to ~50 Hz | OS-scheduled polling | ~5 % of one core | Long-running data logging, background collection |

## Run modes

The controller exposes two ways to start a run:

- **Blocking** (`Controller::run()` / `.run()`) — rigorous and well-posed. The call returns when the run terminates.
- **Non-blocking** (`Controller::run_nonblocking()` / `.run_nonblocking()`) — returns a handle you can use to read live values, write manual inputs, and issue a stop signal from another thread. Useful for interactive or scripted control from a REPL/notebook.

## The four building-block traits

The controller is built from four Rust traits. Any custom extension slots in by implementing one of them — the [top-level README](../README.md) lists the concrete implementations shipped in-tree.

| Trait | Role |
| --- | --- |
| **Peripheral** | How the controller represents a piece of hardware |
| **Socket** | Transport between controller and peripheral (UDP, unix socket, thread channel, …) |
| **Calc** | Realtime math and control logic (PID, Butterworth, sequence state machine, table lookups) |
| **Dispatcher** | Where data goes (CSV, DataFrame, TimescaleDB, latest-value) |

## Where to read next

- Repo-level [`README.md`](../README.md) — exhaustive tables of peripherals, sockets, dispatchers, calc functions.
- [`signal-pipeline.md`](signal-pipeline.md) — what happens to a sample between the ADC and your database.
- [`hardware.md`](hardware.md) — the physical boards and sensor frontends.
- [`CLAUDE.md`](../CLAUDE.md) — build/test commands and HOOTL invocations used in CI.
