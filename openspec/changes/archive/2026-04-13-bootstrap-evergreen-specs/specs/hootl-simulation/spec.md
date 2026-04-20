<!-- REVIEW-COMPLETE (task 3.3): approved — spec now describes actual HOOTL behavior (outbound-only byte compat; lifecycle-equivalent rather than value-equivalent; wall-clock-bounded rather than cycle-count-bounded; start/stop handle surface). Feature gaps (synthetic parse outputs, cycle-count stop, observation hooks) are tracked as follow-on change proposals. -->

## ADDED Requirements

### Requirement: HOOTL provides a first-class `Peripheral` implementation with no hardware required

Deimos SHALL ship `HootlPeripheral` as a `Peripheral` impl that can drive a controller through its full lifecycle without hardware. A user running a controller backed entirely by `HootlPeripheral` instances MUST be able to complete the full lifecycle (`Disconnected → Binding → Configuring → Operating`) and produce rows for dispatchers without any physical network adapter or DAQ hardware present. HOOTL is lifecycle- and wire-framing-equivalent to real hardware but is NOT value-equivalent: output samples produced by `HootlPeripheral` are synthetic (see the "HOOTL parses and emits the same wire format as its modeled peripheral" requirement below).

Reference: `software/deimos/src/peripheral/hootl.rs` — `HootlPeripheral`, `HootlDriver`, `HootlRunHandle`, `HootlTransport`; exported from `software/deimos/src/peripheral/mod.rs`.

#### Scenario: CI runs the controller without hardware

- **WHEN** a CI environment runs a test that instantiates a controller with `HootlPeripheral` and `HootlTransport::ThreadChannel`
- **THEN** the run MUST succeed end-to-end without opening a network socket to any external host and without access to `/dev` device nodes

<!-- Note: `HootlTransport::Udp` and `HootlTransport::UnixSocket` do open OS-level sockets. CI tests that must avoid network sockets MUST use the `ThreadChannel` variant. -->

### Requirement: HOOTL transport is packet-equivalent to the production transport

`HootlTransport` (paired with the `thread_channel` socket) SHALL present the same packet framing and routing semantics as a real UDP/Unix transport: one logical packet per `send`, one logical packet per `recv` (subject to the `thread_channel` buffer-size caveat documented in the `transport-abstraction` capability), and correct routing by `PeripheralId`. Arrival timestamps are recorded on the receiver side after `recv_timeout` returns; for in-process channels this is expected to be sub-microsecond and sufficient for LOCAL time-sync, but the spec does not pin a quantitative bound.

Reference: `HootlTransport` in `software/deimos/src/peripheral/hootl.rs`; `ThreadChannelSocket` in `software/deimos/src/socket/thread_channel.rs`; related contract in the `transport-abstraction` capability.

#### Scenario: Controller lifecycle reproduced in HOOTL

- **WHEN** a controller configuration is run against HOOTL
- **THEN** the lifecycle state machine (`Disconnected → Binding → Configuring → Operating`, loss-of-contact → `Disconnected`) MUST exercise the same controller code paths as production deployments

<!-- Note: The data interpretation path (parsing operating-roundtrip output packets into channel values) is NOT value-equivalent — HOOTL synthesizes outputs rather than reproducing real hardware values. Bugs in output *value* interpretation may not reproduce under HOOTL; bugs in *framing*, *routing*, *lifecycle*, or *timing* do. See the "wire format" requirement below. -->

### Requirement: HOOTL run handle exposes start/stop lifecycle control

`HootlRunHandle` SHALL expose `stop()`, `is_running()`, and `join()` so tests can drive a HOOTL session from the main thread without blocking on it. The driver state machine runs on a dedicated "hootl-runner" thread; `Drop` on the handle calls `stop()` automatically.

Reference: `HootlRunHandle` in `software/deimos/src/peripheral/hootl.rs`.

#### Scenario: Test starts and stops HOOTL in a single process

- **WHEN** an integration test calls `HootlDriver::run(...)` and then `handle.stop()` / `handle.join()`
- **THEN** the test MUST be able to start HOOTL, drive the controller through operation, stop HOOTL, and inspect dispatcher output (via a dispatcher the test registered before `Controller::run`) — all within a single test process

<!-- FOLLOW-ON: `HootlRunHandle` does not currently expose cycle-level observation or injection hooks (no cycle counter, no per-cycle callback, no way to inject responses). The `FUTURE` comment at `hootl.rs:148` acknowledges this. A future change proposal could add an observation/injection surface for tests that need to assert on specific cycles. Not a gate on this bootstrap. -->

### Requirement: HOOTL is the default simulation surface, not a third-party add-on

HOOTL SHALL be shipped and maintained in the same crate as the controller, peripheral trait, and built-in calcs. Using HOOTL MUST NOT require enabling an optional Cargo feature flag, pulling an extra dependency, or installing tooling outside the default `deimos` crate.

Reference: `software/deimos/src/peripheral/hootl.rs` is compiled unconditionally by `software/deimos/src/peripheral/mod.rs` (`pub mod hootl;`).

<!-- REVIEW: VERIFIED. `software/deimos/src/peripheral/mod.rs` line 30: `pub mod hootl;` — no `#[cfg(feature = ...)]` guard. `software/deimos/Cargo.toml` `[features]` section: `default = []`, `python = ["pyo3"]`. The `pyo3` gate (`#[cfg(feature = "python")]`) is present on the Python-binding blocks inside `hootl.rs` but the types and Rust API are compiled unconditionally. `use deimos::peripheral::HootlPeripheral` requires only the `deimos` crate with no extra features. `pub use hootl::{HootlDriver, HootlPeripheral, HootlRunHandle, HootlTransport}` at `mod.rs:31` makes all four types available at the crate root. -->

#### Scenario: New contributor writes a controller test

- **WHEN** a new contributor adds an integration test that needs a peripheral
- **THEN** they MUST be able to `use deimos::peripheral::HootlPeripheral` with no additional setup beyond the `deimos` dev-dependency already in the project

<!-- REVIEW: VERIFIED. No feature flag needed. The canonical entry point is `Controller::attach_hootl_driver(peripheral_name, HootlTransport::thread_channel("name"), end)` which atomically replaces a real peripheral with a `HootlPeripheral` and starts the driver thread. Alternatively, `HootlDriver::new` + `HootlDriver::run` can be used standalone. See `controller/mod.rs:179-211`. -->

### Requirement: HOOTL emits the same wire format as its modeled peripheral; response values are synthetic

When `HootlPeripheral` simulates a specific DAQ model, its **outbound** `emit_operating_roundtrip` and `emit_configuring` implementations MUST produce byte-level wire formats identical to the real peripheral being simulated (pure pass-through to the inner peripheral's emit path). The **inbound** response path is synthetic: `HootlRunner` in Operating state writes `OperatingMetrics` (counter, last_input_id) to the first `OperatingMetrics::BYTE_LEN` bytes and zeros the remainder, and `HootlPeripheral::parse_operating_roundtrip` overwrites the inner peripheral's parsed outputs with placeholder values (`counter + idx * 0.01`). This divergence is intentional: HOOTL is a lifecycle/framing simulator, not a hardware model.

Reference: `Peripheral` trait in `software/deimos/src/peripheral/mod.rs`; `HootlPeripheral` in `software/deimos/src/peripheral/hootl.rs`; real-peripheral wire formats in `software/deimos_shared/src/peripherals/`. Empirical confirmation: `test_deimos_daq_rev7_hootl_emit_byte_compat` and `test_deimos_daq_rev7_hootl_parse_diverges` in `peripheral::hootl::tests`.

#### Scenario: Outbound byte-level compat with real firmware

- **WHEN** a HOOTL peripheral is configured to simulate `DeimosDaqRev7` and calls `emit_operating_roundtrip`
- **THEN** the packet bytes MUST be byte-for-byte identical to what a real `DeimosDaqRev7` would produce for the same inputs, period/phase deltas, and cycle id

#### Scenario: Inbound response is synthetic

- **WHEN** a HOOTL peripheral receives an operating-roundtrip cycle
- **THEN** the host-visible output values are synthetic (`counter + idx * 0.01`), NOT reproductions of real hardware ADC/encoder/GPIO readings

<!-- FOLLOW-ON: A future change proposal could make `HootlRunner` delegate response-bytes construction to the inner peripheral (the "FUTURE: use peripheral object to write output" comment at `hootl.rs:584` acknowledges this). That would make HOOTL value-equivalent as well as framing-equivalent. Not a gate on this bootstrap — the synthetic behavior is what the spec now describes. -->

### Requirement: HOOTL sessions are bounded by wall-clock time

A HOOTL session SHALL support a wall-clock time bound via `HootlDriver::with_end(end: Option<SystemTime>)`. The state machine advances whenever a packet arrives; `thread::sleep(Duration::from_millis(1))` in the idle branches is a pacing guard (~1 kHz polling), not a correctness dependency.

Reference: `HootlDriver::with_end` and runner loop at `hootl.rs:470-621`; the controller's loop pacing strategies in `software/deimos/src/controller/timing.rs`.

#### Scenario: Time-bounded HOOTL run

- **WHEN** an integration test configures `HootlDriver::with_end(Some(deadline))` and starts the driver
- **THEN** the driver MUST stop on or shortly after `deadline`, and the test MUST be able to assert on the rows delivered to its dispatcher before the deadline elapsed

<!-- FOLLOW-ON: There is no cycle-count-based stop API today. `HootlConfig` could grow a `max_cycles: Option<u64>` field to enable deterministic "run exactly N operating cycles" tests. A future change proposal could add this alongside the observation/injection hooks noted above. Not a gate on this bootstrap. -->
