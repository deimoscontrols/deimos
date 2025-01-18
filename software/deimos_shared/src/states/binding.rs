//! Binding I/O format is the same for all devices.

use byte_struct::*;
pub use byte_struct::{ByteStruct, ByteStructLen};

use crate::peripherals::PeripheralId;

/// Input from the controller to the device during `binding` state.
///
/// The peripheral will immediately enter `configuring` after receiving
/// this message, then after `configuring_timeout_ms` milliseconds,
/// it will either continue into `operating` (if it has received configuration)
/// or time-out back to `connecting` then return to `binding`.
#[derive(ByteStruct, Clone, Copy, Debug, Default)]
#[byte_struct_le]
pub struct BindingInput {
    /// How long should we wait in Configuring state
    /// before timeout back to Connecting -> Binding?
    pub configuring_timeout_ms: u16,
}

/// Output from the peripheral to the controller during `binding` state.
///
/// The peripheral's unique id is returned in order to allow the use of
/// `binding` frames to scan the network for live peripherals.
#[derive(ByteStruct, Clone, Copy, Debug, Default)]
#[byte_struct_le]
pub struct BindingOutput {
    pub peripheral_id: PeripheralId,
}
