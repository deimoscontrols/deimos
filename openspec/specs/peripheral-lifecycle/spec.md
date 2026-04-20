# peripheral-lifecycle Specification

## Purpose

Defines the `Peripheral` trait surface: stable identity, named input/output fields, per-lifecycle-stage wire-format contracts, serializable trait objects, plugin registration, and the HOOTL parity requirement that any real peripheral can also be simulated in-process.

## Requirements
### Requirement: Peripheral identity is unique and stable across a run

Every peripheral SHALL expose an identifier combining a model number and a serial number. That identifier MUST remain stable for the lifetime of a controller run and SHALL be the key the controller uses to distinguish peripherals sharing a transport.

#### Scenario: Two peripherals on the same UDP segment

- **WHEN** two DAQ nodes with different serial numbers respond on the same UDP broadcast
- **THEN** the controller SHALL treat them as distinct peripherals and route inputs/outputs to each by peripheral id

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
- **THEN** its peripheral id SHALL remain unchanged and the controller SHALL re-associate it without restarting the run lifecycle from scratch

### Requirement: Peripheral exposes named input and output fields

A peripheral SHALL publish an ordered list of input field names (values sent *to* the peripheral each cycle) and an ordered list of output field names (values received *from* the peripheral each cycle). Ordering MUST be stable for the lifetime of a run so that downstream consumers can bind to field positions at init time and read from the same position every cycle.

#### Scenario: Calc consumes a peripheral output by name

- **WHEN** a calc maps an input to a peripheral output field by name
- **THEN** the controller SHALL resolve that name against the peripheral's output field list at init time and consume the corresponding value every cycle

### Requirement: Peripheral defines its wire format for each lifecycle stage

For both the Configuring stage and the OperatingRoundtrip stage, a peripheral SHALL declare the byte length of outbound and inbound packets and SHALL provide emit/parse functions that translate between in-memory values and those byte buffers.

#### Scenario: Operating cycle roundtrip

- **WHEN** the controller executes an operating cycle for a bound peripheral
- **THEN** it SHALL emit an outbound packet sized by the peripheral's declared operating input size, carrying cycle id and period/phase information
- **AND** the peripheral's parse of the inbound response MUST populate the output tape and return per-cycle operating metrics to the controller

### Requirement: Peripherals are serializable and clonable as trait objects

A peripheral value SHALL be serializable and deserializable as a tagged trait object, and SHALL be clonable. Round-tripping a peripheral through serialization MUST preserve its concrete type and state. This is how controller configurations are persisted and transported.

#### Scenario: Persist a controller configuration to JSON

- **WHEN** a user serializes a controller that owns a peripheral
- **THEN** the peripheral MUST round-trip through JSON preserving its concrete type and state

### Requirement: Peripheral ships a standard calc set for its outputs

A peripheral SHALL provide a default set of calcs that convert its raw outputs into usable engineering units (for example, raw ADC counts → volts, or raw thermocouple voltage → temperature).

#### Scenario: User adds a peripheral without writing calcs

- **WHEN** a user constructs a controller with a peripheral and supplies no custom calcs
- **THEN** the peripheral SHALL contribute its default calc set so engineering-unit channels appear on the tape

### Requirement: Binding responses resolve to a concrete peripheral type

When the controller receives a binding response, it SHALL produce a concrete peripheral instance based on the reported model number. An optional plugin map MAY override built-in model resolution. An unrecognized model with no matching plugin MUST return an error identifying the unrecognized model number.

#### Scenario: Plugin overrides a built-in model resolution

- **WHEN** a plugin map contains the model number reported by a peripheral
- **THEN** the controller MUST invoke the plugin and use its returned peripheral instead of the built-in resolution for that model

#### Scenario: Unknown model number with no plugin

- **WHEN** a peripheral binds with a model number not present in the built-in set and no plugin is registered
- **THEN** binding MUST fail with an error identifying the unrecognized model number

### Requirement: HOOTL is a first-class peripheral implementation

Deimos SHALL provide a hardware-out-of-the-loop peripheral that implements the peripheral contract without real hardware, so tests and examples can exercise the full controller lifecycle without a physical DAQ.

#### Scenario: Running a controller without hardware

- **WHEN** a user constructs a controller with only HOOTL peripheral instances
- **THEN** the controller lifecycle (Connecting → Binding → Configuring → OperatingRoundtrip) MUST complete end-to-end without any physical network or device attached
