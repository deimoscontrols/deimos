# Third-party instrument integration design

## Scope

Deimos may expose third-party laboratory instruments as `Peripheral` plugin implementations under `deimos::peripheral::instruments`.

An integration should expose only the minimum required functions, not attempt to achieve full API coverage.

## Goals

- Make instrument inputs and outputs available in the control loop.
- Keep blocking I/O away from the control loop.
- Fail safely and observably.
- Keep dependencies and complexity small.
- Provide a repeatable pattern for future instrument integrations.

## Non-goals

- Hard-realtime or phase-synchronized instrument I/O.
- Complete coverage of a vendor's remote-control API.
- A single transport or protocol abstraction for every instrument.
- A general framework for instrument interfaces.

## Architecture

```text
                         control-process boundary

  Controller
      |
      | Deimos bind/configure/operating packets
      v
  ThreadChannelSocket
      |
      v
  protocol responder thread
      |                         ^
      | latest request          | latest completed state
      v                         |
  instrument I/O worker thread
      |
      | instrument protocol over its external transport
      v
  third-party instrument
```

Each instrument integration has four parts:

1. A `Peripheral` implementation defines identity, channel names packet codecs, validation, and standard calculations.
  * It owns no socket, thread, mutex, or live connection.
2. A protocol responder implements the software side of the normal Deimos Binding, Configuring, and Operating lifecycle over a thread channel. 
  * It must drain controller packets promptly and never perform blocking external I/O.
3. An instrument worker owns the external connection and protocol state.
4. A run handle owns stop signals and thread joins.
  * The control-program builder that starts the driver retains this handle and shuts it down after the controller stops.

The split between protocol responder and instrument worker is required to prevent blocking instrument I/O from causing deadline misses.

## Module boundaries

Instrument code lives below:

```text
deimos::peripheral::instruments
├── responder                 shared private lifecycle and registration helpers
├── scpi                      shared public settings and private TCP transport
└── <instrument>              one module per supported integration
    ├── config                public instrument configuration
    ├── peripheral            serializable controller-side representation
    ├── proxy                 device-side responder adapter
    └── driver                live state and instrument communication
```

The `peripheral` module exposes the `instruments` namespace but does not re-export concrete instrument module internals.

Each concrete instrument module exposes an `attach(name, config, &mut controller) -> RunHandle` helper that registers the peripheral, adds a thread-channel socket with a name derived from the instrument model and logical serial number, and starts the instrument driver.

Instrument model numbers occupy an explicitly documented software-only range and never overlap hardware model numbers.

## Transport and protocol rules

Each integration uses the simplest reliable transport and protocol supported by the instrument.

Addresses, connection settings, timeouts, and response limits are explicit configuration.

SCPI integrations group their shared network, identity, and timeout settings in `ScpiTcpConfig`.

Where the instrument provides an identity query, the worker verifies and logs the configured identity.

Only the instrument worker may access its connection.

## Operating lifecycle

### Startup

1. Construct the `Peripheral`, driver configuration, and driver.
2. Add one uniquely named `ThreadChannelSocket` and peripheral to the controller.
3. Start the driver with the controller context.
  * Connect, verify identity, and enforce safe state.
5. Enter Binding only after instrument setup succeeds.
6. Scan, bind, etc. via the controller.

Startup failure returns an error before controller operation begins.

### Operation

The protocol responder acknowledges every valid controller request using the latest completed instrument state.

Instruments may run at a lower rate than the controller.

The latest commanded full instrument state is reasserted when the previous transaction has finished and a new command has arrived from the controller.

### Shutdown

Controller termination first emits its normal zero-valued peripheral inputs.

The worker then makes a bounded best-effort attempt to enforce safe state, stop acquisition, and close its connection.

Dropping a run handle signals all integration threads; joining reports panics and latched worker errors.

Shutdown never waits forever for an unresponsive instrument.

## Timing semantics

Instrument data is asynchronous and has loose, host-observed timing:

- Outputs become applied when the external operation completes.
- An acquired value is timestamped with the best available information.

## Failure and safety policy

- Clamp source commands to explicitly configured limits. NaN maps to safe-state.
- Limits, safe states, and expected identity are explicit configuration.
- Error on startup identity mismatch.
- A timeout, disconnect, malformed response, or invalid acquired value latches a driver error.
  * The proxy ceases valid operating responses so the existing controller loss-of-contact policy terminates the run.
- Errors identify the instrument and operation but never substitute a plausible value for invalid or missing data.
- Instruments use a documented safe configuration unless supplied by the caller.
- Only one Deimos driver owns a given instrument control connection.

## Testing policy

Automated tests use a HOOTL mockup. No laboratory hardware or network discovery required.
