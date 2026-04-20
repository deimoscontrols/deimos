# Dispatcher subsystem — non-obvious patterns

## Terminate semantics vary by dispatcher

`CsvDispatcher::terminate` synchronously joins its worker thread after closing the mpsc sender,
so the file is flushed and truncated to its final length before `terminate` returns. Readers
may safely open the CSV as soon as `Controller::run` exits. `WorkerHandle` also joins on drop,
so re-init and unexpected drops are safe too.

`TimescaleDbDispatcher::terminate` (`tsdb.rs:226-233`) and `LatestValueDispatcher::terminate`
(`latest.rs:158-162`) still only drop `self.worker` and return immediately. Their worker
threads flush asynchronously after the channel is closed — so for those dispatchers, any
external observation that depends on flushed state must account for the race.

## TimescaleDbDispatcher::init panics on unreachable DB

`Controller::run` calls `dispatcher.init(…).unwrap()` at `src/controller/mod.rs:839`. If
`TimescaleDbDispatcher::init` returns `Err` (bad host, bad credentials, unreachable port), the
controller **panics** — it does not return `Err`. The CSV and other dispatchers receive no rows.

Observed in `examples/hootl_csv_fallback.rs` (task 2.2).

## Dispatcher consume errors are fatal to the entire session

The operating loop at `src/controller/mod.rs:1543-1558` iterates all dispatchers each cycle.
If **any** dispatcher's `consume` returns `Err`, all errors are collected and the loop returns
`Err(String)`, terminating the session. There is no per-dispatcher error isolation: one failing
dispatcher kills the run for all others.

This means the `data-dispatch` spec's "emergency fallback" scenario (CSV continues when DB fails
mid-run) is **not** satisfied. See `<!-- REVIEW: -->` in `specs/data-dispatch/spec.md`.

## TimescaleDbDispatcher construction

`TimescaleDbDispatcher::new` takes `(dbname, host, user, token_name, buffer_time, retention_time)`.
`token_name` is the **name of an environment variable** whose value is the DB password — not the
password itself. The env var must be set before `init` is called or init returns `Err`.

## CsvDispatcher pre-allocates the file

`CsvDispatcher::new(chunk_size_megabytes, overflow_behavior)` pre-allocates a file of exactly
`chunk_size_megabytes * 1 MiB` at init time using `file.set_len`. A small value (e.g. 1 MB at
20 Hz) is sufficient for short HOOTL sessions. Fixed-width row formatting makes O(1) seek to any
row by row number.
