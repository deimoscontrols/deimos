//! Third-party laboratory instruments presented as Deimos peripherals.
//!
//! Concrete modules pair a pure peripheral with a blocking-I/O worker and an
//! `attach` helper. The controller communicates with each worker through a
//! nonblocking, identity-keyed protocol responder; shared SCPI transport stays
//! independent of instrument-specific commands.
//!
//! Retain every returned [`InstrumentRunHandle`] until after the controller has
//! stopped, then join it so output-capable instruments can enter their safe
//! state and worker failures remain observable.

#![deny(missing_docs)]

pub mod keithley_dmm6500;
mod responder;
mod scpi;
pub mod siglent_sdg2042x;

#[cfg(test)]
mod test_support;

pub use responder::InstrumentRunHandle;
pub use scpi::ScpiTcpConfig;

use deimos_shared::peripherals::ModelNumber;

/// Beginning of the model-number range reserved for built-in software peripherals.
pub(crate) const SOFTWARE_MODEL_NUMBER_BASE: ModelNumber = 1 << 63;
