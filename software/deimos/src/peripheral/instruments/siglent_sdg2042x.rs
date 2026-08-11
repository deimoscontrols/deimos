//! Siglent SDG2042X basic-wave generator integration.
//!
//! The peripheral exposes requested settings for both output channels while a
//! dedicated worker serializes SCPI commands over TCP. The controller-facing
//! responder publishes only the most recently transmitted two-channel state,
//! so the controller never waits for network or relay latency. Physical state
//! is read back during startup and shutdown, not on each operating cycle.
//!
//! A disabled channel is physically driven to 0 V DC before its output relay is
//! opened. Startup, controller loss, explicit disable, worker failure, and
//! shutdown all use this safe state. Arbitrary waveforms, modulation, burst,
//! and the generator's internal sweep subsystem are intentionally unsupported.
//!
//! `peripheral` remains a pure, serializable description of the Deimos wire
//! contract. `driver` owns the live SCPI state, and `proxy` is the only module
//! that translates between their packet types and the shared responder.

mod config;
mod driver;
mod peripheral;
mod proxy;

#[cfg(test)]
mod tests;

pub use config::{ChannelConfig, Config, Load, Waveform};
pub use driver::SiglentSdg2042XDriver;
pub use peripheral::{MODEL_NUMBER, SiglentSdg2042X};

use super::responder::{InstrumentRunHandle, attach_instrument};
use crate::controller::Controller;

/// Attach one configured SDG2042X to a controller.
///
/// Connection and safe-state validation complete before registration. Retain
/// the returned handle until the controller run has stopped.
///
/// Errors:
///   Returns an error for duplicate peripheral names, invalid configuration,
///   connection or identity failure, safe-state readback failure, thread
///   startup failure, or controller registration failure.
pub fn attach(
    peripheral_name: &str,
    config: Config,
    controller: &mut Controller,
) -> Result<InstrumentRunHandle, String> {
    let driver = SiglentSdg2042XDriver::new(config)?;
    attach_instrument(
        peripheral_name,
        driver.peripheral(),
        "SDG2042X",
        controller,
        |ctx| driver.run(ctx),
    )
}
