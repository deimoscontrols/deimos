# Changelog

## 2025-08-25 deimos 0.11.0, deimos_shared 0.5.0, Deimos DAQ Rev 5.0.0 hardware and firmware

### Added

* Add deimos_daq_rev5 schematic, layout, fab files, and firmware
* Add deimos_daq_rev5 interface to deimos_shared
* Add deimos_daq_rev5 peripheral implementation and calcs to deimos::peripheral

## 2025-06-28 deimos 0.10.2

### Changed

* Do not exit control loop immediately on transmit error
    * Instead, allow peripherals or controller to register loss of contact & follow established loss of contact procedure
    * This allows the control loop to bridge brief outages during DHCP IP address lease renewal

### Added

* Add ltspice models of several sizing options for zero-ripple Sallen-Key filters

## 2025-06-25 analog_i_rev4 firmware 0.4.0, deimos 0.10.1

### Changed

* Firmware
    * Update internal samplerate to 33kHz to restore timing margin that was lost to fractional delay filters
    * Add accumulated sampling time per comm cycle to cycle timing margin metric
* Software
    * Restore DataFrameDispatcher to top level public API
    * Remove conditional compilation flags from readme example

## 2025-06-21 analog_i_rev4 firmware 0.3.0

### Added

* Implement fractional delay FIR filters on scanned ADC samples to produce synthetic simultaneous sampling

## 2025-06-21 deimos 0.10.0, deimos_gui 0.1.1

### Changed

* Refactor to eliminate all conditional compilation features from control program
  * All features now enabled by default
* Remove polars dep due to excessive build time
* Refactor dataframe dispatcher module to use a lightweight internal dataframe target instead of a polars dataframe
* Update gui dep on control program to not reference features that no longer exist

### Added

## 2025-06-21 analog_i_rev4 firmware 0.2.0

### Added

* Add second (butter1) filter bank for low reporting rate to Sampler struct
* Add record of target cutoff ratio to Sampler struct
* Use second butter1 filter bank if target cutoff ratio is below stable range for nominal butter2 filter bank
  * This will activate below a reporting rate of 40Hz, allowing proper (though less performant) filtering at the very low end of viable reporting rate
  * Minimum tabulated cutoff ratio for the butter1 filter extends to a reporting rate of 4Hz, just below the system timing minimum freq of 5Hz
* Incidentally add SVG version of logo

## 2025-05-06 deimos 0.9.0, deimos_gui 0.1.0

### Added

* !Add bound on Debug for Calc and Peripheral to support use as part of state objects in iced gui
* Extract determination of calc eval order to `Orchestrator::eval_order()` and include evaluation depth
* Add read-only access to orchestrator via controller
* Add `all` feature
* Add `basic.rs` example


## 2025-05-03 deimos 0.8.1

Edits to prep for implementation of a calc node editor GUI.

### Added

* impl Clone for `Box<dyn Calc>` and `Box<dyn Peripheral>` by sending for a roundtrip loop through serde_json
  * Supports construction from prototype in GUI editor
* Add `kind` methods with default implementation to `Calc` and `Peripheral` to retrieve type name
* Add more methods for immutable access to calcs and peripherals via `Controller`
* Add `PeripheralProto` trait for prototyping `Peripheral` like `Calc`

### Changed

* Improve validation of `Sequence`
* Add non-empty default for `SequenceMachine`
* Derive `Default` on existing `Peripheral` types to allow blanket impl of `PeripheralProto`

## 2025-03-23 deimos 0.8.0

Implement `SequenceMachine` calc, which provides arbitrary lookup-table state machines in order to support
user-defined scheduling and operational logic.

### Changed

* !Make `ser` a default feature
* Make serde_json part of `ser` feature
* Update `multi_daq` example
  * Include more hardware units
  * Include an example `SequenceMachine` usage
  * Use the examples folder as the op folder
  * Make `ser` a required dep to handle loading `SequenceMachine` from disk

### Added

* Add `calc::sequence_machine` module with `SequenceMachine` and related types and functionality.

## 2025-02-23 deimos 0.7.0

Place functionality not required to run the base control program behind feature flags,
including serialization, user channels, and manipulation of thread priority and core affinity.
This reduces the base set of deps to 21, only 8 of which are not either proc macros or build deps.

### Changed

* !Roll forward to 2024 edition
* !Eliminate default features
* !Put serialization behind a feature flag
* !Put sideloading thread channels behind a feature flag
* !Put thread priority and core affinity behind a feature flag
* !Rename `SuperSocket` trait and types to `Socket`
* Reduce saved intermediate calcs in standard calcs for AnalogIRev{3,4}
* Include all features in docs

## 2025-02-22 Hardware - Analog I Rev 4.0.2

### Changed

* Replace input comparators with SN74LV Schmitt trigger logic buffers
    * Adds about 0.5V hysteresis to resolve jitter & eliminates exposure of 1V024 ref to noise from digital signals

## 2025-02-22 deimos 0.6.2, deimos_shared 0.4.0

### Changed

* !Remove duty cycle measurement from Analog I Rev4 firmware, packet format, and calcs
    * Despite deconflicting the duty cycle measurement to a different compare-and-capture and its own pin, configuring TIM4's second CCMR for duty cycle measurement causes both CCMR1 and CCMR2 to fail to trigger
    * This is the only remaining timer module with a second pin or compatible second compare-and-capture available, so duty cycle measurement will not be available on this unit
* Relicense under MIT/Apache-2.0 from 0BSD/Apache-2.0
* Update readme to be more concise

## 2025-02-09 deimos 0.6.1

### Added

* Add methods for parsing and emitting configuration packets to Peripheral
    * Default impl matches existing system, but allows future peripherals with additional config fields

## 2025-02-05 deimos 0.6.0

### Added

* Add `DataFrameDispatcher` for writing data to in-memory dataframe
* Add `Msg::Packet(Vec<u8>)` variant of user channel message to enable generic packetized message-passing
* !Add `terminate` method to `Dispatcher` and `Calc` traits and `Orchestrator` struct
    * To be called when the controller terminates, returning dispatchers to their pre-init state for reuse

### Changed

* !Rename `Panic` variant of `Overflow` to `Error`
    * Dispatchers can return an error on consuming values; this provides more flexibility in future error handling
* !Rename `initialize` method of `Dispatcher` trait to `init`
* !Rename `calcs` module to `calc`
* !Rename `peripherals` module to `peripheral`
* !Move orchestrator module under `calcs` and export 
* Make large const arrays static to avoid inlining excessively large data

## 2025-02-04 deimos 0.5.1

### Added

* Implement user channels
    * Bidirectional multiple-producer, multiple-consumer buffering message pipes
    * Passed to appendages with context during init
* Add sideloading example that uses user_ctx and user_channels fields to bypass nominal flow of information

## 2025-02-02 Hardware - Analog I Rev 4.0.1

### Changed - Hardware - Analog I Rev 4.0.1

* Replace input comparators with TLV3201s with 1M input pulldown to resolve inadequate drive strength
    * Non-inverting hysteresis-free configuration
* Move screw terminal inward and place silkscreen terminal labels on the outside
* Slightly widen board to make room for labels
* Update 3 pin screw terminal footprint for more accurate courtyard
* Update silkscreen labels to orient outward or toward the side opposite the power/eth plugs

## 2025-02-02 deimos 0.5.0, deimos_shared 0.3.0, firmware analog_i_rev4 0.1.0

### Added - deimos

* Add AnalogIRev4 peripheral implementation

### Changed - deimos

* Rename `analog_i_rev3` example to `multi_daq` and add an AnalogIRev4
* Specify mode explicitly in config packet
* Start clock for binding just before transmission & remove 1ms pad
* Update function signature for SuperSocket::update_map to be fallible
    * This provides a means to handle errors for duplicate addresses
* Update UDP and unix socket implementations to check for duplicate addresses when building address table

### Added - deimos_shared

* Add AnalogIRev4 packet format

### Changed - deimos_shared

* Mark `Mode` enum non-exhaustive

### Added - firmware

* Add analog_i_rev4 firmware
    * Compared to rev3, includes use of second compare-and-capture on TIM4/FREQ0 input to extract duty cycle

## 2025-02-01 deimos 0.4.0, deimos_shared 0.2.0

### Added - deimos

* Add implementation of SuperSocket for unix datagram socket
* Add non-exhaustive `ControllerCtx` context struct to store growing number of settings
* Add non-exhaustive `Termination` enum for planned termination criteria
* Add non-exhaustive `LossOfContactPolicy` enum for choosing reconnect behavior
* Add `ipc_plugin` example demonstrating use of peripheral plugins, a software-defined peripheral state machine, and use of unix socket for communication with a software-defined peripheral
* Add `dispatcher::fmt_time` method to reduce repeated code for fixed-width UTC formatting
* Add `calcs` and `peripherals` modules relocated from `deimos_shared`
* Add `scripts` folder relocated from `deimos_shared`
* Add github actions workflow that runs IPC example for end-to-end smoketest

### Changed - deimos

* Update function signatures for controller methods to use context instead of individual arguments
* Update function signatures for plugins to be compatible with object safety
* Update control loop to accept and use plugins
* Update SuperSocket init to take context
* Update Dispatcher init to take context
* Update Orchestrator and Calc init to take context
* Make runtime internal state (`ControllerState`, `PeripheralState`, `TimingPid`) pub(crate) as they are not usable outside the specifics of the controller internals
* Update docs

### Changed - deimos_shared

* Remove unnecessary methods from `Peripheral` trait & implementations
* Remove `std` feature and related deps
* Move stdlib portion of library (`Calc` and `Peripheral` traits and impls, as well as `PluginFn` and `PluginMap`) to `deimos`
    * deimos_shared is now purely the `no-std` shared library
* Move `scripts` folder to `deimos`
* Update docs

### Changed - firmware

* Update flash.py and flash.sh to accept model name and use it for the path to the appropriate firmware folder

## 2025-01-26 deimos 0.3.0, deimos_shared 0.1.3

### Added - deimos

* Add socket module with SuperSocket trait and related types
* Add implementation of SuperSocket for UDP

### Changed - deimos

* Update controller to use new socket interface

### Changed - deimos_shared

* Update module docstrings
