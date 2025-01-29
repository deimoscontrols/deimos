//! Example of defining a mockup of a peripheral in software and communicating
//! with the controller via unix socket.
//!
//! In this example, the software peripheral is running in the same process,
//! but in general, the unix socket interface allows connecting to software
//! peripherals running in different processes.

use std::{
    os::unix::net::{SocketAddr, UnixDatagram},
    path::PathBuf,
    time::{Duration, SystemTime},
};

use controller::context::{ControllerCtx, Termination};
use deimos::*;
use deimos_shared::{
    peripherals::{analog_i_rev_2::operating_roundtrip::{OperatingRoundtripInput, OperatingRoundtripOutput}, PeripheralId},
    states::{
        BindingInput, BindingOutput, ByteStruct, ByteStructLen, ConfiguringInput, ConfiguringOutput,
    },
};
use socket::unix::UnixSuperSocket;

fn main() {
    let mut ctx = ControllerCtx::default();
    ctx.op_name = "ipc_example".to_string();

    // Set control rate
    let rate_hz = 50.0;
    ctx.dt_ns = (1e9_f64 / rate_hz).ceil() as u32;

    // Set termination criteria to end the control loop after 250ms
    ctx.termination_criteria = vec![Termination::Timeout(Duration::from_millis(250))];

    // Define idle controller
    let mut controller = Controller::new(ctx);

    // Remove the default UDP socket and add a unix socket
    controller.clear_sockets();
    controller.add_socket(Box::new(UnixSuperSocket::new("ipc_ex")));
}

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
    fn run(self) -> Self {
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
                                model_number: 0,
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

                        return Self::Operating { controller, end, sock }
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
