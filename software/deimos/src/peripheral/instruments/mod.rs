//! Third-party laboratory instruments presented as Deimos peripherals.

pub mod keithley_dmm6500;
mod protocol;
mod scpi;
pub mod siglent_sdg2042x;

pub use protocol::InstrumentRunHandle;

use deimos_shared::peripherals::ModelNumber;

/// Beginning of the model-number range reserved for built-in software peripherals.
pub(crate) const SOFTWARE_MODEL_NUMBER_BASE: ModelNumber = 1 << 63;

pub(crate) fn serial_from_address(address: &str) -> u64 {
    // Stable FNV-1a rather than DefaultHasher, whose output is not a public contract.
    address.bytes().fold(0xcbf29ce484222325, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3)
    })
}
