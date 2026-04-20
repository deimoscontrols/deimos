# Peripheral subsystem — non-obvious patterns

## HOOTL architecture

- **`HootlPeripheral` wraps `Box<dyn Peripheral>`** and is itself a `Peripheral` impl. All trait
  methods except `parse_operating_roundtrip` are pure pass-throughs to the inner peripheral.
  Source: `hootl.rs:104-168`.

- **`parse_operating_roundtrip` overrides output values with synthetic data.** The inner
  peripheral's parse is called but its output slice is then overwritten with
  `counter as f64 + idx * 0.01` (Operating mode) or zeroes (other modes). This means HOOTL
  output values do NOT reflect the real peripheral's wire format — they are placeholders.
  A `FUTURE` comment at line 148 acknowledges this gap. Source: `hootl.rs:137-163`.

- **`HootlRunner` sends partial response packets.** In Operating state, the runner writes only
  `OperatingMetrics` (counter + last_input_id) to the first `OperatingMetrics::BYTE_LEN` bytes
  of the response buffer and zeroes the rest — it does NOT call the inner peripheral to produce
  a realistic response. Source: `hootl.rs:583-594`.

- **`HootlDriver::run` spawns the state machine on a named thread ("hootl-runner").** The main
  thread is never blocked. `HootlRunHandle::Drop` sends the stop signal automatically.
  Source: `hootl.rs:338-349`, `hootl.rs:294-298`.

- **Canonical entry point is `Controller::attach_hootl_driver`.** This method atomically removes
  a real peripheral from the controller's map, creates a linked `(HootlPeripheral, HootlDriver)`
  pair (shared `Arc<Mutex<HootlState>>`), starts the driver thread, and re-inserts the wrapper.
  Direct `HootlPeripheral::new_driver_owned` is `fn new_driver_owned` (not pub); callers must go
  through `attach_hootl_driver` or `build_hootl_pair` to preserve the shared state link.
  Source: `controller/mod.rs:179-211`, `hootl.rs:52`, `hootl.rs:629-639`.

- **JSON round-trip breaks the shared state link.** `HootlState` is `#[serde(skip)]` and
  defaults to a fresh `Arc<Mutex<HootlState>>` on deserialization. The `HootlPeripheral` docstring
  warns about this. Never serialize/deserialize a live `HootlPeripheral`. Source: `hootl.rs:47`.

## HOOTL transport

- **`HootlTransport::ThreadChannel` is the only transport that requires no OS resources.** It
  uses `ControllerCtx::sink_endpoint` (in-process crossbeam channel). The `UnixSocket` variant
  creates a real file under `{op_dir}/sock/per/{name}`; `Udp` binds `0.0.0.0:PERIPHERAL_RX_PORT`.
  For CI and unit tests, always use `ThreadChannel`. Source: `hootl.rs:694-729`.

- **Packet framing is symmetric with `ThreadChannelSocket`.** Both sides prepend
  `PeripheralId::BYTE_LEN` bytes before the payload. The controller-side socket
  (`ThreadChannelSocket`) reads the PeripheralId prefix and returns it as
  `SocketPacketMeta::pid`; the HOOTL driver strips the same prefix on recv and prepends it on
  send. Source: `thread_channel.rs:82-93`, `hootl.rs:763-813`.

- **The runner polls at ~1 kHz when idle** (1 ms `thread::sleep` in Binding, Configuring, and
  Operating idle branches). This is a resource guard, not a correctness dependency — packets are
  processed immediately when they arrive. Source: `hootl.rs:513, 560, 617`.

## HOOTL lifecycle gaps (known limitations as of task 1.8 verification)

- **No cycle-count stop.** `HootlDriver::with_end` accepts a wall-clock `SystemTime` deadline
  only. There is no `max_cycles: Option<u64>` API. Deterministic N-cycle tests are not possible
  without external coordination (e.g., wall-clock timing or dispatcher row counting).

- **No injection or observation hook on `HootlRunHandle`.** `stop()`, `is_running()`, and `join()`
  are the entire API. Tests cannot observe cycle count, inject specific output values, or assert
  on per-cycle behavior via the handle alone.

## Built-in peripheral models

- Six built-in models: `AnalogIRev2`, `AnalogIRev3`, `AnalogIRev4`, `DeimosDaqRev5`,
  `DeimosDaqRev6`, `DeimosDaqRev7`. All use `#[typetag::serde]` with `pub mod` in `mod.rs`.
  Source: `peripheral/mod.rs:12-31`.

- **`parse_binding` maps model numbers to peripheral constructors.** If no plugin matches, it
  falls through to a `match m { ... }` on the six known model numbers. Plugins (custom peripherals)
  are provided via `PluginMap<'a>`. Source: `peripheral/mod.rs:150-194`.

- **`Box<dyn Peripheral>` clone is JSON-round-trip.** `impl Clone for Box<dyn Peripheral>` uses
  `serde_json::to_string` + `serde_json::from_str`. Fast for code convenience, slow for hot paths.
  Source: `peripheral/mod.rs:66-72`.
