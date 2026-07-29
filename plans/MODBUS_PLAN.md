# Rev7 Modbus/TCP Implementation Plan

## Status

Phases 1 through 4 are implemented. The Phase 1 hardware-test checkpoint is
`e60e8e5`, the Phase 3 handoff checkpoint is `aac4013`, and the Phase 4 handoff
checkpoint is `362f295`. Results are recorded in
`plans/MODBUS_PHASE1_REPORT.md`, `plans/MODBUS_PHASE2_REPORT.md`,
`plans/MODBUS_PHASE3_REPORT.md`, and `plans/MODBUS_PHASE4_REPORT.md`. The
equipment-assisted identity-first calibration run and physical pre-rev7
compatibility run remain deferred to the final Phase 5 hardware verification.
Phase 5 acquisition timestamps and release verification are next.

This is the implementation plan for adding a Modbus/TCP operating mode to
`firmware/deimos_daq_rev7` while keeping the existing Deimos UDP operating mode.

This plan intentionally changes the nominal Deimos operating path before adding
the Modbus protocol. The shared result is one cycle-driven engineering snapshot
which is sent as the normal UDP output packet in Deimos mode and exposed through
Modbus registers in Modbus mode.

## Goals

- Publish coherent ADC channel groups rather than copying independently updated
  ADC atomics.
- Associate each published ADC group with its board acquisition timestamp and
  carry that timestamp through the common engineering snapshot.
- Apply rev7 calibration and hardware-specific engineering conversions in
  firmware in both operating modes.
- Use one `ByteStruct` engineering snapshot as the source of truth for both the
  existing UDP response and future Modbus reads.
- Put a packet-type magic value at the beginning of every state packet and
  validate packet length, magic, and fields at both ends of the connection.
- Move reusable pure calculation functions into a new no-std
  `deimos_shared::calcs` module and make the existing `deimos::calc` APIs delegate
  to or re-export them.
- Add one statically allocated smoltcp TCP socket and its storage before adding
  any Modbus protocol processing.
- After the baseline is complete, characterize whether a sample-per-cycle path
  can extend the supported cycle rate while retaining the free-running 33 kHz
  sampler below a measured, alias-safe cutover.
- Reuse the existing operating entrypoint, cycle timer, filter-cutoff policy,
  loss-of-contact counter, output handling, and address-management behavior.
- Make the eventual difference between Deimos and Modbus operation primarily
  the transport and request/response parser.

## Fixed design decisions

The following are requirements rather than open design questions:

- Do not add a `NetworkServing` board state.
- In the baseline free-running mode, do not move ADC filter construction or
  installation out of the sampling interrupt. The current
  `ADC_CUTOFF_RATIO`/`NEW_ADC_CUTOFF` handshake remains. A successful Phase 6
  keeps that path intact; its alternative path has no ADC IIR and configures
  only the rate-specific fractional-delay filter when its sampling scope starts.
- Do not create independent Modbus publication, filter-cutoff, and timeout
  clocks. The operating cycle is the publication cycle, and the operating cycle
  rate is also used as the ADC filter cutoff exactly as it is in Deimos mode.
- Use the existing loss-of-contact cycle count as the only connection timeout.
- Default Modbus operation to 10 Hz and a one-minute timeout when the first
  accepted Modbus request does not write those fields. At 10 Hz the default
  `loss_of_contact_limit` is 600 cycles.
- Preserve the most recently applied outputs when a Modbus cycle-rate change
  causes the operating state to exit and re-enter.
- In Modbus mode, retain the last successfully written output values across
  reads and writes which do not include an output update. Only a later explicit
  output write or the existing loss-of-contact transition changes them.
- Externally publish engineering values, not the raw ADC-voltage array.
- Publish RTD input resistance for the three external RTD channels. Finish those
  three resistance-to-temperature calculations in software.
- Publish board temperature in firmware because it is also the thermocouple
  cold-junction input.
- Use `f32` for calibration coefficients, firmware-side engineering
  calculations, spline and polynomial evaluation, filter state, and transmitted
  engineering values. Software upcasts received/shared results to `f64` at its
  boundary.
- Use offline-fitted regular-grid cubic B-splines for the two runtime K-type
  conversion directions rather than evaluating the NIST piecewise functions.
- Use the published IEC 60751 Callendar--Van Dusen relationship directly for
  forward Pt100 conversion. Use one offline-fitted global inverse polynomial
  for the realtime resistance-to-temperature conversion rather than fitting
  the existing rounded resistance table or solving the inverse at runtime.
- Support one Modbus TCP connection initially.
- Accept and echo any one-byte Modbus Unit Identifier.
- Carry an explicit firmware-calibrated flag in the included rev7 calibration
  and report it in a rev7-specific Deimos configuring response. Normal Deimos
  operation requires it to be set; Modbus/TCP is not served while it is clear.
- There is no backwards-compatibility requirement for the rev7 packet change.
  All existing rev7 hardware will be reflashed and recalibrated as a coordinated
  firmware/software rollout.

## Packet identity and validation

Every rev7 `ByteStruct` packet used by `Binding`, `Configuring`, and `Operating`,
in both directions, begins with a fixed-width packet-type magic value. Use a
different nonzero `u32` magic for every packet type and direction rather than a
single protocol-wide magic. The first field of `OperatingSnapshot` is therefore
also its magic; the Modbus snapshot register view exposes it as an identity and
layout check. Do not change the shared packet layouts still used by pre-rev7
firmware.

Binding occurs before the controller knows the responding model, so discovery
supports both framings without treating old rev7 as compatible:

- broadcast the existing shared Binding request for pre-rev7 devices;
- also broadcast the magic-bearing rev7 Binding request;
- new rev7 firmware responds only to the rev7 request with its magic-bearing
  rev7 Binding response;
- retain parsing of the existing shared Binding response for pre-rev7 models.

After Binding identifies the model, use the existing device-specific
configuring and operating hooks to select the rev7 magic-bearing packets or the
unchanged pre-rev7 packets as appropriate.

Define the magic constants beside the packet types in `deimos_shared`. Each
receive path performs these checks before a packet can affect state:

1. Require exactly the packet type's `BYTE_LEN`; reject both truncation and
   trailing data.
2. Deserialize without panicking.
3. Require the packet-type magic expected in the current state and direction.
4. Validate enums, finite numeric values, ranges, IDs, and state-specific
   invariants.

Constructors and explicit `Default` implementations must install the correct
magic; do not rely on a derived zero-filled default. An invalid packet does not
transition state, update outputs, or reset loss of contact. Test each packet's
golden encoding and representative wrong-magic, wrong-length, and invalid-field
cases; no generic validation framework is required.

This is deliberately fail-fast rather than version-negotiated. Do not add a
decoder for an obsolete rev7 packet layout. Continuing to parse the existing
pre-rev7 packet types is support for those devices, not rev7 compatibility.

Keep the shared Binding and Configuring responses used by pre-rev7 devices free
of calibration fields. Add a rev7-specific configuring response containing the
normal acknowledgement plus one byte named `firmware_calibrated`. Define `0` as
uncalibrated and `1` as calibrated; treat any other value as uncalibrated.

```rust
pub struct Rev7ConfiguringOutput {
    pub magic: u32,
    pub acknowledge: AcknowledgeConfiguration,
    pub firmware_calibrated: u8,
}
```

Use the existing `Peripheral` configuring size/emit/parse hooks for the rev7
response, and fix controller paths which currently validate the device-specific
size but then deserialize the common `ConfiguringOutput` directly. The normal
Deimos controller refuses to enter Operating when the rev7 field is not `1` and
reports an explicit “unit requires calibration” error. The rev7 calibration
collection path follows the opposite rule: it requires `0` and proceeds, which
prevents accidentally calibrating an already calibrated unit. Do not add a
general-purpose “ignore calibration state” option.

## Terminology and state model

Use distinct top-level states for the two externally meaningful operating
protocols while retaining one shared operating implementation. Rename the
current unit-like `Operating` state to `OperatingDeimos` and add a data-carrying
`OperatingModbus` state:

```rust
enum BoardState {
    Connecting,
    Binding,
    Configuring,
    OperatingDeimos,
    OperatingModbus(ModbusInitialConfig),
}

enum OperatingMode {
    Deimos,
    Modbus(ModbusInitialConfig),
}
```

`BoardState` remains the persistent state-machine representation;
`OperatingMode` is only the private argument which configures one invocation of
the shared operating loop. The top-level dispatch maps both states to that
function:

```rust
BoardState::OperatingDeimos => self.operate(OperatingMode::Deimos),
BoardState::OperatingModbus(initial_config) => {
    self.operate(OperatingMode::Modbus(initial_config))
}
```

Do not keep a second stored mode alongside `BoardState`. Make
`OperatingOutputSettings`, `ModbusInitialConfig`, `OperatingMode`, and
`BoardState` `Clone + Copy`: they contain only fixed-size scalar/array values,
so this preserves the current direct `match self.state` dispatch without a
take/replace mechanism or another configuration owner. The current `Eq` derive
on `BoardState` must be removed because the new configuration contains `f32`
outputs; retain `PartialEq` only if it is useful to tests.

In particular, a Modbus cycle-rate update returns:

```rust
BoardState::OperatingModbus(initial_config)
```

This is the concrete interpretation of “reuse the existing OperatingRoundtrip
entrypoint”: both top-level operating states call the same `operate` function,
and the common loop is not duplicated. This does not add a `NetworkServing`
state.

State transitions will be:

```text
Connecting -> Binding -> Configuring -> OperatingDeimos
                    \
                     +-- first accepted Modbus read or write
                         -> OperatingModbus(initial_config)

OperatingModbus -- cycle-rate update --> OperatingModbus(updated config)
OperatingDeimos -- loss of contact ----> Connecting
OperatingModbus -- loss of contact ----> Connecting
```

`Binding` will poll the already-created UDP and TCP sockets. A successful Deimos
bind continues to `Configuring`; the first valid supported Modbus read or write
enters `OperatingModbus(initial_config)`. A read enters with the complete default
configuration and safe output values. A write starts from those defaults and
overlays the complete fields supplied by that request. This provides the
protocol-selection point directly in the top-level state machine. When rev7 is
uncalibrated, Binding continues to serve the Deimos UDP handshake for the
calibration workflow but does not listen on the Modbus TCP port. Only one state
owns outputs at a time.

## Common data path

The target data flow is:

```text
TIM2 sampler at 33 kHz
    -> publish filtered ADC group and acquisition timestamp into ADC double buffer
    -> operating-cycle snapshot publisher
         - copy coherent ADC group
         - read counters, frequencies, and digital inputs
         - apply per-device calibration
         - perform channel engineering conversions
         - advance the 1 Hz board-temperature filter
         - calculate thermocouples using filtered board temperature
         - serialize one OperatingSnapshot
    -> Deimos: send OperatingSnapshot over UDP
       Modbus: retain OperatingSnapshot for register reads
```

Snapshot publication occurs once per nominal operating cycle in both modes.
Network reads never advance filters or cause measurements to be recalculated.
The diagram describes the baseline free-running sampler. If Phase 6 passes its
measurement gate, the high-rate path invokes the same acquisition/alignment and
double-buffer publication code once from the SysTick scope immediately before
the common snapshot publisher; the downstream data path does not change.

## Engineering snapshot contract

Add a device-specific `ByteStruct` packet in
`deimos_shared::peripherals::deimos_daq_rev7`. `OperatingSnapshot` is the
canonical name in this plan; replace the obsolete raw-output packet rather than
retaining a compatibility alias.

The snapshot should contain the existing `OperatingMetrics` plus these final
measurement fields:

```rust
pub struct OperatingSnapshot {
    pub magic: u32,
    pub metrics: OperatingMetrics,
    pub sample_time_ns: i64,             // acquisition time of this ADC group

    pub module_bus_current_a: f32,       // ain0
    pub module_bus_voltage_v: f32,       // ain1
    pub board_temperature_k: f32,        // ain2, filtered at 1 Hz
    pub current_4_20_a: [f32; 4],        // ain3..ain6
    pub rtd_resistance_ohm: [f32; 3],    // ain7..ain9
    pub thermocouple_temperature_k: [f32; 2], // ain10..ain11
    pub voltage_v: [f32; 6],             // ain12, ain15..ain19

    pub encoder: i64,
    pub pulse_counter: i64,
    pub frequency_meas_hz: [f32; 2],
    pub gpio: u8,
}
```

The bus current and voltage remain available as final engineering telemetry,
preserving the useful outputs currently produced by `standard_calcs`. No
intermediate sense voltages, thermocouple voltages, or external RTD
temperatures are included.

`sample_time_ns` is the board timestamp captured immediately before the first
ADC conversion group represented by the snapshot. It is distinct from
`metrics.cycle_time_ns` and `metrics.sent_time_ns`, which describe snapshot
publication rather than acquisition. The timestamp and all ADC values are one
double-buffer payload and therefore always come from the same published sampler
iteration. This field describes the final packet layout; Phases 1--4 use the
otherwise-identical snapshot without it, and Phase 5 updates the packet length,
golden encoding, host parser, and Modbus register map together.

The field ordering is a wire contract. Add byte-length assertions, validate the
snapshot magic before parsing its fields, and add golden serialization tests.
The Modbus register map will explicitly translate these
little-endian `ByteStruct` fields into Modbus's defined register byte/word order;
it must not reinterpret the packet bytes directly.

For common metric fields in Modbus mode:

- `metrics.id` is the monotonically wrapping snapshot/publication ID.
- `cycle_time_ns` is the start of the publication cycle.
- `sent_time_ns` is the snapshot publication time, even though a later Modbus
  request may read it.
- `last_input_id` is the most recently accepted 16-bit Modbus transaction ID,
  zero-extended into the existing field.
- `last_input_received_time_ns` is updated for each accepted complete Modbus
  request.

## Calibration binary format and inclusion

Add compact wire types alongside the rev7 packet definitions, for example:

```rust
#[derive(ByteStruct, Clone, Copy, Debug)]
#[byte_struct_le]
pub struct LinearCalibration {
    pub slope: f32,
    pub offset: f32,
}

#[derive(ByteStruct, Clone, Copy, Debug)]
#[byte_struct_le]
pub struct Rev7Calibration {
    pub firmware_calibrated: u8,
    pub voltage_cals: [LinearCalibration; ADC_CHANNEL_COUNT],
}
```

The existing JSON `LinearCal` remains human-readable `f64`. Binary generation
rejects non-finite or unrepresentable coefficients and converts them to `f32`.
Firmware never upcasts these coefficients during runtime.

The binary record is generated from the existing JSON calibration and included
in firmware similarly to `serialnumber.in` and `macaddr.in`:

```rust
const CALIBRATION_BYTES: &[u8; Rev7Calibration::BYTE_LEN] =
    include_bytes!("../../static/calibration.in");
```

The fixed array length makes a missing or incorrectly sized file a build error.
The generation tool validates the coefficients; firmware simply deserializes
the included record once at startup. It treats only
`firmware_calibrated == 1` as calibrated. The identity artifact contains
identity coefficients and `firmware_calibrated = 0`; the tool which emits a
complete fitted calibration sets it to `1` only after all required channel fits
are present.

Extend the rev7 calibration tooling to emit `calibration.in` using
`Rev7Calibration::write_bytes`, while retaining the human-readable JSON record.
One serialization round-trip test is sufficient. Updating calibration over
Modbus is not part of the initial implementation.

### Calibration-run workflow after engineering conversion

Calibration capture will intentionally use the normal engineering snapshot; it
does not require a second raw-ADC telemetry format. The workflow assumes each
calibration run starts with a known identity `Rev7Calibration`:

1. Flash an explicitly generated identity-calibration image.
2. Require the rev7 configuring response to report
   `firmware_calibrated = 0`, then start the calibration run. No firmware
   version, coefficient, or unit-identity matching is required.
3. Upcast received packet values to `f64`, then invert the final monotonic
   engineering conversion in software to recover the frontend sense voltage
   used to fit each affine channel calibration.
4. Fit and validate calibration coefficients in `f64`, emit the checked `f32`
   binary calibration with `firmware_calibrated = 1`, flash the calibrated
   image, and perform a final end-to-end verification run through the normal
   engineering packet.

The inverse used by the runner must be the algebraic or shared-calculation
inverse of the exact firmware conversion:

- multiply each 4--20 mA result by 75 ohm;
- multiply each external RTD resistance by 250 uA;
- invert board temperature with the shared `pt100_resistance_ohm` helper and
  multiply by 250 uA;
- use the same-snapshot board temperature and shared K-type temperature-to-
  voltage spline to undo cold-junction compensation and recover sensed
  thermocouple voltage;
- invert each voltage channel's documented final gain/offset stage.

For thermocouples, the explicit inverse is
`sensed_voltage = E(published_hot_temperature) - E(published_board_temperature)`.
Sequence calibration and verification so shared/reference channels needed by a
dependent conversion are known.

This relies on every final-stage mapping being monotonic and invertible over its
supported calibration domain. The calibration procedure excludes saturation,
non-finite results, and values outside that domain. Board-temperature and
thermocouple capture steps wait for the 1 Hz board-temperature filter to settle
and use values from the same snapshot. Recalibration never composes a new fit on
top of unknown coefficients: flash the identity-calibration firmware
immediately before starting the run.

## Shared calculation module

Create `software/deimos_shared/src/calcs/mod.rs` and expose it from
`deimos_shared::lib`:

```rust
pub mod calcs;
```

The module contains pure, allocation-free, no-std functions which are reusable
by firmware and host software. Runtime inputs, coefficients, intermediate
values, and outputs are `f32`. Initial candidates are:

- `affine(x, slope, offset)` and/or the minimal affine helpers needed by the
  channel conversion pipeline;
- `pt100_temp_k(resistance_ohm)` for board temperature;
- `pt100_resistance_ohm(temperature_k)` for calibration and validation;
- `ktype_temp_k(voltage_v)`;
- `ktype_voltage_v(temperature_k)`;
- `ktype_corrected_temp_k(sensed_voltage_v, cold_junction_temperature_k)`.

Do not move the dynamic `Calc` trait, named-input graph, serde configuration, or
stateful calc wrappers into `deimos_shared`. Replace the current `once_cell`,
heap-backed interpolation setup, and K-type piecewise runtime functions with a
common fixed-size regular-grid cubic B-spline evaluator and generated `f32`
spline definitions. Replace the dense RTD runtime tables with the direct
Callendar--Van Dusen implementation. Define out-of-range and non-finite behavior
explicitly rather than relying on unwraps.

The existing `deimos::calc::RtdPt100` and `TcKtype` objects remain host-side
wrappers. Their `eval` methods call the shared `f32` functions and upcast the
result to the controller tape's `f64`. Where the existing public free-function
API must retain `f64` signatures, keep only thin downcast/call/upcast adapters in
`deimos::calc`; otherwise re-export the shared functions directly. Do not retain
a second implementation or coefficient set in `deimos`.

```rust
pub use deimos_shared::calcs as shared_calcs;

pub fn ktype_corrected_temp_k(voltage_v: f64, cold_junction_k: f64) -> f64 {
    shared_calcs::ktype_corrected_temp_k(voltage_v as f32, cold_junction_k as f32) as f64
}
```

Apply the same adapter pattern to the other shared functions.

Remove duplicate source tables and polynomial implementations from the host
calc modules after parity tests pass. `interpn` remains available for unrelated
sequence-machine lookup functionality.

### Pt100 Callendar--Van Dusen forward and global polynomial inverse

Use the nominal IEC 60751 relationship and coefficients directly. With
temperature `t` in degrees Celsius and `R0 = 100 ohm`:

```text
R(t) / R0 = 1 + A*t + B*t^2                         for t >= 0
R(t) / R0 = 1 + A*t + B*t^2 + C*(t - 100)*t^3     for t < 0

A = 3.9083e-3 / degC
B = -5.775e-7 / degC^2
C = -4.183e-12 / degC^4
```

The supported standard range is -200 to 850 degrees Celsius. Implement
`pt100_resistance_ohm` as direct evaluation of this relationship after the
Kelvin-to-Celsius conversion. This forward function is used by host calibration
and validation code and is not the firmware's in-loop board-temperature path.
Do not estimate new forward coefficients from the two-decimal source table:
doing so would make the implementation follow table rounding and transcription
error instead of the standard curve.

Generate `pt100_temp_k` as one polynomial covering the entire corresponding
resistance range, approximately 18.52 to 390.48 ohm. There must be no
below/above-zero partition, coefficient-table selection, Newton solve, square
root, or data-dependent iteration in the supported-range runtime path. Store a
single input origin and scale, output origin, and coefficient array, and use the
form:

```text
u = (resistance_ohm - resistance_origin_ohm) * resistance_scale
temperature_k = temperature_origin_k + horner(coefficients, u)
```

Choose the origins so `u` is centered and scaled to approximately `[-1, 1]` over
the complete resistance domain. Generate exact fit pairs by sweeping temperature
and evaluating the forward Callendar--Van Dusen function; neither the rounded
table nor an inverse solver is needed to generate the reference. Fit in `f64` in
a numerically conditioned basis, using a linear least-squares fit as a seed and
a minimax/Remez-style refinement if needed, then emit the equivalent compact
`f32` constants used by the fixed Horner chain. Sweep polynomial degree and
select the lowest global degree whose emitted runtime evaluator satisfies all
value-error, monotonicity, derivative, and forward/inverse round-trip
requirements. Do not introduce range partitions if a candidate degree fails;
increase the global degree or improve the fit.

Validate strict error bounds using bounded local extrema searches over the
complete curve, with multiple seeds and explicit checks at -200, 0, and 850
degrees Celsius. Run the check against both the fitted `f64` polynomial and the
generated `f32` Horner evaluator, including coefficient reduction and operation
rounding in the error budget. The maximum inverse temperature error must not
exceed `0.01 K` anywhere in the standard domain, and the polynomial derivative
must remain positive throughout it.

Outside the standard resistance range, apply the exact inverse endpoint tangent
as linear extrapolation, matching the requested linearized-extrapolation
behavior. This is the only range-dependent selection in the calculation; the
entire supported measurement range uses the same polynomial. Define NaN and
infinity behavior explicitly.

Check in the fitting generator, generated constants, and a report containing
the degree, origins and scale, `f64` and emitted-`f32` maximum/RMS error,
monotonicity and derivative results, forward/inverse round-trip error, and
endpoint-extrapolation checks.

### Offline K-type B-spline fitting and generated runtime form

Add a reproducible host-side generator which fits regular-grid cubic B-splines
in `f64` using `interpn.MultiBsplineRegular` inside a SciPy
Levenberg--Marquardt solve. Emit `interpn`'s precomputed coefficients as the
compact `f32` runtime definitions in `deimos_shared`. Check in both the
generator and generated Rust source so firmware builds do not perform fitting
and do not need an allocation feature.

Fit the two monotonic K-type mappings needed in the runtime path:

- K-type temperature to thermoelectric voltage, using the NIST piecewise
  “polynomial” as the dense reference source;
- K-type thermoelectric voltage to temperature.

Both K-type directions are needed for cold-junction compensation on every
cycle. Each fit replaces runtime branching across NIST regions with
`interpn::MultiBsplineRegular`'s regular-grid index calculation and evaluation
of four adjacent coefficients.

The generator should sweep a modest range of coefficient counts and select the
smallest count which satisfies documented value-error, derivative-quality,
monotonicity, and inverse-consistency limits. Start the forward search in the
tens and allow the inverse search to extend through the low hundreds rather
than assuming either final count.

Set the maximum interpolation error limit to `0.01 K`, including the error from
the generated `f32` coefficients and evaluator. Apply this directly to mappings
whose output is temperature. For a temperature-to-voltage mapping, convert its
voltage residual through the high-precision inverse reference and enforce the
same `0.01 K` temperature-equivalent limit. Coefficient-count selection may
use a smaller internal target, but never a larger reported bound.

A dense validation grid is useful for diagnostics but is not sufficient to
claim a strict error bound. For every regular-grid span, seed bounded local
optimizations within the span to locate local maxima of both signs of the
error. Include the span endpoints, spline knots, source
function branch boundaries, and other known nonsmooth reference points as
explicit candidates. Bracket and solve all detected stationary points of the
signed error; use multiple interior seeds/subdivisions rather than assuming a
unimodal error function. Run this extrema search against both the `f64` fitted
spline and the generated `f32` runtime evaluator. Account for optimizer/root
solver tolerance with a conservative acceptance margin, and reject a fit if the
certified maximum could exceed its specified absolute or relative bound.

Error is measured against the continuous NIST reference functions. Check in a
fit report containing:

- reference source/version and supported domain;
- grid origin, spacing, boundary convention, and number of coefficients;
- maximum value error found by interval-local optimization and RMS error on a
  much denser validation grid;
- first-derivative error/smoothness metrics;
- monotonicity results;
- forward/inverse round-trip error;
- the error added when coefficients and evaluation are reduced to `f32`.

The runtime representation consists only of domain bounds, grid origin and
spacing, and a fixed `f32` coefficient array. The evaluator constructs a
borrowed `interpn::MultiBsplineRegular` view and performs O(1) indexing and one
cubic evaluation with no heap, table search, or interior piecewise-function
branch. Boundary handling uses `interpn`'s zero-third-derivative ghost-
coefficient convention. With the `linearize_extrapolation` flag set, finite
inputs outside the supported physical domain use the endpoint value plus the
endpoint tangent times distance outside the domain. Use that policy
consistently in firmware and software, and test continuity of both value and
first derivative at each boundary. NaN and infinity behavior remains explicit
and is not treated as ordinary extrapolation.

## Rev7 engineering conversion pipeline

Implement one compact firmware function which consumes a coherent filtered ADC
group and the loaded calibration, and produces the analog portion of
`OperatingSnapshot`. Preserve the current standard-calc ordering:

1. Convert the sampler's ADC values to the relevant frontend sense voltage.
2. Apply the channel's affine voltage calibration at the same point in the
   pipeline used by the current software standard calcs.
3. Produce the final channel value:
   - ain0: module bus current using the 6 mohm shunt and gain of 50;
   - ain1: module bus voltage using the existing divider ratio;
   - ain2: board Pt100 resistance and temperature, then the existing 1 Hz
     second-order filter;
   - ain3..ain6: calibrated sense voltage divided by 75 ohm;
   - ain7..ain9: calibrated sense voltage divided by 250 uA, stopping at
     resistance;
   - ain10..ain11: calibrated K-type voltage with cold-junction compensation
     from filtered board temperature;
   - ain12 and ain15: calibrated 0--2.5 V values;
   - ain16..ain17: gain-six voltage conversion followed by calibration;
   - ain18..ain19: inverse gain/offset conversion followed by calibration.

Perform the complete firmware conversion pipeline in `f32`, including affine
calibration, frontend conversion, B-spline evaluation, cold-junction
compensation, and the board-temperature filter. The packet therefore contains
the values produced by the firmware without a precision-changing final cast.
The software parser immediately upcasts packet values to its normal `f64` tape.

The board-temperature 1 Hz filter is operating-mode state, not sampling-ISR
state. Initialize it on operating entry and rebuild it when cycle rate changes.
Thermocouple calculation uses its filtered output from the same publication
cycle.

Select the board-temperature filter coefficients once on operating entry. At
cycle rates where a 1 Hz second-order low-pass is well-defined, install the
normal coefficients. At a future sufficiently low cycle rate, install identity
or passthrough coefficients in the same filter type. The publication hot path
always invokes the filter and contains no rate-dependent branch. Define the
cutover from the filter design's valid normalized-frequency range. The present
timer reload representation already limits operation to roughly 4 Hz, so this
is a planned fallback rather than a reason to redesign the current timer in this
work. Use the filter's normal initialization in either case. Any transient from
a rare cycle-rate change is documented; no continuity or status mechanism is
added.

On the host side, update `DeimosDaqRev7::output_names` and
`parse_operating_roundtrip` for the engineering packet. Rev7 `standard_calcs`
then becomes small:

- do not repeat conversions already performed in firmware;
- add only the three external Pt100 resistance-to-temperature calculations and
  no aliases for the removed raw ADC outputs;
- keep shared free-function re-exports for calibration code and other hardware.

This is a breaking rev7 wire change. Update firmware and software together, and
update calibration capture configurations and documentation that currently
refer to `p1.ain*` or intermediate standard-calc names.

## ADC double buffer

Replace the public `[AtomicF32; ADC_CHANNEL_COUNT]` latest-value array with a
two-slot atomic abstraction owned by the sampling subsystem, conceptually:

```rust
struct AtomicAdcSampleGroup {
    values: [AtomicF32; ADC_CHANNEL_COUNT],
    sample_time_lo: AtomicU32,
    sample_time_hi: AtomicU32,
}

struct AdcSampleDoubleBuffer {
    buffers: [AtomicAdcSampleGroup; 2],
    latest: AtomicBool, // false selects slot 0; true selects slot 1
}
```

The reader reconstructs an ordinary `AdcSampleGroup` containing the `f32` values
and `i64 sample_time_ns`. Phase 1 may initially omit the two timestamp words;
Phase 5 adds them without changing the publication protocol. Split the timestamp
into two atomic 32-bit words because this Cortex-M target does not provide native
64-bit atomics.

Required invariants:

- The sampling closure is the only writer and the only context which modifies
  `latest`.
- The writer loads `latest` with relaxed ordering, fills the other slot using
  relaxed atomic stores, executes a release `compiler_fence`, and publishes that
  slot with one relaxed `AtomicBool` store.
- The communication closure loads `latest` exactly once with relaxed ordering,
  executes an acquire `compiler_fence`, and copies every field from the selected
  slot with relaxed atomic loads. It does not recheck or modify the flag.
- Compiler fences prevent source-level reordering across publication but emit no
  Cortex-M hardware memory-barrier instruction. The relaxed `AtomicF32`,
  `AtomicU32`, and `AtomicBool` accesses compile to the same ordinary aligned
  loads and stores used by the current handoff.
- SysTick is the only reader and has higher priority than the TIM2 writer. If it
  preempts before publication, it reads the previous slot while TIM2 writes the
  other one. If it preempts after publication, the new slot is complete. TIM2
  cannot resume and reuse a slot until the reader has finished copying it.
- In sample-per-cycle mode, the same publication and copy occur sequentially in
  the SysTick scope and therefore satisfy the same contract.
- The reader returns an `AdcSampleGroup` by value. It must never return a
  reference into either static slot which could remain live after the
  communication interrupt returns.
- No critical section, interrupt masking, reader flag, reader guard, retry, or
  unsafe interior mutability is required.

Keep the implementation specific to the current single-core, single-writer,
single-higher-priority-reader interrupt model. Add a prominent comment that a
multicore platform, a writer capable of preempting the reader, or an additional
reader would require a triple buffer or a stronger ownership protocol to
preserve the no-wait read/write contract.

Update the nominal operating snapshot publisher to use the single-load reader
API. For filter-update steady-state initialization inside the sampling
interrupt, use the sampler's own `adc_values` directly rather than going through
the shared reader API. Do not change where the filter bank is constructed or
installed.

This double buffer guarantees that all ADC fields copied into one operating
snapshot came from one completed sampler iteration. Counter/frequency coherence
retains its current behavior; no additional synchronization is added.

## ADC acquisition timestamping

Implement acquisition timestamping as an independent fifth phase, after the
nominal and Modbus operating paths are stable. Use SysTick as both the
operating-cycle boundary and the within-cycle counter; do not add a TIM5 wrap
interrupt or transfer TIM5 ownership to the sampling interrupt.

Maintain a small operating acquisition-clock state containing:

```rust
struct AcquisitionClock {
    cycle_start_ns: i64,
    active_reload: u32,
}
```

Initialize `cycle_start_ns` from the board time on operating entry. At the start
of each higher-priority SysTick communication handler, advance it by the actual
duration of the completed SysTick interval and record the reload which was
loaded for the interval that has just started. Derive interval durations from
the applied timer ticks, including the SysTick `reload = ticks - 1` convention,
rather than repeatedly adding nominal `dt_ns`; this includes period adjustment
and timer quantization. Keep the active reload separate from a reload programmed
later in the handler for the next cycle. This acquisition clock does not change
the existing cycle labels or phase/period timing-control calculations.

Do not use acquire/release or sequentially consistent atomics to transfer this
state. SysTick already has higher priority than TIM2. Protect the ordinary
`AcquisitionClock` copy with a very short interrupt-masked critical section:

1. Check `SCB_ICSR.PENDSTSET`; abandon this attempt if SysTick is already
   pending.
2. Copy `AcquisitionClock` and read `SYST_CVR` through
   `SYST::get_current()` while interrupts remain masked.
3. Check `PENDSTSET` again. If it became set during the copy, restore interrupts
   so SysTick runs and retry.
4. Otherwise restore interrupts and calculate elapsed ticks as
   `active_reload - current_count` under the verified hardware convention.

Capture the timestamp immediately before starting the first ADC conversion
group. Bound the operation to two attempts: one normal capture and at most one
retry after a pending SysTick handler completes. There is no unbounded ISR loop.
If both attempts fail, publish the group normally with
`last_sample_time_ns + nominal_sample_period_ns`, where the nominal period is
derived from the configured sampling timer and rounded to nanoseconds. Do not
add a validity field, error counter, or downstream conditional path for this
case. Under the interrupt-priority and deadline assumptions, that fallback
sample will be superseded by later 33 kHz samples before the next snapshot is
published. Initialize `last_sample_time_ns` to one nominal sample period before
the operating-entry acquisition-clock anchor so the fallback is defined even
before the first successful counter capture.

Store the resulting `sample_time_ns` and the completed filtered ADC values in
the same `AdcSampleGroup`, then publish the double-buffer flag. The operating
snapshot publisher copies that timestamp without rereading any timer. Document
the timestamp as the acquisition-start instant; do not attempt to compensate it
for filter group delay.

Add target-independent tests for the counter/reload arithmetic and a small
model of the capture protocol covering a SysTick pending before the copy,
between the clock copy and counter read, after the counter read, and on both
bounded attempts. On hardware, compare timestamps against a GPIO marker around
the first conversion start and verify monotonic sample timestamps across normal
cycles and applied phase/period adjustments.

## TCP transport scaffold

In phase one, add transport capability but no Modbus frame handling:

- Add smoltcp's `socket-tcp` feature.
- Add one TCP socket handle to `Net`.
- Add statically allocated TCP RX and TX storage to `NetStorageStatic` and its
  initialization in `main.rs`.
- Allocate 512 bytes in each direction, enough for one maximum Modbus TCP ADU
  and bounded partial-frame accumulation.
- Construct the TCP socket in `Net::new` and add lifecycle helpers for listen,
  receive, send, close, and relisten.
- Reserve port 502 for the later Modbus service.
- Ensure existing DHCP/fallback address changes close or relisten the TCP socket
  consistently with the UDP reconnect behavior.

Phase one does not add `rmodbus`, parse requests, accept configuration, change
state based on TCP traffic, or expose a register map. The existing UDP behavior
must continue to work with an unused TCP socket present.

## Modbus operating configuration

Add an internal configuration which contains the fully resolved values needed
to enter or re-enter Modbus operation:

```rust
struct ModbusInitialConfig {
    dt_ns: u32,
    loss_of_contact_limit: u16,
    outputs: OperatingOutputSettings,
}
```

`OperatingOutputSettings` contains the four PWM duties and frequencies, two DAC
values, and four digital outputs. Reuse this type for the corresponding fields
of `OperatingRoundtripInput` so the two modes do not duplicate output handling.

External Modbus configuration may express cycle rate in hertz, but normalize it
to the existing `dt_ns` representation after finite/range validation. Report the
actual applied period/rate. Defaults are resolved from the first accepted
Modbus request:

- cycle rate omitted: 10 Hz (`dt_ns = 100_000_000`);
- timeout omitted: 60 seconds converted to a checked cycle count using the
  applied rate;
- output not written by that request: the corresponding existing safe/default
  output value.

If the first request is a read, all of these defaults apply. If it is a write,
overlay its complete fields on the defaults before entering Modbus operation.

Because `loss_of_contact_limit` is currently `u16`, reject configurations whose
requested rate/duration cannot be represented rather than silently wrapping.
After initial resolution, preserve the current cycle count unless a later write
explicitly changes it.

Do not add a staged copy, presence mask, or commit generation. Treat one Modbus
write request as the atomic update boundary. Once operating, overlay complete
fields on the current configuration, so omitted outputs retain their last
values while explicitly written zeroes remain zeroes. Reject writes which split
a multi-register value or otherwise do not match a documented writable
field/group, and validate the complete candidate before applying any part of it.

## Common operating implementation

Refactor `operate` around common per-cycle work and small mode-specific I/O:

```text
common entry
  - apply mode's dt, timeout, and initial outputs
  - initialize systick and publisher state
  - request the corresponding ADC cutoff

each cycle
  - account for loss of contact
  - publish one engineering snapshot
  - mode-specific send/read/respond
  - apply latest accepted outputs
  - maintain operating address policy
  - update timing metrics and feed watchdog
```

### Deimos mode

- Entry configuration comes from the existing UDP configuring state.
- Send the common `OperatingSnapshot` over UDP.
- Parse the existing `OperatingRoundtripInput`.
- Reset loss of contact only for a newly accepted input ID, preserving current
  behavior.
- Continue applying phase/period timing corrections.
- Preserve the existing reliability stance: each accepted roundtrip input is a
  fresh complete controller command for that cycle.

### Modbus mode

- Entry configuration comes from `ModbusInitialConfig`.
- This mode is unreachable unless `firmware_calibrated == 1`; an uncalibrated
  unit does not listen on the Modbus TCP port.
- Do not send unsolicited packets; retain the latest snapshot for reads.
- Poll the TCP socket and process complete Modbus requests.
- Accept every one-byte Modbus Unit Identifier and echo the request value
  unchanged in the response. Do not configure or infer a unit-ID allowlist.
- Reset loss of contact for every syntactically valid, supported, accepted
  Modbus request, whether read or write. Malformed or rejected requests do not
  count as contact.
- Do not apply Deimos phase/period timing corrections.
- Apply output changes only from a validated write request.
- Reads never mutate outputs. Repeated reads, and writes which omit some or all
  output fields, retain the last configured PWM, DAC, and GPIO values.
- An accepted read still resets loss of contact, so read-only polling can retain
  output authority. If accepted contact stops, the existing timeout transition
  to `Connecting` still applies safe outputs. This intentional reliability
  stance differs from Deimos roundtrip operation.

Document one outstanding Modbus request per connection. This avoids needing to
preserve partial or pipelined application frames across a rare cycle-rate
re-entry.

The bounded one-ADU receive buffer is owned by `Board` or `Net` so the complete
first request survives the `Binding -> OperatingModbus` transition. After
common operating initialization, the first normal operating cycle publishes a
snapshot, processes that request, and sends its normal response. This lets a
read-only client select Modbus mode and receive data at the default 10 Hz
without a separate setup write.

## Cycle-rate changes in Modbus mode

When an accepted Modbus write changes the cycle rate:

1. Finish generating/enqueuing the Modbus response.
2. Capture the latest fully applied output settings.
3. Build a new `ModbusInitialConfig` containing the new `dt_ns`, current
   `loss_of_contact_limit`, and captured outputs.
4. Request the new ADC cutoff using the existing cutoff mailbox.
5. Exit `operate` and return
   `BoardState::OperatingModbus(initial_config)`.
6. Re-enter the common operating initialization without resetting outputs to
   defaults.

The existing TCP socket remains owned by `Net`; do not add session snapshot or
migration logic around the state re-entry. The client must wait for the
rate-write response before sending another request. If the connection drops, it
uses the normal reconnect path. Document that behavior with the existing
skipped-sample and counter-reset effects.

The current ADC cutoff update runs in the sampling interrupt, pauses the sample
timer, skips one sample, and resets encoder/pulse-counter state. Cycle-rate
changes are expected to be very rare, so this plan deliberately keeps those
effects and adds no mitigation or continuity mechanism. Document them in the
register-map guide and release notes, and test that the observed behavior
matches the documentation.

## Modbus protocol phase

After phase-one support is stable:

- Add `rmodbus` with default features disabled and fixed-size response storage.
- Accumulate at most one bounded TCP ADU, handling partial TCP reads, invalid
  MBAP lengths, transmit backpressure, disconnect, and relisten. Require clients
  to wait for each response before sending the next request. In every
  Modbus-capable cycle, perform at most two TCP receive-buffer calls, two TCP
  transmit-buffer calls, two Ethernet frame receives, and two Ethernet frame
  transmits. Parse at most one complete ADU; do not use resynchronization scans
  or any unbounded packet-draining loop.
- Define a zero-based register map tested alongside the `OperatingSnapshot`
  definition.
- Support function code 04 for the immutable latest-snapshot input registers,
  function code 03 for reading configuration/output holding registers, and
  function code 16 for writing complete configuration/output fields or groups.
  Other data-access function codes are unsupported initially.
- Lay out the snapshot so it fits in one standard Modbus read and document that
  clients should read the full block in one request for a synchronized sample.
  Add a size assertion against the protocol's register-count limit.
- Encode each 16-bit register in Modbus network byte order. For `f32`, `u32`,
  and all 64-bit values, place the most-significant 16-bit register first.
  Floating-point registers contain the IEEE-754 bit pattern.
- Reject writes to read-only or unsupported addresses with standard Modbus
  exceptions.
- Service only the connection owned by the single TCP socket; additional clients
  are not supported.

The initial register map should expose at least:

- all fields from `OperatingSnapshot`, including its magic, snapshot ID, and
  timing/status metrics;
- current cycle period/rate;
- loss-of-contact limit and current loss counter;
- readable/writable current configuration and output fields.

Use standard Modbus exceptions for invalid addresses, lengths, functions, and
values instead of adding an application command-status protocol.

## Documented operational constraints

Keep these as operating instructions rather than firmware mechanisms:

- Deploy Modbus/TCP only on the trusted, isolated control network; the initial
  implementation does not add authentication or encryption.
- Connect one Modbus client and keep at most one request outstanding.
- Read the complete snapshot register block in one request when a synchronized
  measurement group is required.
- Write the complete output block in one request when several outputs must
  change together. Writes of individual complete fields are allowed and leave
  all omitted outputs unchanged.
- Treat cycle-rate changes as rare maintenance operations. Wait for the write
  response, allow the documented filter transient/skipped sample/counter reset,
  and use normal reconnect behavior if the TCP connection drops.
- Flash the latest firmware with identity coefficients immediately before each
  calibration run. The calibration command confirms the rev7 configuring
  response says uncalibrated, but does not compare versions, coefficients, or
  unit metadata.

## High-rate performance regression benchmark

Maintain a repeatable hardware benchmark throughout implementation so that
individually small firmware costs do not silently reduce the maximum viable
control rate. Establish a baseline before the Phase 1 changes, rerun it after
each phase and after any material sampling, conversion, serialization, or
network hot-path change, and check the result into the corresponding phase
report.

The canonical benchmark is a 5 kHz Deimos UDP roundtrip run lasting 10 seconds
in a release build on the same designated rev7 unit, controller host, direct
network setup, controller loop method, and operating configuration. Record the
controller's `loss_of_contact_counter` on every one of the expected 50,000
cycles. For this benchmark:

- a cycle with `loss_of_contact_counter > 0` is one dropped cycle;
- the drop rate is the number of dropped cycles divided by the number of cycles
  in the selected measurement window; do not sum the counter values, because
  that would overcount a consecutive-loss burst;
- the maximum observed counter is the maximum consecutive-drop burst length;
- report one-second buckets for the complete run so the initial synchronization
  transient and later steady behavior remain visible;
- report the steady drop rate over the final five seconds in addition to the
  whole-run drop rate. The fixed final window makes comparisons reproducible
  while excluding the expected initial synchronization transient;
- report the DAQ-reported cycle-time margin over both the whole run and final
  five seconds. Use its minimum to detect any missed deadline and a low
  percentile to estimate smaller firmware execution-time changes without
  allowing one host/network outlier to dominate the comparison;
- treat a reconnect, operating-state exit, or missing benchmark output as a
  failed run rather than as an ordinary dropped cycle.

Archive the firmware and software revisions, hardware serial, applied cycle
period, packet sizes, whole-run and steady drop rates, maximum burst length, and
minimum observed board/controller cycle margins. Use the initial measurements
to set the allowed regression tolerance before judging later phases. Any result
outside that tolerance blocks the phase until it is explained, optimized, or
explicitly accepted with an updated supported-rate limit. Do not hide a
performance regression by moving the sample-per-cycle cutover downward.

The DAQ margin describes the preceding completed cycle, so the first snapshot
has no margin measurement and retains its packet-default zero. Exclude that
default if it reaches the dispatched CSV; depending on synchronization, it may
be consumed before dispatch begins, so retain a nonzero first row. Clear the
sampling-time accumulator immediately before enabling the operating SysTick so
the first measured cycle does not subtract work accumulated during Connecting,
Binding, and Configuring. Use both the remaining minimum and first percentile
as active firmware-timing gates; Phase 5's acquisition timestamp work is
independent of this measurement.

## Implementation phases

Every phase exit includes the canonical 5 kHz/10-second hardware regression run
and comparison with the stored baseline; the functional exit criteria below do
not supersede that performance gate.

### Phase 1: shared nominal-path support, no Modbus protocol

1. Add unique magic fields and validated decoding to every rev7 Binding,
   Configuring, and Operating packet in both directions. Make discovery send
   and parse both the unchanged pre-rev7 Binding framing and the new rev7
   framing.
2. Add `firmware_calibrated` to the calibration binary and a rev7-specific
   configuring response. Route all controller configuring responses through the
   existing device-specific parser hooks. Make normal rev7 configuration require
   `1` and rev7 calibration collection require `0`; leave pre-rev7 packet types
   unchanged.
3. Add the direct shared Callendar--Van Dusen forward conversion and fitted
   global inverse polynomial, the offline K-type spline-fit generator, generated
   compact spline definitions, `deimos_shared::calcs`, and
   numerical/derivative parity tests.
4. Make host RTD and thermocouple calc wrappers use/re-export the shared
   functions; remove duplicate implementations.
5. Add `LinearCalibration` and `Rev7Calibration` `ByteStruct` types, binary
   generation, and compile-time inclusion.
6. Add the ADC double-buffer abstraction and switch the current UDP publisher
   to it.
7. Add `OperatingSnapshot` and the firmware engineering publisher.
8. Change the nominal UDP path and host rev7 parser/output names to use final
   engineering measurements.
9. Reduce rev7 host `standard_calcs` to external RTD resistance-to-temperature
   calculations, without retaining aliases for the obsolete raw packet.
10. Update rev7 calibration capture tooling for the identity-first inverse-
   conversion workflow and calibration binary artifact.
11. Enable smoltcp TCP and add static storage, one socket, and lifecycle helpers,
   without parsing or responding to Modbus.

Phase 1 exit criteria:

- Existing Deimos control operation works over UDP with the new snapshot.
- Firmware and host tests agree with the former standard-calc outputs within
  defined tolerances.
- Runtime shared calculations and calibration are entirely `f32`; software
  upcasts their results at its boundary.
- ADC values in every snapshot come from one published buffer generation.
- The generated calibration binary is included and deserializes correctly.
- Normal rev7 Deimos configuration rejects an uncalibrated unit, while the
  calibration command requires it and can proceed. Pre-rev7 configuration
  behavior is unchanged.
- Every state packet is rejected on either endpoint when its length, magic, or
  semantic validation is wrong.
- The identity-calibration capture workflow has been exercised end-to-end and
  the generated calibration reverified through engineering telemetry.
- The unused TCP socket does not change UDP timing, DHCP, or fallback behavior.
- Existing pre-rev7 units still scan, bind, configure, and operate with their
  unchanged packet formats.

### Phase 2: operating-mode/state refactor

1. Rename the existing state to `BoardState::OperatingDeimos`, add
   `BoardState::OperatingModbus(ModbusInitialConfig)`, and add the private
   `OperatingMode` dispatch argument.
2. Separate common operating entry/cycle work from Deimos UDP I/O.
3. Make `Configuring` enter `OperatingDeimos`; make both operating states call
   the same `operate(OperatingMode)` implementation.
4. Preserve outputs across Modbus-mode re-entry in unit tests before adding the
   Modbus parser.

Phase 2 exit criteria:

- Deimos mode remains behaviorally equivalent apart from the intentional
  engineering packet change.
- Synthetic Modbus-mode tests can enter, publish snapshots at 10 Hz, time out
  after 600 cycles, and re-enter at a new rate without an output glitch.

### Phase 3: Modbus/TCP protocol and register map

1. Add and integrate `rmodbus`.
2. Allow the first valid supported read or write received while binding to
   select `OperatingModbus(initial_config)` only when firmware calibration is
   present;
   preserve that request for processing after entry.
3. Implement full-snapshot reads, directly applied validated writes, standard
   exception responses, and loss-of-contact reset.
4. Implement cycle-rate re-entry with current outputs preserved.
5. Publish the register-map document and one reference client
   example.

Phase 3 exit criteria:

- Standard clients can connect, read a coherent complete snapshot, and decode
  all engineering values.
- An uncalibrated unit does not accept a Modbus TCP connection.
- A first read enters Modbus mode at 10 Hz with a 600-cycle timeout and safe
  outputs, then receives the first published snapshot.
- A first write overlays only its fields on those defaults.
- Output writes are atomic and rejected values leave outputs unchanged.
- Repeated reads and writes which omit outputs preserve the last applied outputs;
  only an explicit output field changes its corresponding value.
- Rate changes preserve outputs while updating the operating cycle and ADC
  cutoff; clients wait for the response before issuing another request.
- Requests using any Unit Identifier, including `0` and `255`, are accepted and
  responses echo the identifier unchanged.
- Each Modbus-capable cycle performs no more than two socket receives, two
  socket transmits, two Ethernet-frame receives, two Ethernet-frame transmits,
  and one complete ADU parse.
- One minute without accepted requests at the default configuration returns the
  board to `Connecting` and safe outputs through the existing transition path.

### Phase 4: Modbus/TCP hardening (complete)

1. Exercise malformed and partial-read Modbus TCP frames. Document that clients
   keep only one request outstanding; behavior for pipelined requests is not
   guaranteed.
2. Test disconnect/reconnect, DHCP/fallback address changes during an active
   session, deliberately stalled clients with TX backpressure, and an
   uninterrupted 60-second default loss-of-contact timeout.
3. Run the hardware matrix at the supported 4 Hz and 5 kHz endpoints. At each
   endpoint, sustain complete 75-register FC04 snapshot reads, complete
   21-register output-block FC16 writes, and complete three-register timing-
   configuration FC16 writes. Retain the one-request-outstanding client rule.
4. For every matrix case, capture DAQ cycle-time margin, loss-of-contact count,
   reconnect or operating-state exits, and the on-target MSP stack high-water
   mark. Run the canonical 5 kHz/10-second Deimos benchmark on the final Phase 4
   image and compare it with the Phase 3 handoff results.

Phase 4 exit criteria:

- Malformed, fragmented, stalled, and disconnected TCP sessions cannot create
  unbounded per-cycle work, alter outputs through a rejected request, or prevent
  a later client from reconnecting.
- An active connection recovers cleanly through tested DHCP/fallback address
  changes, and the default 60-second idle interval returns through the existing
  safe-output connection path.
- The 4 Hz and 5 kHz endpoint cases complete the full-read and full-write matrix
  without a firmware deadline miss, unexpected state exit, or stack-headroom
  violation. Any accepted loss or margin regression is recorded explicitly.
- The canonical Deimos benchmark remains within its established regression
  tolerance.

### Phase 5: ADC acquisition timestamps and release

1. Add the SysTick-based `AcquisitionClock` and update it at the beginning of
   the higher-priority communication handler using the actual completed
   interval.
2. Add the bounded, interrupt-masked TIM2 capture helper with `PENDSTSET`
   rollover detection, at most two attempts, and nominal-sample-period
   fallback.
3. Extend the ADC double-buffer payload with `sample_time_ns` and capture it
   immediately before the first conversion group.
4. Add `sample_time_ns` to `OperatingSnapshot`, its Modbus register-map input,
   and host parsing/output names.
5. Add arithmetic/protocol tests, update and rerun snapshot/register-map golden
   tests, and verify the acquisition instant against a hardware marker.
6. After all preceding baseline firmware machinery is complete, reflash and
   rerun the full identity-first calibration procedure for both existing rev7
   units with the final packet layout, then flash and verify their generated
   calibrated images.
7. Perform the deferred full-range calibrated engineering checks, including
   conversion accuracy over the supported RTD and thermocouple ranges. Keep
   this at the end so any failure is isolated from further firmware-mechanism
   changes.
8. Perform and archive the deferred physical pre-rev7 compatibility run. Use
   the Phase 1 checkpoint `e60e8e5` when the result must be attributed to the
   packet/calculation changes independently of later rev7 state-machine work.
9. Update firmware flashing/calibration procedures, software docs, examples,
   changelog, and coordinated-rollout notes.

Phase 5 exit criteria:

- Every engineering snapshot carries the acquisition time of the coherent ADC
  group from which its analog values were calculated.
- The normal capture path uses no strongly ordered atomics and masks interrupts
  only for the bounded clock-state/counter snapshot.
- Counter rollover cannot pair a cycle base with the wrong SysTick interval,
  and capture performs no more than two attempts.
- The fallback produces a monotonic nominal-period timestamp without adding a
  packet-validity field, diagnostic counter, or downstream branch.
- Existing cycle labels, active timing control, operating rates, and TIM5 uses
  remain behaviorally unchanged.
- Both existing rev7 units complete the identity-first procedure, reject normal
  operation while uncalibrated, accept it after their generated calibration is
  flashed, and pass the final calibrated engineering checks.
- Physical pre-rev7 discovery, binding, configuration, and operation remain
  compatible, with the checkpoint and hardware result archived.

### Phase 6: post-baseline high-rate sample-per-cycle investigation

Treat this as a measurement-gated extension after the complete baseline
firmware and software are running. Preserve the 33 kHz free-running sampler as
the default implementation unless the timing and aliasing characterizations
demonstrate a safe overlap between the two sampling modes.

1. Measure the final free-running path after firmware engineering conversions,
   snapshot timestamping, and both transports are present. Sweep operating
   rates with full packets and worst-case accepted reads/writes, active Deimos
   timing corrections, and release-build cache behavior. Record total cycle
   margin, communication-handler duration, TIM2 latency, and missed or coalesced
   sample events. Include the canonical 5 kHz loss-of-contact drop benchmark and
   compare its complete per-second series with the stored pre-change results.
   Define the highest acceptable free-running cycle rate from these measurements
   rather than from UDP echo throughput alone.
2. Extend the existing per-channel analog/digital response analysis to include
   folded alias contributions for a mode whose ADC sample rate equals its
   operating cycle rate. Determine the lowest acceptable sample-per-cycle rate
   from the required bandwidth, analog-filter response, and documented alias
   error limit. Evaluate approximately 5 kHz as the preferred cutover target;
   use a lower cutover only if the measured alias performance is acceptable.
3. Require the acceptable free-running range and sample-per-cycle range to
   overlap. If they do not, retain the baseline free-running implementation and
   check in the timing/alias report instead of introducing an unsupported rate
   gap or silently reducing either safety margin.
4. If a safe overlap exists, refactor interrupt ownership so operating entry
   lends the `Sampler` to exactly one IRQ scope. In free-running mode, a
   TIM2-owned closure samples at 33 kHz. In sample-per-cycle mode, the SysTick
   communication closure owns the sampler, performs one ADC acquisition and
   fractional-delay step first, then runs the common snapshot and communication
   work. Do not share a mutable sampler between simultaneously registered
   closures or place it behind a runtime lock.
5. Make both sampling closures publish the same `AdcSampleGroup` through the
   existing double-buffer writer API. The communication side uses the same
   single-flag-load reader handoff, engineering-conversion pipeline, timestamp
   field, and packet construction in both modes. Factor raw acquisition, channel
   alignment, counter capture, and buffer publication so the closures do not
   duplicate those operations.
6. Select the sampling mode once on operating entry from the applied cycle rate;
   do not branch on it in either steady-state hot path. Reuse the existing
   operating re-entry for a rate change. Disable and clear TIM2 before lending
   the sampler to SysTick, and restore its configured 33 kHz timer before
   lending it back to the TIM2 scope.
7. Build fractional-delay coefficients for the selected acquisition rate. The
   free-running path retains its existing IIR bank and 33 kHz sample rate; the
   sample-per-cycle path applies fractional delay but no ADC IIR. Continue using
   the separate 1 Hz board-temperature publication filter in both modes.
8. Define supported maximum post-quadrature encoder count rate and pulse-counter
   edge rate constants. Add compile-time assertions proving that, at the longest
   possible direct-mode cycle—including the permitted positive timing
   adjustment and timer quantization—each counter advances by strictly less than
   half its `2^16` modulus. Use explicit `2^16` and `2^32` modulus constants in
   the unrolling implementation and verify the wrap arithmetic at both
   boundaries.
9. Characterize the implemented sample-per-cycle path with the same worst-case
   UDP and Modbus workloads, phase/period corrections, counter inputs, and
   hardware timing instrumentation. Establish its maximum supported rate and
   attempt to demonstrate at least 5 kHz operation without violating cycle,
   sampling, timestamp, aliasing, watchdog, or output-safety requirements.

Phase 6 exit criteria:

- The checked-in characterization identifies the highest supported
  free-running rate, lowest alias-safe sample-per-cycle rate, and maximum
  sample-per-cycle rate for each relevant operating transport.
- If a safe overlap exists, one documented cutover selects the mode only at
  operating entry, and rate re-entry transfers exclusive sampler ownership
  between IRQ scopes without a steady-state mode branch.
- Both modes produce the same `AdcSampleGroup` contract and exercise the same
  communication-side snapshot, engineering-conversion, and packet code.
- Free-running mode remains behaviorally unchanged below the cutover.
  Sample-per-cycle mode performs exactly one ADC group and fractional-delay step
  per cycle and does not run the ADC IIR.
- Compile-time counter-rate assertions cover the worst direct-mode interval,
  and boundary tests use the correct power-of-two counter moduli.
- Hardware tests cover the shortest corrected cycle, maximum supported packets,
  TIM2 cadence below the cutover, direct-mode cadence above it, and timestamps
  across both modes.
- If the ranges do not overlap or the 5 kHz target is not safe, the report states
  the limiting timing or alias mechanism and the baseline mode remains the only
  supported implementation; the phase does not weaken an existing limit merely
  to add the new mode.

## Verification matrix

### Shared calculations

- Golden vectors compare the `f32` K-type spline functions with dense `f64`
  reference evaluation across the full domains, boundaries, former NIST branch
  points, and representative interpolation points.
- Per-span bounded local optimizations, seeded between every pair of control
  points and augmented with endpoints and known branch boundaries, enforce the
  `0.01 K` maximum temperature or temperature-equivalent error for both fitted
  `f64` and generated runtime `f32` evaluators. A dense sweep separately reports
  RMS error and guards the extrema search itself.
- Tests enforce smooth first derivative, monotonicity, and forward/inverse
  round-trip limits.
- Pt100 tests fix the published forward coefficients, compare direct resistance
  values with standard reference values, and prove the one global `f32` inverse
  polynomial remains monotonic and within `0.01 K` across the standard domain.
- A structural test or generated-code inspection ensures the supported-range
  inverse contains one coefficient array and one fixed Horner chain, with no
  range partition or iterative solver.
- Explicit tests cover below-range and above-range endpoint-tangent linearized
  extrapolation, boundary value/derivative continuity, NaN, and infinity.
- `deimos_shared` builds and tests without `std` or allocation.
- Host calc wrapper outputs equal the direct shared `f32` result upcast to
  `f64`.

### Calibration

- `write_bytes`/`read_bytes` round trips the flag and all coefficients exactly.
- The generator rejects non-finite or unrepresentable coefficients, while
  binary write/read preserves the resulting `f32` values exactly.
- The identity artifact reports `firmware_calibrated = 0`; the full-calibration
  generator cannot emit `1` until every required channel fit is present.
- Forward engineering conversion followed by the runner's inverse recovers
  sense voltage within a bound for every channel across its calibration range.
- The calibration procedure documents staying within the monotonic unsaturated
  domain and waiting for the board-temperature filter to settle.

### Packet validation

- Golden encodings fix the unique first-field magic and exact byte length of
  every rev7 Binding, Configuring, and Operating packet in each direction.
- Firmware and controller reject corrupt or wrong-state magic, truncation,
  trailing bytes, invalid enum representations, and invalid numeric fields.
- Rejected packets cannot change state or outputs and cannot reset the contact
  timeout.
- Pre-rev7 Binding/discovery remains unchanged. Normal rev7 Deimos connection
  stops after the device-specific Configuring response with a clear error,
  while the calibration command requires the uncalibrated response and
  continues.

### Double buffer and snapshots

- Readers never observe a partially written ADC group.
- Each copied `sample_time_ns` belongs to the same sampler iteration as every
  ADC value in that snapshot.
- Model tests cover the communication interrupt preempting TIM2 before the
  relaxed publication store and immediately after it. The reader obtains the
  complete previous or new generation respectively.
- The communication reader loads `latest` once, never writes it, and returns a
  value rather than a reference into static storage.
- Generated-code inspection confirms the relaxed publication operations and
  compiler fences do not emit hardware memory barriers or atomic read-modify-
  write instructions on the firmware target.
- Filter-update initialization uses the sampling subsystem's own latest
  `adc_values` without going through the shared reader API.
- Channel-by-channel conversion tests verify ordering and calibration placement.
- Snapshot serialization and host parsing agree field-for-field.
- Acquisition-clock tests cover the verified SysTick reload/count convention,
  pending-before/pending-during rollover cases, the two-attempt bound, and the
  nominal-period fallback.

### Operating modes

- Deimos loss-of-contact and timing synchronization behavior remains intact.
- Modbus default configuration resolves to 10 Hz and 600 cycles.
- An uncalibrated image never listens on port 502 and cannot enter Modbus mode.
- A valid FC03 or FC04 read or FC16 write can select Modbus mode from Binding;
  the triggering request receives its response after the first operating
  snapshot is published.
- Timeout counter resets only on accepted requests.
- Re-entry preserves PWM, DAC, and GPIO outputs. The client can reconnect if the
  TCP socket restarts.
- Repeated Modbus reads and writes which do not cover output fields preserve
  outputs; explicit zero and nonzero writes replace only the selected outputs.
- A cycle-rate change produces the documented skipped sample and counter reset;
  no continuity compensation is attempted.
- Invalid new rates do not alter filters, cycle timing, timeout, or outputs.
- Rates below the 1 Hz board-filter design threshold select the coefficient-
  level passthrough, retain the branch-free hot path, and have unity response.
- Register-map golden vectors cover FC03/FC04/FC16, network byte order, and
  most-significant-register-first `f32`, 32-bit, and 64-bit values.
- Unit-Identifier tests cover representative values including `0` and `255`
  and verify that each is accepted and echoed unchanged in its response.

### Hardware timing

- Compare release image size and SRAM3 use before/after TCP storage.
- Run and archive the canonical 5 kHz/10-second loss-of-contact benchmark after
  every phase and material hot-path change; compare whole-run and final-five-
  second drop rates with the established baseline.
- Measure TIM2 sample deadline margin while capturing acquisition timestamps
  and publishing all engineering values.
- Compare `sample_time_ns` with a GPIO acquisition-start marker and verify
  monotonicity across ordinary cycles and active phase/period adjustments.
- Measure worst-case operating-cycle margin while serving a maximum snapshot
  read and a full output/configuration write.
- At the Phase 4 4 Hz and 5 kHz endpoints, record the loss-of-contact series,
  DAQ margin, reconnect/state-exit status, and on-target MSP high-water mark for
  complete 75-register reads, 21-register output writes, and three-register
  timing-configuration writes.
- For the post-baseline investigation, sweep both sampling modes across the
  candidate overlap and record cycle margin, TIM2 cadence, communication-handler
  duration, and folded alias response. Include the shortest permitted corrected
  Deimos cycle and the worst accepted Modbus request.
- Exercise rate changes across the selected cutover and verify that exactly one
  IRQ scope owns the sampler, TIM2 is stopped and cleared in sample-per-cycle
  mode, and both modes publish the same coherent group format.
- Verify watchdog feeding and safe-output behavior during network failure.

## Coordinated rollout

Changing the UDP output from raw ADC voltages to engineering fields is an
intentional breaking protocol change. No rev7 units have shipped, so this plan
does not carry a rev7 backwards-compatibility burden. Do not add parsers,
aliases, version negotiation, old-capture adapters, or dual math paths for an
obsolete rev7 layout. Retain the existing pre-rev7 packet support.

Land and deploy firmware and `deimos`, including Python bindings/stubs if
affected, as one coordinated change. Change the packet magic values with the
new layouts; update output names, units, configurations, docs, examples, and
golden packets, then reflash both existing rev7 units. Flash each unit with the
latest firmware and identity calibration immediately before rerunning its full
calibration, generate its new `calibration.in`, flash the final image, and
archive the verification results.
Historical rev7 data which uses the old layout can be read with the old tagged
software; the new runtime does not accept that obsolete rev7 layout.

## Non-goals for the baseline implementation (Phases 1--5)

- A new `NetworkServing` state.
- Moving or redesigning ADC filter initialization.
- Independent Modbus sample, publish, filter, and timeout rates.
- More than one simultaneous Modbus TCP client.
- Concurrent Deimos and Modbus output ownership.
- Exposing raw ADC voltages or intermediate standard-calc values in the normal
  operating snapshot.
- Calculating external RTD temperatures in firmware.
- Updating persistent calibration through Modbus.
- TLS, authentication, or Modbus Security; deployment is limited to the trusted
  control network described above.
- A history/buffering register model; Modbus initially exposes only the latest
  published engineering snapshot.

## Known risks

- The relaxed two-slot publication protocol depends on one core, one writer,
  and one higher-priority communication reader. A multicore system, another
  reader, or a writer which can preempt the reader requires a triple buffer or a
  stronger ownership protocol to preserve no-wait reads and writes.
- The existing filter-rate update resets input counters and skips a sample. A
  writable Modbus cycle rate makes that documented behavior externally
  observable, but rate changes are expected to be rare.
- Too few K-type spline coefficients can introduce value bias; too many waste
  flash. Generated fit metrics and checked error/derivative limits determine
  the count.
- The global Pt100 inverse polynomial must meet the `0.01 K` requirement and
  remain monotonic in `f32` without range partitioning. Degree selection and
  exhaustive extrema validation guard against fit bias and high-order numerical
  instability.
- `f32` coefficients/evaluation can differ from the high-precision references;
  the error budget must include fitting error and precision-reduction error.
- The identity-first calibration workflow relies on flashing the latest
  firmware with identity coefficients immediately before capture and excluding
  saturation or unsettled filtered samples before inverse conversion.
- The new engineering packet deliberately breaks existing rev7 controller
  configurations and calibration tooling, so firmware, software, and both
  in-hand units must be upgraded and recalibrated together.
- A single timeout reset by any accepted Modbus request means read-only polling
  also maintains output authority. This is intentional under the simplified
  one-timeout design and should be documented for integrators.
- Acquisition timestamps rely on the existing SysTick-higher-than-TIM2 priority
  relationship and on the communication handler completing before the next
  SysTick boundary. The bounded fallback prevents this assumption from creating
  an unbounded sampling-ISR path.
- The safe free-running timing range and alias-safe sample-per-cycle range may
  not overlap after all engineering and protocol work is present. Phase 6 is a
  characterization-gated extension and retains the baseline sampler if there is
  no overlap.
- Sample-per-cycle mode makes the ADC cadence follow Deimos period/phase
  corrections. Its timestamp accuracy, fractional-delay behavior, and control
  quality must be verified at the maximum permitted corrections rather than
  inferred from nominal-rate tests.
- Small costs added across otherwise-correct phases can cumulatively reduce the
  viable control rate. The fixed 5 kHz loss-of-contact benchmark is a phase gate,
  and a regression must not be concealed by lowering the later sampling-mode
  cutover.
