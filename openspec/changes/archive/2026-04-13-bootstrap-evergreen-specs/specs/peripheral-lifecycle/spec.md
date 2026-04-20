<!-- REVIEW-COMPLETE (task 3.3): approved-with-open-REVIEW-comments — reconnect policy-dependence in PeripheralId-survives-reconnect scenario needs narrowing (Reconnect vs Terminate policy), but all other requirements are well-formed and code-verified -->

## ADDED Requirements

### Requirement: Peripheral identity is unique and stable across a run

Every peripheral representation SHALL expose a `PeripheralId` that combines a model number and a serial number. The id MUST be stable for the lifetime of a controller run and SHALL be used by the controller to distinguish one peripheral from another on shared transports.

Reference: `software/deimos/src/peripheral/mod.rs` (`fn id(&self) -> PeripheralId`), `software/deimos_shared/src/peripherals/`.

#### Scenario: Two peripherals on the same UDP segment

- **WHEN** two DAQ nodes with different serial numbers respond on the same UDP broadcast
- **THEN** the controller SHALL treat them as distinct peripherals and route inputs/outputs to each by `PeripheralId`

#### Scenario: Peripheral id survives reconnect

<!-- REVIEW: The reconnect re-association logic in controller/mod.rs (`handle_reconnect_packet`)
preserves PeripheralId and reuses the existing PeripheralState without restarting the run
lifecycle — so the PeripheralId stability part is honored. However, the re-association path is
only reachable when the controller is configured with `LossOfContactPolicy::Reconnect`. Under
`LossOfContactPolicy::Terminate` (the other valid policy, controller/mod.rs:1104-1126), the
controller exits on loss of contact and never attempts re-association. The scenario as written
implies re-association is unconditional ("within the same run"), but the code makes it
policy-dependent. Maintainers should decide: (a) narrow this scenario to apply only under the
Reconnect policy, or (b) consider whether Terminate mode also needs a "reconnect first, then
give up" path before this requirement can be stated unconditionally. -->
- **WHEN** a peripheral disconnects and reconnects within the same run
- **THEN** its `PeripheralId` SHALL remain unchanged and the controller SHALL re-associate it without restarting the run lifecycle from scratch

### Requirement: Peripheral exposes named input and output fields

A peripheral SHALL publish an ordered list of input field names (values sent *to* the peripheral each cycle) and an ordered list of output field names (values received *from* the peripheral each cycle). The ordering MUST be stable for the lifetime of a run so that tape indices remain valid.

Reference: `Peripheral::input_names`, `Peripheral::output_names` in `software/deimos/src/peripheral/mod.rs`.

#### Scenario: Calc consumes a peripheral output by name

- **WHEN** a calc maps an input field to `peripheral_0.output_1`
- **THEN** the controller SHALL resolve that name against `output_names()` at init time and consume the value at the corresponding tape index every cycle

### Requirement: Peripheral defines its wire format for each lifecycle stage

For both the Configuring stage and the OperatingRoundtrip stage, a peripheral SHALL declare the byte length of outbound and inbound packets, and SHALL provide emit/parse functions that translate between in-memory values and those byte buffers.

Reference: `operating_roundtrip_input_size`, `operating_roundtrip_output_size`, `emit_operating_roundtrip`, `parse_operating_roundtrip`, `configuring_input_size`, `configuring_output_size` in `software/deimos/src/peripheral/mod.rs`.

#### Scenario: Controller sends an operating cycle packet

- **WHEN** the controller enters `OperatingRoundtrip` for a bound peripheral
- **THEN** it SHALL allocate a buffer of size `operating_roundtrip_input_size()` and ask the peripheral to emit the packet via `emit_operating_roundtrip`, including cycle id and period/phase deltas

#### Scenario: Controller receives an operating cycle response

- **WHEN** an operating-stage packet arrives from a peripheral
- **THEN** the controller SHALL call `parse_operating_roundtrip` which MUST populate the output tape and return `OperatingMetrics` describing that cycle

### Requirement: Peripherals are serializable and clonable as trait objects

`Box<dyn Peripheral>` SHALL be serializable and deserializable via `typetag::serde`. A clone of a boxed peripheral MUST produce an equivalent representation — this is how controller configurations are persisted and transported.

Reference: `#[typetag::serde(tag = "type")]` on `trait Peripheral`, `impl Clone for Box<dyn Peripheral>` in `software/deimos/src/peripheral/mod.rs`.

#### Scenario: Persist a controller configuration to JSON

- **WHEN** a user serializes a controller that owns a `Box<dyn Peripheral>`
- **THEN** the peripheral MUST round-trip through JSON preserving its concrete type and state

### Requirement: Peripheral ships a standard calc set for its outputs

A peripheral SHALL provide `standard_calcs(name)` returning a map of default calcs that convert its raw outputs into usable engineering units (for example, raw ADC counts → volts, or raw thermocouple voltage → temperature).

Reference: `Peripheral::standard_calcs` in `software/deimos/src/peripheral/mod.rs`; see concrete `deimos_daq_rev7.rs`, `analog_i_rev_4.rs`, `hootl.rs`.

#### Scenario: User adds a DAQ node without writing calcs

- **WHEN** a user constructs a controller with a `DeimosDaqRev7` peripheral and no custom calcs
- **THEN** the peripheral SHALL contribute a default set of calcs that produce engineering-unit channels to the tape

### Requirement: Binding responses resolve to a concrete peripheral type

The controller SHALL call `parse_binding` with a `BindingOutput` and produce a `Box<dyn Peripheral>` of the correct concrete type based on the reported model number. Unknown model numbers SHALL be resolvable via an optional plugin map; an unrecognized model with no matching plugin MUST return an error.

Reference: `pub fn parse_binding(...)` and `PluginFn` / `PluginMap` in `software/deimos/src/peripheral/mod.rs`.

#### Scenario: Plugin overrides a built-in model resolution

- **WHEN** a plugin map contains the model number reported by a peripheral
- **THEN** `parse_binding` MUST invoke the plugin function and use its returned peripheral instead of the built-in match arm

#### Scenario: Unknown model number with no plugin

- **WHEN** a peripheral binds with a model number not present in the built-in match and no plugin is registered
- **THEN** `parse_binding` MUST return `Err` with a message identifying the unrecognized model number

### Requirement: HOOTL is a first-class peripheral implementation

Deimos SHALL provide a hardware-out-of-the-loop peripheral (`HootlPeripheral`) that implements `Peripheral` without real hardware, so tests and examples can exercise the full controller lifecycle without a physical DAQ.

Reference: `software/deimos/src/peripheral/hootl.rs`.

#### Scenario: Running a controller without hardware

- **WHEN** a user constructs a controller with only `HootlPeripheral` instances
- **THEN** the controller lifecycle (`Connecting → Binding → Configuring → OperatingRoundtrip`) MUST complete end-to-end without any physical network or device attached
