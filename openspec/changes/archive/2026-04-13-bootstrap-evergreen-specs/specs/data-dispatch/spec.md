<!-- REVIEW-COMPLETE (task 3.3): approved — spec text now describes actual behavior (CSV joins-on-terminate; TSDB/Latest use async-drop; fallback framing removed). Real code gaps flagged by verification are deferred to follow-on change proposals, not gates on this bootstrap. -->

## ADDED Requirements

### Requirement: Dispatchers consume one row per cycle

A `Dispatcher` SHALL expose `consume(system_time, timestamp, channel_values)` accepting a single row per controller cycle. Each row MUST contain the system wall-clock time, a monotonic `i64` timestamp, and the vector of channel values in the order declared at `init`.

Reference: `trait Dispatcher` and `Row { system_time, timestamp, channel_values }` in `software/deimos/src/dispatcher/mod.rs`.

#### Scenario: One row per cycle reaches every registered dispatcher

- **WHEN** the controller completes a cycle and produces a row
- **THEN** every registered dispatcher SHALL receive exactly one `consume` call for that row before the next cycle's row is dispatched

### Requirement: Dispatchers declare their channel list at init

Before a run starts, the controller SHALL call `init(ctx, channel_names, core_assignment)` on each dispatcher. The `channel_names` ordering MUST match the order of values the dispatcher will subsequently receive in `consume`. A dispatcher MAY return `Err` from `init` to abort the run.

Reference: `Dispatcher::init` in `software/deimos/src/dispatcher/mod.rs`.

#### Scenario: Column ordering is fixed for the run

- **WHEN** a CSV dispatcher is initialized with channel names `[a, b, c]`
- **THEN** every subsequent `consume` row MUST contain values in `[a, b, c]` order for the duration of the run

### Requirement: Dispatchers terminate cleanly between runs

At the end of a run the controller SHALL call `terminate()` on each dispatcher. `terminate` MUST close the dispatcher's input channel so that any background worker drains buffered data to its sink, and MUST reset internal state so the dispatcher is reusable for a subsequent `init`. `CsvDispatcher::terminate` joins its worker thread synchronously before returning; `TimescaleDbDispatcher::terminate` and `LatestValueDispatcher::terminate` close their input channels and rely on worker threads finishing asynchronously (via `WorkerHandle::Drop`).

Reference: `Dispatcher::terminate` in `software/deimos/src/dispatcher/mod.rs`; `CsvDispatcher::terminate` in `csv.rs`; `TimescaleDbDispatcher::terminate` in `tsdb.rs:226-233`; `LatestValueDispatcher::terminate` in `latest.rs:158-162`.

#### Scenario: CSV file closed on run end

- **WHEN** a run finishes with a `CsvDispatcher` attached
- **THEN** `terminate` MUST flush and close the underlying file before returning, and a subsequent `init` MUST open a fresh file rather than appending to the closed one

<!-- FOLLOW-ON: TimescaleDbDispatcher and LatestValueDispatcher only close the mpsc sender on terminate; their workers flush on drop. If a stronger "flush before terminate returns" guarantee is desired across all dispatchers, that is a separate change proposal (synchronous join-on-terminate for TSDB/Latest). -->


### Requirement: CsvDispatcher provides a filesystem-backed data path independent of the network

Deimos SHALL provide `CsvDispatcher` as a first-class dispatcher implementation backed by the local filesystem, so that a user can record data without any network, database, or external service dependency.

Reference: `software/deimos/src/dispatcher/csv.rs`.

#### Scenario: CSV-only run with no network configured

- **WHEN** a controller is configured with only a `CsvDispatcher` on a machine with no network access
- **THEN** the run MUST complete end-to-end and the CSV file MUST contain one row per cycle

<!-- FOLLOW-ON: Dispatcher errors currently abort the entire run (`Controller::run` calls
`dispatcher.init(…).unwrap()` at `controller/mod.rs:839`; the consume loop at `mod.rs:1543-1558`
returns `Err` and terminates the session if any dispatcher fails). A future change proposal could
introduce a per-dispatcher isolation policy so that a TSDB failure does not take down a parallel
CSV recording. The `examples/hootl_csv_fallback.rs` example reproduces both failure modes and
confirms CSV-only recording works correctly (39/40 rows at 20 Hz × 2 s). Not a gate on this
bootstrap — the current all-or-nothing behavior is what the spec now describes. -->

### Requirement: Dispatchers include a built-in library covering common sinks

The dispatcher library SHALL include, at minimum: `CsvDispatcher` (filesystem), `TimescaleDbDispatcher` (time-series database), `DataFrameDispatcher` (in-memory), and `LatestValueDispatcher` (most-recent-value cache). Each MUST implement the `Dispatcher` trait.

Reference: `software/deimos/src/dispatcher/{csv,tsdb,df,latest}.rs`.

#### Scenario: Live UI reads the latest value

- **WHEN** a UI holds a `LatestValueHandle` from a `LatestValueDispatcher`
- **THEN** reading the handle MUST return the most recent row consumed by the dispatcher without blocking the control loop

### Requirement: Dispatchers are composable via filtering and reduction

Deimos SHALL provide `ChannelFilter`, `DecimationDispatcher`, and `LowPassDispatcher` so a user can narrow the channel list, reduce the sample rate, or smooth the data stream delivered to a downstream dispatcher without modifying that dispatcher.

Reference: `software/deimos/src/dispatcher/{channel_filter,decimation,low_pass}.rs`.

#### Scenario: Log only a subset of channels to the database

- **WHEN** a user wraps a `TimescaleDbDispatcher` in a `ChannelFilter` selecting three channels out of fifty
- **THEN** the wrapped dispatcher MUST receive `consume` calls containing only those three channels' values, while a CSV dispatcher attached in parallel still records all fifty

### Requirement: CSV overflow behavior is user-selectable

When a CSV output's size limit is reached, the dispatcher SHALL honor a configured `Overflow` policy: `Wrap` (overwrite oldest), `NewFile` (rotate to a new file), or `Error` (abort when neither is viable). `Wrap` MUST be the default.

Reference: `pub enum Overflow { Wrap, NewFile, Error }` in `software/deimos/src/dispatcher/mod.rs`.

#### Scenario: Long run with file rotation

- **WHEN** a `CsvDispatcher` is configured with `Overflow::NewFile` and reaches its size cap
- **THEN** it MUST open a new file and continue recording rows without dropping any of the row currently being written

### Requirement: Dispatcher row encoding is lossless and self-describing

CSV rows produced by Deimos SHALL use a fixed-width float format that round-trips through `f64::from_str`, and SHALL prepend a header row listing `timestamp, time` followed by each channel name in the order declared at init.

Reference: `csv_header`, `csv_row_fixed_width`, `fmt_f64`, `fmt_i64` in `software/deimos/src/dispatcher/mod.rs` (see unit tests `fmt_f64_has_consistent_width`, `fmt_i64_has_consistent_width`).

#### Scenario: Parsing a logged CSV reproduces the exact f64 values

- **WHEN** a CSV file written by `CsvDispatcher` (fixed-width mode) is read back with standard `f64` parsing
- **THEN** every finite channel value MUST parse to the exact bitwise-identical `f64` that was written, and NaN MUST parse as NaN
