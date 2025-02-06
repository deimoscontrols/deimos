# Changelog

## 2025-02-05 deimos 0.6.0

### Added

* Add `DataFrameDispatcher` for writing data to in-memory dataframe
* Add `Msg::Packet(Vec<u8>)` variant of user channel message to enable generic packetized message-passing
* !Add `reset` method to `Dispatcher` trait
    * To be called when the controller terminates, returning dispatchers to their pre-init state for reuse

### Changed

* !Rename `Panic` variant of `Overflow` to `Error`
    * Dispatchers can return an error on consuming values; this provides more flexibility in future error handling
* !Rename `initialize` method of `Dispatcher` trait to `init`

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
