# Changelog

## 2025-02-01

### deimos 0.4.0

#### Added - deimos

* Add implementation of SuperSocket for unix datagram socket
* Add non-exhaustive `ControllerCtx` context struct to store growing number of settings
* Add non-exhaustive `Termination` enum for planned termination criteria
* Add non-exhaustive `LossOfContactPolicy` enum for choosing reconnect behavior
* Add `ipc_plugin` example demonstrating use of peripheral plugins, a software-defined peripheral state machine, and use of unix socket for communication with a software-defined peripheral
* Add `dispatcher::fmt_time` method to reduce repeated code for fixed-width UTC formatting
* Add `calcs` and `peripherals` modules relocated from `deimos_shared`

#### Changed - deimos

* Update function signatures for controller methods to use context instead of individual arguments
* Update function signatures for plugins to be compatible with object safety
* Update control loop to accept and use plugins
* Update SuperSocket init to take context
* Update Dispatcher init to take context
* Make runtime internal state (`ControllerState`, `PeripheralState`, `TimingPid`) pub(crate) as they are not usable outside the specifics of the controller internals

#### Changed - deimos_shared

* Remove unnecessary methods from `Peripheral` trait & implementations
* Remove `std` feature and related deps
* Move stdlib portion of library (`Calc` and `Peripheral` traits and impls, as well as `PluginFn` and `PluginMap`) to `deimos`
    * deimos_shared is now purely the `no-std` shared library

#### Changed - firmware

* Update flash.py and flash.sh to accept model name and use it for the path to the appropriate firmware folder

## 2025-01-26

### deimos 0.3.0, deimos_shared 0.1.3

#### Added - deimos

* Add socket module with SuperSocket trait and related types
* Add implementation of SuperSocket for UDP

#### Changed - deimos

* Update controller to use new socket interface

### Changed - deimos_shared

* Update module docstrings
