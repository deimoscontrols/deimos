# Deimos

Control program and data integrations for the Deimos data acquisition ecosystem.

See the [project readme](https://github.com/deimoscontrols/deimos/blob/main/README.md) for contact details as well as commentary about
the goals and state of the project.

The control program follows the hardware peripheral state machine,
which is linear except that an error in any peripheral state results
in returning to `Connecting`:
1. `Connecting` (no communication with the control machine)
2. `Binding` (waiting to associate with a control machine)
3. `Configuring` (waiting for operation-specific configuration from control machine)
4. `Operating` (roundtrip control)

The initialization schedule is:

```text
              binding timeout window
                 /
                /           
             |----|         timeout to operating
             |--------------|
             |      \
 sent binding|       \
             |    configuring window
   peripherals
    transition
to configuring            
```

Peripherals acknowledge binding and transition to configuring on their next internal cycle (usually 1ms) after receiving a request to bind. They will then wait for configuration input until the timeout
to operating, and either proceed to `Operating` if configuration was
successful, or return to `Connecting` (and likely return immediately to `Binding`) if configuration was not successful.

The control loop then follows a fixed schedule on each cycle:

1. Send control input to peripherals
2. Wait for outputs from peripherals
    * Target synchronization to middle of cycle
3. Update time-sync control
4. Run calculations on peripheral outputs

Control loop timing uses the control machine's best available monotonic clock. Both system time in UTC with nanoseconds and monotonic clock time
are stored in order to support post-processing adjustments to
account for the slow drift of the monotonic clock relative to system time.

# Example: 200Hz Control Program w/ 2 DAQs
```rust
use std::time::Duration;

use deimos::*;
use deimos_shared::calcs::{Constant, Sin};
use deimos_shared::peripherals::{PluginMap, analog_i_rev_3::AnalogIRev3};

// The name of the operation will be used as the table name for databases,
// or as the file name for local storage.
let op_name = "test_op";

// An optional dictionary mapping user-defined custom hardware
// peripheral model numbers to initializer functions.
let peripheral_plugins: Option<PluginMap> = None;

// Configure the controller
//    Sample interval is taken as integer nanoseconds
//    so that any rounding and loss of precision is visible to the user
let rate_hz = 200.0;
let dt_ns = (1e9_f64 / rate_hz).ceil() as u32;  // Control cycle period
//    The delay between peripherals receiving their configuration and
//    entering the control loop operation state.
//    At least 1ms is recommended to allow time for peripherals
//    to process the received configuration.
let timeout_to_operating_ns = (dt_ns * 2).max(1_000_000);
//    After some number of missed control packets, the peripherals
//    must assume contact has been lost, and return to their default
//    state to wait for new instructions.
let loss_of_contact_limit = 10;

// Set up any number of data integrations,
// all of which will receive the same data at each cycle of the control loop
//    TSDB-flavored postgres database
let buffer_window = Duration::from_nanos(1); // Non-buffering mode
let retention_time_hours = 1;
let timescale_dispatcher: Box<dyn Dispatcher> = Box::new(TimescaleDbDispatcher::new(
    "<database name>",  // Database name
    "<database address>", // URL or unix socket interface
    "<username>",  // Login name; for unix socket, must match OS username
    "<token env var>",  // Environment variable containing password or token
    buffer_window,
    retention_time_hours,
));
//    A 50MB CSV file that will be wrapped an overwritten when full
let csv_dispatcher: Box<dyn Dispatcher> =
    Box::new(CsvDispatcher::new(50, dispatcher::Overflow::Wrap));

// Initialize the controller
// with no peripherals associated yet
let mut controller = Controller::new(dt_ns, timeout_to_operating_ns, loss_of_contact_limit);

// Associate hardware peripherals that we expect to find on the network
// The controller can also run with no peripherals at all, and simply do
// calculations on a fixed time interval.
controller.add_peripheral("p1", Box::new(AnalogIRev3 { serial_number: 1 }));
controller.add_peripheral("p2", Box::new(AnalogIRev3 { serial_number: 2 }));

// Associate data integrations with the controller
controller.add_dispatcher(timescale_dispatcher);
controller.add_dispatcher(csv_dispatcher);

// Set up calcs that will be run at each cycle
//     Add a constant for duty cycle and a sine wave for frequency
let freq = Sin::new(1.0 / (rate_hz / 100.0), 0.25, 100.0, 250_000.0, true);
let duty = Constant::new(0.5, true);
controller.add_calc("freq", Box::new(freq));
controller.add_calc("duty", Box::new(duty));
//     Set a PWM on the first peripheral to change its frequency in time
controller.set_peripheral_input_source("p1.pwm0_freq", "freq.y");  // A value to be written to the hardware
controller.set_peripheral_input_source("p1.pwm0_duty", "duty.y");
//     Set a PWM frequency on one peripheral based on a measured temperature from the other peripheral
controller.set_peripheral_input_source("p2.pwm0_duty", "duty.y");  // Values can be referenced any number of times
controller.set_peripheral_input_source("p2.pwm0_freq", "p1_rtd_5.temperature_K");

// Serialize and deserialize the controller (for demonstration purposes).
// All of the configuration up to this point, including any custom peripheral plugins
// or user-defined calcs, are serialized with the controller and can be written to and read from a json file.
let serialized_controller: String = serde_json::to_string_pretty(&controller).unwrap();
let _deserialized_controller: Controller = serde_json::from_str(&serialized_controller).unwrap();

// Run the control program
// (skipped here because there are no peripherals
// on the network in the test environment).
// controller.run(op_name);
```