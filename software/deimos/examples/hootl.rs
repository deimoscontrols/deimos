//! Define a mockup of a peripheral in software and communicate
//! with the controller via unix socket.
//!
//! In this example, the software peripheral is running in the same process,
//! but in general, the unix socket interface allows connecting to software
//! peripherals running in different processes.
//!
//! Demonstrated here:
//!   * Using unix socket for communication with a peripheral
//!   * Running a mock peripheral using the built-in mockup driver
//!   * Defining the controller's representation of that mock peripheral
//!   * Using an in-memory data target
//!   * Running a control program with no hardware in the loop

use std::{
    collections::BTreeMap,
    time::{Duration, SystemTime},
};

// For defining the peripheral mockup
use deimos_shared::{
    OperatingMetrics,
    peripherals::{
        PeripheralId,
        analog_i_rev_3::operating_roundtrip::{OperatingRoundtripInput, OperatingRoundtripOutput},
        model_numbers::EXPERIMENTAL_MODEL_NUMBER,
    },
    states::{ByteStruct, ByteStructLen},
};

use serde::{Deserialize, Serialize};

// For using the controller
use deimos::{
    calc::Calc,
    controller::context::{ControllerCtx, Termination},
    dispatcher::{DataFrameDispatcher, Overflow},
    peripheral::{HootlDriver, MockupTransport, Peripheral, PluginMap},
    socket::unix::UnixSocket,
    *,
};

use tracing::info;

fn main() {
    // Clear sockets
    let _ = std::fs::remove_dir_all("./sock");

    // Start building up controller settings
    let mut ctx = ControllerCtx::default();
    ctx.op_name = "ipc_example".to_string();

    // Set control rate
    let rate_hz = 50.0;
    ctx.dt_ns = (1e9_f64 / rate_hz).ceil() as u32;

    // Set termination criteria to end the control loop after a set duration from start of operating
    ctx.termination_criteria = vec![Termination::Timeout(Duration::from_millis(500))];

    // Define idle controller
    let mut controller = Controller::new(ctx);

    // Remove the default UDP socket and add a unix socket
    controller.clear_sockets();
    controller.add_socket(Box::new(UnixSocket::new("ipc_ex")));

    // Add an in-memory data target
    let (df_dispatcher, df_handle) = DataFrameDispatcher::new(1, Overflow::Error, None);
    controller.add_dispatcher(Box::new(df_dispatcher));

    // Register the mockup as a plugin
    let mut pmap: PluginMap = BTreeMap::new();
    pmap.insert(EXPERIMENTAL_MODEL_NUMBER, &|b| {
        Box::new(IpcMockup {
            serial_number: b.peripheral_id.serial_number,
        })
    });
    let plugins = Some(pmap);

    // Tell the controller to expect the in-memory peripheral
    // and register it as a plugin
    let p = IpcMockup { serial_number: 0 };
    controller.add_peripheral("mockup", Box::new(p));

    // Start the mockup driver on another thread,
    // setting a timer for it to terminate at a specific time
    let end = SystemTime::now() + Duration::from_millis(1000);
    let mockup_driver = HootlDriver::new(
        &IpcMockup { serial_number: 0 },
        MockupTransport::unix_socket("mockup"),
    )
    .with_end(Some(end));
    let mockup_thread = mockup_driver
        .run(&controller.ctx)
        .expect("Failed to start mockup driver");

    // Scan for peripherals to find the mockup
    let scan_result = controller
        .scan(100, &plugins)
        .expect("Failed to scan for peripherals");
    info!("Scan found:\n{:?}", scan_result.values());

    // Serialize and deserialize the controller (for demonstration purposes)
    {
        let serialized_controller = serde_json::to_string_pretty(&controller).unwrap();
        let _: Controller = serde_json::from_str(&serialized_controller).unwrap();
    }

    // Start the controller
    let exit_status = controller.run(&plugins, None);
    info!("Controller exit status: {exit_status:?}");

    // Wait for the mockup to finish running
    mockup_thread.join().unwrap();

    // Get collected dataframe
    let df = df_handle.try_read().unwrap();
    info!("Collected data");
    info!("{:?}", df.headers());
    info!("{:?}", df.rows().first().unwrap());
    info!("...");
    info!("{:?}", df.rows().last().unwrap());

    // Clear sockets
    let _ = std::fs::remove_dir_all("./sock");
}

/// The controller's representation of the in-memory peripheral mockup,
/// reusing the AnalogIRev3's packet formats for convenience.
#[derive(Serialize, Deserialize, Default, Debug)]
pub struct IpcMockup {
    pub serial_number: u64,
}

#[typetag::serde]
impl Peripheral for IpcMockup {
    fn id(&self) -> PeripheralId {
        PeripheralId {
            model_number: EXPERIMENTAL_MODEL_NUMBER,
            serial_number: self.serial_number,
        }
    }

    fn input_names(&self) -> Vec<String> {
        let mut names = Vec::new();

        for i in 0..4 {
            names.push(format!("pwm{i}_duty").to_owned())
        }

        for i in 0..4 {
            names.push(format!("pwm{i}_freq").to_owned())
        }

        names
    }

    fn output_names(&self) -> Vec<String> {
        let mut names = Vec::new();

        for i in 0..20 {
            names.push(format!("ain{i}").to_owned())
        }
        names.push("encoder".to_owned());
        names.push("counter".to_owned());
        names.push("freq0".to_owned());
        names.push("freq1".to_owned());

        names
    }

    fn operating_roundtrip_input_size(&self) -> usize {
        OperatingRoundtripInput::BYTE_LEN
    }

    fn operating_roundtrip_output_size(&self) -> usize {
        OperatingRoundtripOutput::BYTE_LEN
    }

    fn emit_operating_roundtrip(
        &self,
        id: u64,
        _period_delta_ns: i64,
        _phase_delta_ns: i64,
        _inputs: &[f64],
        bytes: &mut [u8],
    ) {
        // If this were a real peripheral, we'd take the inputs from `inputs` here

        let mut msg = OperatingRoundtripInput::default();
        msg.id = id;

        msg.write_bytes(bytes);
    }

    fn parse_operating_roundtrip(&self, bytes: &[u8], _outputs: &mut [f64]) -> OperatingMetrics {
        let n = self.operating_roundtrip_output_size();
        let out = OperatingRoundtripOutput::read_bytes(&bytes[..n]);
        // If this were a real peripheral with measurements, we'd write them to `outputs` here

        out.metrics
    }

    /// Get a standard set of calcs that convert the raw outputs
    /// into a useable format.
    fn standard_calcs(&self, _name: String) -> BTreeMap<String, Box<dyn Calc>> {
        BTreeMap::new()
    }
}
