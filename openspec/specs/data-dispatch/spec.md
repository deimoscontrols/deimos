# data-dispatch Specification

## Purpose

Defines how per-cycle rows reach external sinks: the dispatcher trait contract, built-in sinks (CSV, TimescaleDB, in-memory dataframe, latest-value), composable wrappers (channel filter, decimation, low-pass), and the flush/terminate semantics that govern clean shutdown.

## Requirements
### Requirement: Dispatchers consume one row per cycle

A dispatcher SHALL accept a single row per controller cycle. Each row MUST contain the system wall-clock time, a monotonic integer timestamp, and the vector of channel values in the order declared at init. Every registered dispatcher SHALL receive exactly one row per cycle before the next cycle's row is dispatched.

#### Scenario: Every registered dispatcher receives one row per cycle

- **WHEN** the controller completes a cycle with multiple dispatchers registered
- **THEN** each dispatcher MUST receive exactly one row for that cycle before the next cycle's row is dispatched

### Requirement: Dispatchers declare their channel list at init

Before a run starts, the controller SHALL call an init operation on each dispatcher passing the ordered channel name list. The channel-name ordering declared at init MUST match the order of values the dispatcher subsequently receives, and that ordering is fixed for the duration of the run. A dispatcher MAY return an error from init to abort the run.

#### Scenario: Column ordering is fixed for the run

- **WHEN** a dispatcher is initialized with a channel-name list and the run proceeds
- **THEN** every subsequent row MUST deliver values in that declared order for the entire run

### Requirement: Dispatchers terminate cleanly between runs

At the end of a run the controller SHALL call terminate on each dispatcher. Terminate MUST close the dispatcher's input channel so that any background worker drains buffered data to its sink, and MUST reset internal state so the dispatcher is reusable for a subsequent init.

Flush semantics differ by sink: the CSV dispatcher joins its worker synchronously before terminate returns, while the time-series-database and latest-value dispatchers close their input channels and rely on workers finishing asynchronously when their handles are dropped.

#### Scenario: CSV file closed on run end and re-init opens fresh file

- **WHEN** a run finishes with a CSV dispatcher attached and the dispatcher is re-initialized for a new run
- **THEN** terminate MUST flush and close the underlying file before returning, and the subsequent init MUST open a fresh file rather than appending to the closed one

<!-- FOLLOW-ON: TimescaleDbDispatcher and LatestValueDispatcher only close the mpsc sender on terminate; their workers flush on drop. If a stronger "flush before terminate returns" guarantee is desired across all dispatchers, that is a separate change proposal (synchronous join-on-terminate for TSDB/Latest). -->

### Requirement: CSV provides a filesystem-backed data path independent of the network

Deimos SHALL provide a first-class CSV dispatcher backed by the local filesystem, so that a user can record data without any network, database, or external service dependency.

#### Scenario: CSV-only run with no network configured

- **WHEN** a controller is configured with only a CSV dispatcher on a machine with no network access
- **THEN** the run MUST complete end-to-end and the CSV file MUST contain one row per cycle

<!-- FOLLOW-ON: Dispatcher errors currently abort the entire run (`Controller::run` calls
`dispatcher.init(…).unwrap()` at `controller/mod.rs:839`; the consume loop at `mod.rs:1543-1558`
returns `Err` and terminates the session if any dispatcher fails). A future change proposal could
introduce a per-dispatcher isolation policy so that a TSDB failure does not take down a parallel
CSV recording. The `examples/hootl_csv_fallback.rs` example reproduces both failure modes and
confirms CSV-only recording works correctly (39/40 rows at 20 Hz × 2 s). Not a gate on this
bootstrap — the current all-or-nothing behavior is what the spec now describes. -->

### Requirement: Dispatchers include a built-in library covering common sinks

The dispatcher library SHALL include, at minimum: a filesystem CSV dispatcher, a time-series-database dispatcher, an in-memory dataframe dispatcher, and a latest-value cache dispatcher.

#### Scenario: Live UI reads the latest value without blocking the control loop

- **WHEN** a UI holds a handle from the latest-value dispatcher
- **THEN** reading the handle MUST return the most recent row consumed by the dispatcher without blocking the control loop

### Requirement: Dispatchers are composable via filtering and reduction

Deimos SHALL provide wrapper dispatchers for channel filtering, decimation, and low-pass smoothing so a user can narrow the channel list, reduce the sample rate, or smooth the data stream delivered to a downstream dispatcher without modifying that dispatcher. Wrappers compose in parallel: one branch of the dispatch tree MAY receive a filtered or decimated view while a sibling branch records the full stream.

#### Scenario: Filtered branch and full branch run in parallel

- **WHEN** a user wraps one dispatcher in a channel filter selecting three channels out of fifty, while attaching a second dispatcher in parallel with no wrapper
- **THEN** the wrapped dispatcher MUST receive only those three channels' values, while the parallel dispatcher still records all fifty

### Requirement: CSV overflow behavior is user-selectable

When a CSV output's size limit is reached, the dispatcher SHALL honor a configured overflow policy: wrap (overwrite oldest), new-file (rotate to a new file), or error (abort when neither is viable). Wrap MUST be the default. Rotation MUST NOT split a row that is currently being written.

#### Scenario: File rotation does not split an in-flight row

- **WHEN** a CSV dispatcher configured with the new-file overflow policy reaches its size cap mid-row
- **THEN** it MUST complete the in-flight row in the current file before opening the next file, and continue recording in the new file without dropping any row

### Requirement: Dispatcher row encoding is lossless and self-describing

CSV rows SHALL use a fixed-width float format that round-trips exactly through standard `f64` parsing, and SHALL prepend a header row listing `timestamp, time` followed by each channel name in the order declared at init.

#### Scenario: Parsing a logged CSV reproduces the exact f64 values

- **WHEN** a fixed-width CSV file produced by Deimos is read back with standard `f64` parsing
- **THEN** every finite channel value MUST parse to the bitwise-identical `f64` that was written, and NaN MUST parse as NaN
