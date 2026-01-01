# Changelog

## 2026-01-01 deimos 0.15.0

Broad refactor and many new features to improve usability of software interface. 

### Added

* Rust
    * Add `python` feature for building python bindings
    * Add `controller::nonblocking` module with machinery for running the controller on a separate thread
        * Includes external termination signal and live manual read/write handles
    * Add reconnection logic to controller
        * `LossOfContactPolicy::Reconnect` allows attempting reconnection after losing contact with peripherals during run
        * This provides robustness to IP address reconfiguration, hot-swapping modules, etc
        * Provide a timeout duration or allow indefinite reconnection attempts
    * Add `peripheral::hootl` module with machinery for building software mockup wrappers of hardware
        * Includes UDP loopback implementaiton to mimic hardware as fully as possible
    * Add `python` module with python bindings
    * Add `socket::orchestrator` module with new layer of socket abstraction
        * Option 1 (used for Performant operation mode) is similar to previous (synchronous nonblocking rx/tx)
        * Option 2 (used for Efficient operation mode) uses a fan-in thread channel system to defer rx waiting to OS scheduling and places sockets on separate threads during run
    * Add `socket::thread_channel` module with a Socket implementation that uses a user channel for comms
        * This supports HOOTL testing on non-unix platforms with the unix socket is not available
    * Add `socket::worker` module with socket thread workers for use with Efficient operation mode fan-in pattern
    * Add `Efficient` operation mode
        * Uses OS scheduling instead of busy-waiting, and thread channel fan-in pattern for comms instead of polling
        * Reduces CPU usage to around 1% at the expense of degraded performance above 50Hz
        * Does not pin core affinity
    * Add `dispatcher::latest` for extracting the latest values during run
    * Add `dispatcher::low_pass` wrapper for running a low-pass filter on each channel
    * Add `dispatcher::decimation` wrapper for taking every Nth value
    * Add `dispatcher::channel_filter` wrapper for selecting only specific channels to be passed along
* Python
    * Add python bindings and type stubs
    * Add python examples, tests, and deployment workflow

### Changed

* Rust
    * Remove `calc::Orchestrator` from public API and rename to `CalcOrchestrator`
    * Refactor `Socket` trait for new optional fan-in system
    * Refactor `Controller` and `Context`
        * Accommodate both Performant and Efficient operating methods
        * Implement reconnection logic
        * Implement nonblocking operation and external read/write/stop handles
        * Fine-tune control loop performance to reduce cycle busy time to about 12 microseconds and eliminate context switching opportunities from main loop, allowing at least 20kHz stable operation of control software under real-life conditions with 7 DAQs attached on UDP and 447 channels of data stored (now limited primarily by hardware performance)
        * Store, add, and remove sockets and dispatchers by name
    * Refactor `Calc` and `Dispatcher` implementations to return `Box<Self>` from `new` to reduce boilerplate
    * Refactor `logging` module to allow reentrant runs without conflict due to duplicate logger setup
    * Refactor `calc::sequence_machine` module into a folder with multiple files and remove Timeout::Error option
    * Set up workspace versioning
    * Update and unpin dep versions to be more friendly to use within a larger project
    * Gate unix socket functionality behind `#[cfg(unix)]` to allow Windows builds to run
    * Improve logging and error handling across entire project
    * Use `x86-64-v3` reference cpu target instead of manually enabling instruction sets
    * Update readme
    * Move `dispatcher::tsdb::Row` up to `dispatcher` for reuse
    * Add live-reading handle to `dispatcher::df` for use from python
    * Add `Send + Sync` to trait bounds for `peripheral::PluginFn` to support nonblocking run method
* Hardware
    * Improve manufacturability of silkscreen on Deimos DAQ Rev6
* Firmware
    * Update Deimos DAQ Rev6 firmware to use latest version of `flaw` filtering library

### Removed

* Rust
    * Remove `CalcProto` and `PeripheralProto` object prototyping interfaces and related types and constants.
    * Remove `Orchestrator` (from public API)

## 2025-10-26 Deimos DAQ Rev 6.0.2 hardware

### Changed

* Deimos DAQ Rev 6
    * Remove trace that bridged 3.3V and 1.024V rails due to autocleanup failure during hierarchical layout update
    * Re-generate fab files

## 2025-10-16 deimos 0.14.0

### Added

* Add `math` and `math::polynomial` modules with polynomial evaluation and curvefitting

### Changed

* `calc::Polynomial`
  * !Remove `eval_poly` method and defer to `math::polyval`, which uses mul_add operations
  * Add `fit_from_points` method for initializing coeffs from calibration points

## 2025-10-14 deimos 0.13.0

### Added

* Add polynomial calc

### Changed

* !Make logger guards local instead of global statics to allow reconfiguring logging between sequential ops
* !Return log file location and guards from logger setup
* Name log file after op
* Append logs instead of truncating
* Remove thread priority functionality that requires elevated permissions
* Instead of using every second core for auxiliary functions, reserve the first 2 cores
  for the main thread and cycle auxiliary functions over all the remaining cores
* Improve logging and error handling

## 2025-10-13 deimos 0.12.0

### Added

* Add logging module with terminal and file logging setup using nonblocking `tracing` ecosystem

### Changed

* !Calc trait methods now return Result types
* !Controller::{bind, scan} now return Result types and map previous panic behavior to errors
* Orchestrator methods map more cases to Err instead of panic
* Terminal printing in Controller and examples replaced with logging
* Controller::terminate rolls up all errors before returning
* In Controller::run, when attempting to send data to dispatchers, an attempt is made to send
  data to every dispatcher before returning an error rollup if any errors occurred
* In Controller::run, log malformed or unexpected packets that were previously ignored
* Initialize tsdb dispatcher on main thread and propagate initialization errors instead of panicking worker thread
* Propagate errors instead of panicking when possible in CSV dispatcher

## 2025-10-13 deimos 0.11.3

### Added

* `calc::butter` module with `Butter2` second order butterworth filter calc
* `branches` dep for branch hinting filter initialization

### Changed

* Apply 1Hz Butter2 filtered `board_rtd_filtered` channel in rev6 standard calcs
* Use filtered board temperature for thermocouple cold-junction correction

## 2025-10-12 deimos 0.11.2, deimos_shared 0.5.1, Deimos DAQ Rev 6.0.1 hardware and firmware

Tooling
* ubuntu 24.04
* kicad 8 with hierarchical PCB plugin
* protocase designer v7.2.1 linux version
  * Run like `LD_LIBRARY_PATH=/opt/protocasedesigner/lib/app/natives/occjava/linux-x86_64/ /opt/protocasedesigner/bin/ProtocaseDesigner` to resolve link path issue for java runtime
* freecad 1.0.2

### Added

Hardware
* Deimos DAQ Rev 6.0.1 schematic and layout
    * [x] Replace overvoltage protection diodes from leaky zeners to low-leakage JFET clamp
    * [x] Add 5V LDO as supply for resistance amplifiers to restore gate voltage margin
    * [x] Use smaller resistor values for resistance amp common-mode filter
        * INA826 has significant input bias current (~25nA) which makes a nontrivial voltage across 20k resistance
    * [x] Disable internal buffer for DAC to allow rail-to-rail operation
        * Buffered mode min and max voltages are 20mV off the rails
        * Can rely on external buffer that is already implemented for overvoltage protection
* Controller Base Rev 16.0.1
    * [x] Remove unnecessary clamping diodes
    * [x] Remove unnecessary eth PHY crystal load caps
    * [x] Clean up ethernet layout

Software & Firmware
* deimos_daq_rev6 firmware
    * Rolled forward to latest `interpn` and `flaw` versions w/ perf improvements; ~30% increase in cycle timing margin
* deimos
    * Add rev 6 firmware and calc plugin
* deimos_shared
    * Add rev 6 packet formats

Analysis
* Added JFET diode clamp spice model


## 2025-09-19 deimos 0.11.1, Deimos DAQ Rev 6.0.0 hardware

Tooling
* ubuntu 24.04
* kicad 8 with hierarchical PCB plugin
* protocase designer v7.2.1 linux version
  * Run like `LD_LIBRARY_PATH=/opt/protocasedesigner/lib/app/natives/occjava/linux-x86_64/ /opt/protocasedesigner/bin/ProtocaseDesigner` to resolve link path issue for java runtime
* freecad 1.0.2

### Added

* Enclosure design (protocase)
* 3D assembly (freecad)
    * With STEP exports from protocase and kicad
* Rev6 schematic and layout
    * [x] Add damping in RTD current sink control loop
        * Existing circuit sees about 60mV peak-to-peak oscillation on the FET gate due to lack of damping in loop
        * Add integrator-style filter
        * Resistor from FET source to amp negative
        * Capacitor from FET gate to amp negative
        * Amp output to amp gate
        * 10k 10nF
    * [x] Use higher-voltage supply for inamp circuits again to handle common-mode without saturating
        * Without this, TC and RTD amps read low and high-gain voltage inputs saturate well within intended 0-2.5V range
        * +/-12V split rail should do the trick
    * [x] Put supply voltage on ADJ/EN pin of LDOs
        * Previous revs' 4-20mA current limiter LDOs were functioning due to a coincidental hardware bug in the LDOs that caused them to latch enabled during board startup
        * Intended behavior of the LDO chip is to be powered down when ADJ/EN is shorted to ground
        * LDO targets ADJ pin as output voltage if ADJ voltage is > enable threshold (around 1.5V)
        * Bodge-wire testing confirms that putting 12V on the ADJ pin for the 12V LDO produces the intended behavior (pass-through with short-circuit protection)
    * [x] Remove LDO output caps & add 0.1uF input caps
        * Limit inrush to avoid over-current trip during board startup
    * [x] Add 1.024V on ref pin for high-gain voltage inputs to allow measuring with symmetric noise near 0V
    * [x] Add one power-on indicator LED back in
    * [x] Fix labeling of gain in schematic for 660x channel
    * [x] Update 3.3V zener to PN with smaller footprint
    * [x] Use back-to-back zeners for filter input to handle negative rail saturation
    * [x] Replace OPA333 with OPA196 for filters & use +/-12V split supply to get full range of 0-2.5V output
    * [x] Add -12V rail for inamp biasing
        * TI application note SLVAE10 fig 3 shows use of TPS560430 as inverting buck-boost
        * https://www.ti.com/lit/an/slvae10/slvae10.pdf
    * [x] Remove inamp from 4-20mA and G<=1 circuits in favor of overvoltage protection in filter circuits
    * [x] Remove PWM I/O logic buffers in favor of resistor-diode overvoltage protection & current-limiting
        * Buffers pull too much current & produce ground plane noise 
    * [x] Remove 5V converter that is no longer needed for anything 
    * [x] Remove extra mounting holes that will not be needed with an extrusion enclosure 
    * Prep layout for aluminum extrusion enclosure
        * https://www.protocase.com/products/electronic-enclosures/aluminum.php
        * [x] Move components back from board edge (min 0.64" clearance each side to tall parts, 0.2" to short parts & traces)
        * [x] Place remaining horizontal connectors on ends for panel access, 0.064" overhang for front/back panels
        * [x] Pack lever-wire connectors closer together now that the dimensions are confirmed
    * [x] Replace eth connector with one that is smaller and cheaper 
    * [x] Add enclosure
        * [x] Template
        * [x] Cutouts
        * [x] Silkscreen labels

### Changed

* Improved error handling and formatting in `deimos::controller`

## 2025-08-25 deimos 0.11.0, deimos_shared 0.5.0, Deimos DAQ Rev 5.0.0 hardware and firmware

### Added

* Add deimos_daq_rev5 schematic, layout, fab files, and firmware
    * Adds active analog filters, 2x buffered analog outputs, and more raw voltage inputs
* Add deimos_daq_rev5 interface to deimos_shared
* Add deimos_daq_rev5 peripheral implementation and calcs to deimos::peripheral

### Removed

* Remove deimos_gui

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
