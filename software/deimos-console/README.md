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
| `staleness_threshold_secs` | float | `2.0` | Seconds without a received row before the connection-health indicator turns stale |
| `forensic_log_path` | path | none | If set, a per-session CSV is written here; rotates at 64 MiB |
| `panels` | array of `{title, channels}` | `[]` | Display panels; each renders the named channels as overlaid traces |

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
