# realtime-calc Specification

## Purpose

Defines the realtime calc contract: once-per-cycle evaluation over a shared tape, name-based input resolution at init, serializable trait objects, the built-in calc library (affine, polynomial, Butter2, PID, thermocouple, sequence machine, ...), and reset-between-runs state hygiene.

## Requirements
### Requirement: Calcs evaluate once per cycle in lock-step with sampling

Each registered calc SHALL be evaluated exactly once per controller cycle, in the same tick as peripheral I/O, before the next cycle begins. Calcs MUST NOT block: the orchestrator runs each calc synchronously with no timeout guard, so a hanging calc stalls the entire controller loop. The "no block" rule is enforced by convention and reviewer judgment only, not by a runtime budget check.

<!-- FOLLOW-ON: A future change proposal could add a per-eval wall-clock budget that aborts a hanging calc and fails the cycle, turning the convention into an enforced contract. Not a gate on this bootstrap. -->

#### Scenario: Every registered calc evaluates each cycle

- **WHEN** the controller completes one operating cycle
- **THEN** every registered calc MUST have evaluated exactly once before peripheral I/O for the next cycle begins

### Requirement: Calcs are pure functions over a shared tape

The controller SHALL provide all calc inputs and outputs as indices into a single shared mutable scalar tape. A calc MUST only read from its registered input indices and write to its registered output range; reads or writes outside that range are undefined.

#### Scenario: Calcs chain via tape indices within a single cycle

- **WHEN** calc B declares an input that maps to an output field of calc A
- **THEN** the orchestrator SHALL order evaluation so A writes its output slot before B reads it, within the same cycle

### Requirement: Calcs declare a name-based input map resolvable at init

Each calc SHALL expose a map from its local input names to fully-qualified source names, and SHALL support re-routing an input to a different source by name before init. Source names are resolved to tape indices exactly once, at init. Re-routing after init is not supported: the stored name is mutated unconditionally, the resolved tape index is NOT updated, no error is returned, and the calc continues reading from the originally resolved index.

<!-- FOLLOW-ON: Post-init re-routing could return an error (or be a documented no-op) to make the misuse explicit. Not a gate on this bootstrap. -->

#### Scenario: Input re-routed before run start takes effect at init

- **WHEN** a user re-routes a calc input to a new fully-qualified source before the run starts
- **THEN** the orchestrator SHALL resolve the new source to its tape index at init and the calc MUST consume that value each cycle

#### Scenario: Unresolvable source fails init fast

- **WHEN** an input map references a source name no other entity produces
- **THEN** init MUST fail with an error identifying the missing source

### Requirement: Calcs are serializable, clonable, and configurable by name

A boxed calc SHALL round-trip through typed serde serialization preserving its concrete type. Calcs MUST expose their configuration as a string-keyed map of f64 values, settable by the same keys. Each calc MUST expose a per-calc flag that opts its outputs in or out of dispatcher streams.

#### Scenario: Serialize a controller configuration with calcs

- **WHEN** a controller containing multiple calcs is serialized to JSON and deserialized
- **THEN** every calc MUST reconstruct with the same concrete type, input map, and config values

### Requirement: Deimos ships a library of built-in calc types

The built-in calc set SHALL include, at minimum: Affine, InverseAffine, Constant, Sin, Polynomial, Butter2, Pid, RtdPt100, TcKtype, and SequenceMachine. These MUST be usable without additional dependencies, covering common sensor linearization (RTD, K-type thermocouple), filtering, closed-loop control, and sequencing needs out of the box.

#### Scenario: Common sensor linearization available without custom code

- **WHEN** a user wires up an RTD or K-type thermocouple channel
- **THEN** a built-in calc MUST be available to convert the raw electrical reading to temperature without the user writing a custom calc

### Requirement: Calcs reset between runs

Before a new run starts, every calc SHALL have its terminate hook invoked to clear state, and its init hook invoked again before the next operating cycle begins. Persistent state (filter history, integrator state, sequence machine position) MUST NOT leak across runs. Invalid init preconditions are a fail-fast panic, not a recoverable error — the controller process is expected to die on violation rather than continue with bad state.

<!-- FOLLOW-ON: A future change proposal could convert bad init preconditions (e.g. non-positive sample interval) to recoverable errors, aligning calc init with the Result-based error contract used elsewhere. The current panic-on-bad-precondition behavior is intentional fail-fast for now. -->

#### Scenario: Stateful calc resets between runs

- **WHEN** a stateful calc (IIR filter, PID integrator, sequence machine) is used in run N and the controller starts run N+1
- **THEN** its internal state MUST be re-initialized to the same baseline it had at the start of run N
