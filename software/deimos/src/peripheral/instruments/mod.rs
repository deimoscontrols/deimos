//! Third-party laboratory instruments presented as Deimos peripherals.
//!
//! Each concrete instrument module contains its peripheral representation,
//! configuration, blocking-I/O driver, and a convenience `attach` function.
//! Live connections remain on worker threads; the controller communicates with
//! a nonblocking protocol responder through an internal thread-channel socket.
//! The shared responder knows only the Deimos packet lifecycle, while the SCPI
//! transport knows only newline-delimited TCP. Instrument-specific modules are
//! the narrow adapter between those two pieces.
//!
//! Retain every returned [`InstrumentRunHandle`] until after the controller has
//! stopped, then join it so output-capable instruments can enter their safe
//! state and worker failures remain observable.

#![deny(missing_docs)]

pub mod keithley_dmm6500;
mod responder;
mod scpi;
pub mod siglent_sdg2042x;

pub use responder::InstrumentRunHandle;
pub use scpi::ScpiTcpConfig;

use deimos_shared::peripherals::ModelNumber;

/// Beginning of the model-number range reserved for built-in software peripherals.
pub(crate) const SOFTWARE_MODEL_NUMBER_BASE: ModelNumber = 1 << 63;
