//! Example of defining a mockup of a peripheral in software and communicating
//! with the controller via unix socket.
//!
//! In this example, the software peripheral is running in the same process,
//! but in general, the unix socket interface allows connecting to software
//! peripherals running in different processes.

use std::{
    collections::BTreeMap,
    os::unix::net::{SocketAddr, UnixDatagram},
    thread::{self, JoinHandle},
    time::{Duration, SystemTime},
};

use controller::context::{ControllerCtx, Termination};
use deimos::*;
use deimos_shared::{
    calcs::Calc,
    peripherals::{
        analog_i_rev_3::operating_roundtrip::{OperatingRoundtripInput, OperatingRoundtripOutput},
        model_numbers::EXPERIMENTAL_MODEL_NUMBER,
        Peripheral, PeripheralId, PluginMap,
    },
    states::{
        BindingInput, BindingOutput, ByteStruct, ByteStructLen, ConfiguringInput, ConfiguringOutput,
    },
    OperatingMetrics,
};
use serde::{Deserialize, Serialize};
use socket::unix::UnixSuperSocket;

fn main() {
    let mut ctx = ControllerCtx::default();
    ctx.op_name = "ipc_example".to_string();

    // Set control rate
    let rate_hz = 50.0;
    ctx.dt_ns = (1e9_f64 / rate_hz).ceil() as u32;

    // Set termination criteria to end the control loop after 1s
    ctx.termination_criteria = vec![Termination::Timeout(Duration::from_millis(1000))];

    // Define idle controller
    let mut controller = Controller::new(ctx);

    // Remove the default UDP socket and add a unix socket
    controller.clear_sockets();
    controller.add_socket(Box::new(UnixSuperSocket::new("ipc_ex")));

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

    // Scan to trigger the controller to build the socket folder structure
    controller.scan(1, &plugins);

    // Open a socket for the peripheral mockup
    let sock = UnixDatagram::bind("./sock/per/mockup").unwrap();

    // Start the in-memory peripheral on a another thread,
    // setting a timer for it to terminate after 5 seconds
    let mockup = PState::Binding {
        end: SystemTime::now() + Duration::from_secs(5),
        sock,
    };
    let mockup_thread = mockup.run();

    // Start the controller
    controller.run(&plugins).unwrap();

    // Wait for the mockup to finish running
    mockup_thread.join().unwrap();
}

/// The controller's representation of the in-memory peripheral mockup
#[derive(Serialize, Deserialize, Debug)]
pub struct IpcMockup {
    pub serial_number: u64,
}

#[typetag::serde]
impl Peripheral for IpcMockup {
    fn model_number(&self) -> u64 {
        EXPERIMENTAL_MODEL_NUMBER
    }

    fn serial_number(&self) -> u64 {
        self.serial_number
    }

    fn id(&self) -> PeripheralId {
        PeripheralId {
            model_number: EXPERIMENTAL_MODEL_NUMBER,
            serial_number: self.serial_number,
        }
    }

    fn n_inputs(&self) -> usize {
        8
    }

    fn n_outputs(&self) -> usize {
        24
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

    fn parse_operating_roundtrip(&self, bytes: &[u8], outputs: &mut [f64]) -> OperatingMetrics {
        assert_eq!(outputs.len(), self.n_outputs());
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

/// The actual in-memory peripheral mockup.
///
/// Bare-bones peripheral state machine with a fixed end time.
/// This simple implementation does not respect the target dt_ns,
/// instead responding immediately on receiving a controller input.
enum PState {
    Binding {
        end: SystemTime,
        sock: UnixDatagram,
    },
    Configuring {
        controller: SocketAddr,
        end: SystemTime,
        sock: UnixDatagram,
    },
    Operating {
        controller: SocketAddr,
        end: SystemTime,
        sock: UnixDatagram,
    },
    Terminated,
}

impl PState {
    /// Run state machine until arriving at Terminated
    fn run(self) -> JoinHandle<()> {
        let mut state = self;
        thread::spawn(|| loop {
            state = match state.transition() {
                Self::Terminated => return,
                x => x,
            }
        })
    }

    /// Proceed to next state transition
    fn transition(self) -> Self {
        match self {
            Self::Binding { end, sock } => {
                let buf = &mut vec![0_u8; 1522][..];
                loop {
                    // Check for timeout
                    if SystemTime::now() > end {
                        return Self::Terminated;
                    }

                    // Receive packet
                    if let Ok((size, src_addr)) = sock.recv_from(buf) {
                        // For this example, just check if the packet is the right length
                        if size != BindingInput::BYTE_LEN {
                            continue;
                        }

                        let resp = BindingOutput {
                            peripheral_id: PeripheralId {
                                model_number: EXPERIMENTAL_MODEL_NUMBER,
                                serial_number: 0,
                            },
                        };

                        resp.write_bytes(&mut buf[..BindingOutput::BYTE_LEN]);
                        let _ = sock.send_to_addr(&buf[..BindingOutput::BYTE_LEN], &src_addr);

                        return Self::Configuring {
                            controller: src_addr,
                            end,
                            sock,
                        };
                    }
                }
            }
            Self::Configuring {
                controller,
                end,
                sock,
            } => {
                let buf = &mut vec![0_u8; 1522][..];
                loop {
                    // Check for timeout
                    if SystemTime::now() > end {
                        return Self::Terminated;
                    }

                    // Receive packet
                    if let Ok((size, src_addr)) = sock.recv_from(buf) {
                        if size != ConfiguringInput::BYTE_LEN
                            || src_addr.as_pathname().unwrap() != controller.as_pathname().unwrap()
                        {
                            continue;
                        }

                        let resp = ConfiguringOutput {
                            acknowledge: deimos_shared::states::AcknowledgeConfiguration::Ack,
                        };

                        resp.write_bytes(&mut buf[..ConfiguringOutput::BYTE_LEN]);
                        let _ = sock.send_to_addr(&buf[..ConfiguringOutput::BYTE_LEN], &controller);

                        return Self::Operating {
                            controller,
                            end,
                            sock,
                        };
                    }
                }
            }
            Self::Operating {
                controller,
                end,
                sock,
            } => {
                let buf = &mut vec![0_u8; 1522][..];
                let mut i = 0;
                loop {
                    // Check for timeout
                    if SystemTime::now() > end {
                        return Self::Terminated;
                    }

                    // Receive packet
                    if let Ok((size, src_addr)) = sock.recv_from(buf) {
                        if size != OperatingRoundtripInput::BYTE_LEN
                            || src_addr.as_pathname().unwrap() != controller.as_pathname().unwrap()
                        {
                            continue;
                        }

                        let inp = OperatingRoundtripInput::read_bytes(&buf[..size]);
                        let last_received_id = inp.id;

                        let mut resp = OperatingRoundtripOutput::default();
                        resp.metrics.last_input_id = last_received_id;
                        resp.metrics.id = i;

                        resp.write_bytes(&mut buf[..ConfiguringOutput::BYTE_LEN]);
                        let _ = sock.send_to_addr(&buf[..ConfiguringOutput::BYTE_LEN], &controller);

                        i += 1;
                    }
                }
            }
            Self::Terminated => return Self::Terminated,
        }
    }
}
