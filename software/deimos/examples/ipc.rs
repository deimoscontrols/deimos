//! Example of defining a mockup of a peripheral in software and communicating
//! with the controller via unix socket.
//!
//! In this example, the software peripheral is running in the same process,
//! but in general, the unix socket interface allows connecting to software
//! peripherals running in different processes.

use std::time::Duration;

use controller::context::{ControllerCtx, Termination};
use deimos::*;
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
