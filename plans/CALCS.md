# Calc execution design

## Scope

This document defines the interface and ownership boundary between serializable
`Calc` definitions and `CalcOrchestrator` runtime execution.

## Goals

- Separate persistent configuration from per-run mutable state.
- Keep calcs independent of the global tape layout.
- Reuse configured calcs across multiple runs.
- Allow dynamic configuration and shapes before a run while fixing the
  execution layout before the control loop starts.
- Keep evaluation bounded, allocation-free, and panic-free.

## Anti-goals

- Changing calc configuration or graph topology during a run.
- Giving calcs access to the orchestrator or global tape.
- Selecting saved outputs at the calc-definition level.
- Supporting cyclic calc graphs.

## Interface

`Calc` is a serializable definition. `CalcFn` is the initialized evaluator for
one run.

```rust
pub type CalcFn = Box<
    dyn FnMut(&[f64], &mut [f64]) -> Result<(), String>
        + Send
        + Sync
        + 'static,
>;

#[typetag::serde(tag = "type")]
pub trait Calc: Send + Sync + Debug {
    /// Validate configuration and create fresh state for one run.
    fn init(&self, ctx: ControllerCtx) -> Result<CalcFn, String>;

    /// Map every calc input name to its source field.
    fn input_map(&self) -> BTreeMap<CalcInputName, FieldName>;

    /// Return input and output names in evaluation order.
    fn names(&self) -> (Vec<CalcInputName>, Vec<CalcOutputName>);

    fn kind(&self) -> String {
        type_name::<Self>()
            .split(':')
            .next_back()
            .unwrap()
            .into()
    }
}
```

The slices passed to `CalcFn` follow the ordering returned by `Calc::names`.
A calc never receives global tape indices or ranges.

## Ownership and lifecycle

A `Calc` stores configuration only and is serialized as part of the controller
configuration. Calling `Calc::init` creates a closure containing fresh mutable
state for that run. Configuration needed during evaluation is copied or cloned
into the closure.

Initialization must not depend on a previous run. Dropping `CalcFn` drops its
run state, so `Calc` has no `terminate` method. Initialization failure likewise
drops any evaluators already created while assembling the run.

`CalcFn` is owned and `'static`, allowing the orchestrator to store evaluators
alongside calc definitions without borrowing them or becoming
self-referential.

## Orchestrator runtime

`CalcOrchestrator` owns the global `f64` tape and all routing information. Each
initialized calc has this runtime entry:

```rust
struct RunningCalc {
    evaluator: CalcFn,
    input_indices: Vec<usize>,
    inputs: Vec<f64>,
    output_range: Range<usize>,
}
```

During initialization, the orchestrator resolves each input source, assigns a
contiguous output range, allocates the input buffer, and creates the evaluator.

During each cycle, it copies the source values from the tape into `inputs`,
passes `inputs` and the mutable output slice to the evaluator, then proceeds to
the next calc in dependency order. This gives calcs contiguous data while
keeping tape layout and borrowing inside the orchestrator.

Copying inputs is a deliberate departure from zero-copy evaluation. It isolates
the user-facing calc API from the orchestrator's tape storage and routing
details. The expected cost is small because gathering noncontiguous tape values
already requires sparse indexed reads; the copy only writes those gathered
values into a contiguous buffer.

## Configuration and shape contract

Configuration may determine input mappings and input/output shapes before a
run. Successful initialization freezes the complete execution layout until the
run ends.

The keys of `input_map` must exactly match the input names returned by `names`.
Input and output names must be unique within their respective lists, and every
mapped source must exist and be available before the calc executes. The
orchestrator rejects contract violations and cyclic dependencies before
entering the control loop.

Every `CalcFn` invocation receives slices with the frozen input and output
lengths. It must write every output on every successful invocation.

## Naming and serialization

Every concrete `Calc` implementation needs a globally unique typetag name. By
default this is derived from the Rust struct name, so calc struct names must be
globally unique unless an explicit typetag name is provided.

This type identity is separate from the calc instance name. Instance names
must be unique within a controller because they prefix output field names.

Only calc definitions are serialized. Evaluators and all other per-run state
are not.

## Output retention

All declared calc outputs are exposed to dispatchers. Applications use a
channel-filter dispatcher when they need to retain only a subset.

## Realtime contract

`CalcFn` runs synchronously in the control loop. It must not allocate, block,
perform I/O, or panic, including for NaN or infinite inputs. Work such as
validation and construction belongs in `init`.

An evaluator that cannot continue returns an error. If constructing a `String`
error would violate the allocation contract, the error must be prepared during
initialization or the interface must adopt an allocation-free error type.

The orchestrator also performs no allocation during evaluation; it allocates
the tape, routing state, and input buffers before operation begins.

## Future unit-of-measure system

The calc interface does not expose unit metadata. Channel names and calc
documentation describe expected physical quantities for now.

A future major version is planned to add a cohesive unit-of-measure
documentation and validation system rather than unvalidated string labels.
