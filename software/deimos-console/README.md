# deimos-console

Operator console viewer for Deimos realtime reporting. Joins the UDP multicast stream published by
a `ReportingDispatcher`, renders scrolling unit-labeled scope traces for configured channel groups,
writes a per-session forensic CSV log so operators can reconstruct exactly what was on screen at any
moment, and displays a connection-health indicator that turns stale when the stream goes quiet.

## Running

```sh
cargo run -p deimos-console -- --config software/deimos-console/examples/console.toml
```

Run from the repo root. The console connects automatically when the controller enters Operating
state and recovers gracefully when it restarts.

## Config file format

The config file is TOML. All fields except `multicast_group` and `port` are optional.

| Field | Type | Default | Description |
|---|---|---|---|
| `multicast_group` | IPv4 address | — | Multicast group the reporting dispatcher publishes to (e.g. `239.255.0.1`) |
| `port` | integer | — | UDP port (must match the dispatcher's configured port) |
| `interface` | IPv4 address | OS default | Local interface address to bind for multicast reception |
| `window_seconds` | float | `30.0` | Seconds of history kept in each scrolling trace |
| `staleness_threshold_secs` | float | `2.0` | Seconds without a received row before the connection-health indicator turns stale; also the threshold for the UI-pause and per-row data-staleness signals that drive stall detection |
| `tail_keep_secs` | float | `0.5` | Seconds of pre-stall context retained in each per-channel ring buffer when a stall fires (the rest is evicted so the live window is restored within one repaint) |
| `recovery_settle_secs` | float | `2.0` | Seconds the connection-health indicator stays in `Recovering` after a stall clears, so an operator notices the discontinuity even on a brief glance |
| `forensic_log_path` | path | none | If set, a per-session CSV is written here; rotates at 64 MiB |
| `panels` | array of `{title, channels}` | `[]` | Display panels; each renders the named channels as overlaid traces |

### Per-second telemetry tick

The console emits a `key=value` line to stderr roughly once per second so operators and post-hoc
log analysis can correlate viewer behavior with the controller's tracing output. Format:

```text
[HH:MM:SS.mmm] deimos-console: tick rows=N rate=X.YHz last_seq=N \
    wire_drops=N recv_drops=N overwritten_frames=N stalls_detected=N \
    stale_rows_evicted=N pending=N [lag_ms=min/mean/max(min/mean/max)] [(idle Ns)]
```

| Key | Meaning |
|---|---|
| `rows` | `Row` messages processed since the previous tick |
| `rate` | Rows per second over the elapsed tick interval (Hz) |
| `last_seq` | Sequence number of the most recently processed `Row`, or `-` before the first row |
| `wire_drops` | Cumulative count of sequence-number gaps observed on the wire (controller-side or network packet loss) |
| `recv_drops` | Cumulative count of messages the receiver thread could not enqueue at all — zero in steady state under drop-oldest semantics |
| `overwritten_frames` | Cumulative count of receiver-thread drop-oldest evictions; grows under sustained backpressure while preserving freshness |
| `stalls_detected` | Cumulative count of stall events flagged by the UI-thread heuristic; each event corresponds to one pre-stall buffer clear |
| `stale_rows_evicted` | Cumulative count of pre-stall samples popped from per-channel ring buffers across all stall events (forensic log retains them) |
| `pending` | Current depth of the pre-schema pending-rows queue |
| `lag_ms` | Optional per-row rx-lag triple (viewer receipt minus controller cycle-start wall clock); only present when at least one row was processed this tick |

### Example

```toml
# Sample configuration for the Deimos operator console.
# Run with: cargo run -p deimos-console -- --config software/deimos-console/examples/console.toml

# Multicast group the reporting dispatcher publishes to.
multicast_group = "239.255.0.1"

# UDP port (must match the reporting dispatcher's configured port).
port = 29573

# Outbound interface for multicast reception. Comment out to use the OS default.
# interface = "192.168.1.10"

[[panels]]
title = "Temperatures"
channels = ["rtd0_temperature_K", "tc0_temperature_K"]

[[panels]]
title = "Currents and Voltages"
channels = ["shunt0_A", "bus0_V"]
```

A full example lives at [`examples/console.toml`](examples/console.toml).
