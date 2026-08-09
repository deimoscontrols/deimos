//! Siglent SDG2042X basic-wave generator integration.
//!
//! The peripheral exposes requested settings for both output channels while a
//! dedicated worker serializes SCPI commands over TCP. The controller-facing
//! responder publishes only the most recently completed two-channel state, so
//! the controller never waits for network or relay latency.
//!
//! A disabled channel is physically driven to 0 V DC before its output relay is
//! opened. Startup, controller loss, explicit disable, worker failure, and
//! shutdown all use this safe state. Arbitrary waveforms, modulation, burst,
//! and the generator's internal sweep subsystem are intentionally unsupported.

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
/// This connects and validates the instrument before registering its software
/// peripheral and automatically named thread-channel socket with `controller`.
/// Both channel configurations and all six dynamic inputs per channel remain
/// independent, while applied outputs are published as one completed
/// two-channel state.
///
/// Args:
///   peripheral_name: Unique name used for controller fields such as
///   `peripheral_name.ch1_enabled`.
///   config: Complete connection, identity, waveform, channel, and timeout
///   configuration.
///   controller: Controller to receive the peripheral and generated socket.
///
/// Returns:
///   A running instrument handle that must outlive the controller run.
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
    let channel_name = driver.channel_name().to_owned();
    attach_instrument(
        peripheral_name,
        &channel_name,
        driver.peripheral(),
        "SDG2042X",
        controller,
        |ctx| driver.run(ctx),
    )
}
