//! I/O formatting for each peripheral state.
//!
//! `connecting` => `binding` => `configuring` => `operating`
//!
//! `connecting`, during which devices are acquiring an IP address, does not
//! need any I/O formats here, because there is no communication with the controller.

pub mod binding;
pub mod configuring;
pub mod operating;

pub use binding::*;
pub use configuring::*;
pub use operating::*;
