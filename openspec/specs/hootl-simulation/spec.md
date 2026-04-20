# hootl-simulation Specification

## Purpose

Defines the hardware-out-of-the-loop peripheral: a first-class Peripheral implementation that exercises the full controller lifecycle without hardware. Outbound packets are byte-identical to the modeled DAQ; inbound responses are synthetic. Ships by default (no feature flag) and is bounded by wall-clock time.

## Requirements
### Requirement: HOOTL provides a first-class Peripheral implementation with no hardware required

Deimos SHALL ship a HOOTL Peripheral implementation that can drive a controller through its full lifecycle (Disconnected → Binding → Configuring → Operating) and produce rows for dispatchers without any physical network adapter or DAQ hardware present. HOOTL is lifecycle- and wire-framing-equivalent to real hardware but is NOT value-equivalent: output samples are synthetic (see the wire-format requirement below).

#### Scenario: CI runs the controller without hardware

- **WHEN** a CI environment runs a test that instantiates a controller with a HOOTL peripheral using the in-process thread-channel transport
- **THEN** the run MUST succeed end-to-end without opening a network socket to any external host and without access to OS device nodes
- **NOTE** The UDP and Unix-socket HOOTL transport variants DO open OS-level sockets; CI tests that must avoid network sockets MUST use the in-process thread-channel variant.

### Requirement: HOOTL transport is packet-equivalent to the production transport

The HOOTL transport (paired with the in-process thread-channel socket) SHALL present the same packet framing and routing semantics as a real UDP/Unix transport: one logical packet per send, one logical packet per recv (subject to the thread-channel buffer-size caveat documented in the transport-abstraction capability), and correct routing by peripheral id. Arrival timestamps are recorded on the receiver side after recv returns; for in-process channels this is expected to be sub-microsecond and sufficient for LOCAL time-sync, but the spec does not pin a quantitative bound.

#### Scenario: Controller lifecycle reproduced in HOOTL

- **WHEN** a controller configuration is run against HOOTL
- **THEN** the lifecycle state machine (Disconnected → Binding → Configuring → Operating, plus loss-of-contact → Disconnected) MUST exercise the same controller code paths as production deployments
- **NOTE** The data interpretation path (parsing operating-roundtrip output packets into channel values) is NOT value-equivalent — HOOTL synthesizes outputs rather than reproducing real hardware values. Bugs in output *value* interpretation may not reproduce under HOOTL; bugs in *framing*, *routing*, *lifecycle*, or *timing* do.

### Requirement: HOOTL run handle exposes start/stop lifecycle control

A HOOTL run handle SHALL allow a test to start a session, stop it, query whether it is running, and join the runner thread — all from the main test thread without blocking on it. The driver state machine runs on a dedicated runner thread, and dropping the handle SHALL stop the driver automatically.

#### Scenario: Test starts and stops HOOTL in a single process

- **WHEN** an integration test starts a HOOTL driver and later stops and joins it
- **THEN** the test MUST be able to drive the controller through operation, stop HOOTL, and inspect dispatcher output (via a dispatcher the test registered before the controller run began) — all within a single test process

<!-- FOLLOW-ON: The HOOTL run handle does not currently expose cycle-level observation or injection hooks (no cycle counter, no per-cycle callback, no way to inject responses). A future change proposal could add an observation/injection surface for tests that need to assert on specific cycles. Not a gate on this bootstrap. -->

### Requirement: HOOTL is the default simulation surface, not a third-party add-on

HOOTL SHALL be shipped and maintained in the same crate as the controller, peripheral trait, and built-in calcs. Using HOOTL MUST NOT require enabling an optional Cargo feature flag, pulling an extra dependency, or installing tooling outside the default crate.

#### Scenario: New contributor writes a controller test

- **WHEN** a new contributor adds an integration test that needs a peripheral
- **THEN** they MUST be able to use the HOOTL peripheral with no additional setup beyond the default crate dependency already in the project — no feature flag, no extra crate

### Requirement: HOOTL emits the same wire format as its modeled peripheral; response values are synthetic

When a HOOTL peripheral simulates a specific DAQ model, its **outbound** operating-roundtrip and configuring emissions MUST produce byte-level wire formats identical to the real peripheral being simulated (pure pass-through to the inner peripheral's emit path). The **inbound** response path is synthetic: in Operating state, HOOTL writes operating-metrics (counter, last input id) into the leading bytes of the response and zeros the remainder, and the HOOTL parser overwrites the inner peripheral's parsed outputs with deterministic placeholder values derived from the cycle counter and channel index. This divergence is intentional: HOOTL is a lifecycle/framing simulator, not a hardware model.

#### Scenario: Outbound byte-level compat with real firmware

- **WHEN** a HOOTL peripheral is configured to simulate a specific DAQ model and emits an operating-roundtrip packet
- **THEN** the packet bytes MUST be byte-for-byte identical to what the real modeled peripheral would produce for the same inputs, period/phase deltas, and cycle id

#### Scenario: Inbound response is synthetic

- **WHEN** a HOOTL peripheral receives an operating-roundtrip cycle
- **THEN** the host-visible output values are synthetic deterministic placeholders, NOT reproductions of real hardware ADC/encoder/GPIO readings

<!-- FOLLOW-ON: A future change proposal could make the HOOTL runner delegate response-bytes construction to the inner peripheral. That would make HOOTL value-equivalent as well as framing-equivalent. Not a gate on this bootstrap — the synthetic behavior is what the spec now describes. -->

### Requirement: HOOTL sessions are bounded by wall-clock time

A HOOTL session SHALL support a wall-clock time bound via an optional deadline passed to the driver. The state machine advances whenever a packet arrives; the short sleep in the idle branches of the runner loop is a pacing guard (~1 kHz polling), not a correctness dependency.

#### Scenario: Time-bounded HOOTL run

- **WHEN** an integration test configures a HOOTL driver with a wall-clock deadline and starts it
- **THEN** the driver MUST stop on or shortly after the deadline, and the test MUST be able to assert on the rows delivered to its dispatcher before the deadline elapsed

<!-- FOLLOW-ON: There is no cycle-count-based stop API today. A future change proposal could add a max-cycles bound to enable deterministic "run exactly N operating cycles" tests, alongside the observation/injection hooks noted above. Not a gate on this bootstrap. -->
