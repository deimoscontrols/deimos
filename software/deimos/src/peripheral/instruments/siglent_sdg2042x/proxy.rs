//! Deimos operating-packet adapter for the live SDG2042X driver.
//!
//! This module is deliberately the only place where the driver implements the
//! shared responder interface. The driver itself deals in typed instrument
//! states, while the responder deals in opaque byte packets and lifecycle
//! events.

use std::sync::Arc;

use deimos_shared::peripherals::PeripheralId;
use deimos_shared::states::{ByteStruct, OperatingMetrics};

use super::super::responder::{InstrumentProxy, InstrumentRunHandle, start_driver};
use super::driver::SiglentSdg2042XDriver;
use super::peripheral::{INPUT_SIZE, OUTPUT_SIZE, OperatingInput, OperatingOutput};
use crate::controller::context::ControllerCtx;
use crate::peripheral::Peripheral;

impl SiglentSdg2042XDriver {
    /// Connect, validate identity, apply the safe state, and start both threads,
    /// returning a handle that owns their shutdown and joining.
    ///
    /// Errors:
    ///   Returns an error for connection, identity, setup, readback, or thread
    ///   startup failures. The protocol responder is not started until physical
    ///   instrument setup has succeeded.
    pub fn run(&self, ctx: &ControllerCtx) -> Result<InstrumentRunHandle, String> {
        let worker = self.shared_handle();
        start_driver(
            ctx,
            format!("sdg2042x-{}", self.peripheral().serial_number),
            "SDG2042X",
            self.startup_timeout(),
            Arc::new(self.shared_handle()),
            move |stop, startup| worker.run_worker(stop, startup),
        )
    }
}

impl InstrumentProxy for SiglentSdg2042XDriver {
    fn id(&self) -> PeripheralId {
        self.peripheral().id()
    }

    fn input_size(&self) -> usize {
        INPUT_SIZE
    }

    fn output_size(&self) -> usize {
        OUTPUT_SIZE
    }

    fn process_request(&self, bytes: &[u8]) -> u64 {
        let packet = OperatingInput::read_bytes(bytes);
        // Submission only updates shared memory and wakes the worker; no SCPI
        // operation can block the protocol responder.
        self.submit(packet.state);
        packet.id
    }

    fn write_response(&self, metrics: OperatingMetrics, bytes: &mut [u8]) -> Result<(), String> {
        OperatingOutput {
            metrics,
            state: self.applied()?,
        }
        .write_bytes(bytes);
        Ok(())
    }

    fn on_loss_of_contact(&self) {
        // The worker gives this request priority over ordinary operating state.
        self.request_safe_state();
    }

    fn error(&self) -> Option<String> {
        self.latched_error()
    }
}
