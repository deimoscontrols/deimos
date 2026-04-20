# Deimos — top-level guidance

## Build and test commands

Run all commands from the **repo root**. The `Justfile` wraps the underlying
`cargo ... --manifest-path software/deimos/Cargo.toml` invocations.

- **Build the host library:** `just build`
- **Run all library tests:** `just test`
- **Build a specific example:** `just example <name>`
- **List all targets:** `just`

## Running HOOTL

HOOTL (Hardware-Out-Of-The-Loop) runs a full controller lifecycle entirely in-process — no OS network sockets or `/dev` nodes required. The peripheral is simulated via `ThreadChannelSocket`.

### Verified invocation (confirmed 2026-04-13)

```sh
just hootl
```

The example lives at `software/deimos/examples/hootl_lifecycle.rs`.

### Expected log output (in order)

```
INFO  Binding peripherals.
INFO  Configuring peripherals.
INFO  All peripherals acknowledged configuration.
INFO  Entering control loop.
```

These four lines confirm the lifecycle stages in order:
`Connecting` (implicit) → `Binding` → `Configuring` → `OperatingRoundtrip`.

The run terminates automatically after 3 seconds (configurable via the `Termination::Timeout` in the example). Output CSV lands in a temp directory printed in the logs.

### Key types involved

- `ThreadChannelSocket` — in-process socket transport (`software/deimos/src/socket/thread_channel.rs`)
- `DeimosDaqRev7` — concrete peripheral model used for HOOTL (`software/deimos/src/peripheral/hootl.rs`)
- `HootlTransport::thread_channel` — connects the HOOTL driver to the named socket channel
- `Controller::attach_hootl_driver` — spawns the HOOTL runner thread and returns a join handle

## HOOTL Butter2 reset verification (task 2.3, confirmed 2026-04-13)

```sh
just hootl-butter-reset
```

Runs two back-to-back HOOTL sessions with a `Butter2` calc and
compares the first 5 output samples. Both runs produce bit-for-bit identical leading output,
confirming `terminate()+init()` resets filter state to the same baseline before each run.

**Spec check result:** The `realtime-calc` spec's "IIR filter state cleared between runs" scenario
is **satisfied**. `Butter2::terminate()` resets `filt` and `initialized`; `init()` sets
`initialized = false` again, so steady-state init fires on the first eval of every new run.

The HOOTL driver resets its internal counter to 0 each time it enters Operating state, so both
runs see the same input sequence and produce identical outputs when the filter starts fresh.

## HOOTL CSV fallback verification (task 2.2, confirmed 2026-04-13)

```sh
just hootl-csv-fallback
```

The example expects a panic in Phase 1 (bogus DB URL causes
`TimescaleDbDispatcher::init` to fail, and the controller panics at `.unwrap()`). Phase 2
runs CSV-only and confirms ~40 rows at 20 Hz × 2 s.

**Spec gap found:** The `data-dispatch` spec's "emergency fallback" scenario (CSV continues when
DB fails mid-run) is **not** satisfied. Two issues:
1. A failing `TimescaleDbDispatcher::init` panics the controller (`mod.rs:839` uses `.unwrap()`).
2. Any `consume` error aborts the entire session (`mod.rs:1551-1558`).
See `<!-- REVIEW: -->` in `openspec/changes/bootstrap-evergreen-specs/specs/data-dispatch/spec.md`
and `software/deimos/src/dispatcher/CLAUDE.md` for details.
