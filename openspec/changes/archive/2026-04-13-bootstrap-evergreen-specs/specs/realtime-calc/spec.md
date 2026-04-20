<!-- REVIEW-COMPLETE (task 3.3): approved — non-block requirement softened to a documented convention; update_input_map post-init behavior described as unguarded; init precondition violations described as panics. Candidate enforcement changes (eval deadline, post-init guard, Result-based init) are tracked as follow-on proposals. -->

## ADDED Requirements

### Requirement: Calcs evaluate once per cycle in lock-step with sampling

A `Calc` implementation SHALL be evaluated exactly once per controller cycle via `eval(tape)`, in the same tick as peripheral I/O. Calcs MUST NOT block by convention — `CalcOrchestrator::eval` runs each calc synchronously with no timeout guard, so a calc that hangs stalls the entire controller loop. The "no block" rule is enforced only by documentation and reviewer judgment, not by a runtime budget check.

<!-- FOLLOW-ON: A future change proposal could add a per-eval wall-clock budget that aborts a hanging calc and fails the cycle, turning the convention into an enforced contract. Not a gate on this bootstrap. -->

Reference: `trait Calc::eval` and `CalcOrchestrator` in `software/deimos/src/calc/mod.rs`, `software/deimos/src/calc/orchestrator.rs`.

#### Scenario: Every cycle evaluates every calc

- **WHEN** the controller is in `OperatingRoundtrip` and completes one cycle
- **THEN** every registered calc MUST have had `eval` called exactly once before the next peripheral I/O cycle begins

### Requirement: Calcs are pure functions over a shared tape

The controller SHALL provide all calc inputs and outputs as indices into a single `&mut [f64]` tape. A calc MUST only read from its registered input indices and write to its registered output range; reading or writing outside that range is undefined.

Reference: `Calc::init(ctx, input_indices, output_range)` and `Calc::eval(tape)` in `software/deimos/src/calc/mod.rs`.

#### Scenario: Calcs chain via tape indices

- **WHEN** calc `B` declares an input that maps to an output field of calc `A`
- **THEN** the orchestrator SHALL order evaluation so `A` writes its output slot before `B` reads it, within the same cycle

#### Scenario: Calc reads out of its registered range

- **WHEN** a calc's `eval` accesses a tape index outside the `input_indices` it registered at `init`
- **THEN** the result is undefined and the calc is considered incorrect

### Requirement: Calcs declare a name-based input map resolvable at init

Each calc SHALL expose `get_input_map()` returning a map from its local input names (e.g. `v`, `setpoint`) to fully-qualified source names (e.g. `peripheral_0.output_1`, `calc_pid.out`). `update_input_map(field, source)` SHOULD be called only before `init`. Concrete impls (e.g. `Butter2`, `Pid`) mutate the name field unconditionally; if called after `init` the stored name and the resolved tape index go out of sync with no error returned, and the calc continues reading from the original tape index.

<!-- FOLLOW-ON: `update_input_map` could return `Err` (or be a documented no-op) when called after `init` to make the post-init misuse explicit. Not a gate on this bootstrap. -->

Reference: `Calc::get_input_map`, `Calc::update_input_map`, `Calc::get_input_names`, `Calc::get_output_names` in `software/deimos/src/calc/mod.rs`.

#### Scenario: User re-routes a PID input

- **WHEN** a user calls `update_input_map("setpoint", "seq.setpoint")` on a `Pid` calc before the run starts
- **THEN** the orchestrator SHALL resolve `seq.setpoint` to its tape index at `init` and the PID MUST consume that value each cycle

#### Scenario: Unresolvable source at init

- **WHEN** an input map references a source name no other entity produces
- **THEN** the orchestrator MUST fail `init` with an error identifying the missing source

### Requirement: Calcs are serializable, clonable, and configurable by name

A `Box<dyn Calc>` SHALL round-trip through `typetag::serde` preserving its concrete type. Calcs MUST expose their configuration as a `BTreeMap<String, f64>` via `get_config` and accept it via `set_config`. A `save_outputs` flag MUST let users opt calc outputs in or out of dispatcher streams per calc.

Reference: `#[typetag::serde(tag = "type")]`, `Calc::get_config`, `Calc::set_config`, `Calc::get_save_outputs`, `Calc::set_save_outputs`; helper macros `calc_config!`, `calc_input_names!`, `calc_output_names!` in `software/deimos/src/calc/mod.rs`.

#### Scenario: Serialize a controller configuration with calcs

- **WHEN** a controller containing a `Butter2` and a `Pid` is serialized to JSON and deserialized
- **THEN** both calcs MUST reconstruct with the same concrete types, input maps, and config values

### Requirement: Deimos ships a library of built-in calc types

The built-in calc set SHALL include, at minimum: `Affine`, `InverseAffine`, `Constant`, `Sin`, `Polynomial`, `Butter2`, `Pid`, `RtdPt100`, `TcKtype`, and `SequenceMachine`. These types MUST implement `Calc` and be available for use without additional dependencies.

Reference: `software/deimos/src/calc/{affine,inverse_affine,constant,sin,polynomial,butter,pid,rtd_pt100,tc_ktype}.rs` and `software/deimos/src/calc/sequence_machine/`.

#### Scenario: Temperature calc available out of the box

- **WHEN** a user adds a K-type thermocouple channel
- **THEN** a `TcKtype` calc MUST be available to convert raw thermocouple voltage plus a cold-junction reference to temperature without requiring custom code

### Requirement: Calcs reset between runs

Before a new run starts, every calc SHALL have `terminate()` invoked to clear state, and `init(...)` invoked again before the next `Operating` cycle begins. Persistent state (filter history, integrator state, sequence machine position) MUST NOT leak across runs. Invalid `init` preconditions are a fail-fast panic, not a recoverable error: `Butter2::init()` and `Pid::init()` use `assert!()` for the `dt_ns > 0` check and panic the controller process on violation.

<!-- FOLLOW-ON: A future change proposal could convert the `dt_ns > 0` (and similar) preconditions to `Result::Err`, aligning calc init with the Result-based error contract used elsewhere. The current panic-on-bad-precondition behavior is intentional fail-fast for now. -->


Reference: `Calc::init`, `Calc::terminate` in `software/deimos/src/calc/mod.rs`.

#### Scenario: IIR filter state cleared between runs

- **WHEN** a `Butter2` calc is used in run N and then the controller starts run N+1
- **THEN** the filter's internal state MUST be re-initialized to the same baseline it had at the start of run N
