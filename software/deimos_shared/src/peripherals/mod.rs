use byte_struct::*;
use core::fmt::Debug;

pub mod analog_i_rev_2;
pub mod analog_i_rev_3;
pub mod analog_i_rev_4;
pub mod deimos_daq_rev5;
pub mod model_numbers;

// Type aliases for device identification

/// Identifier of the kind of device.
/// Many individual devices will have the same model number.
pub type ModelNumber = u64;

/// Identifier of a specific unit for a given model number.
/// Only one device with a given model number will have a given serial number,
/// but multiple devices with different model numbers will have the same serial number.
pub type SerialNumber = u64;

/// Unique identifier that combines model number and serial number.
/// Only one individual unit will have a given peripheral ID.
#[derive(ByteStruct, Clone, Copy, Default, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[byte_struct_le]
pub struct PeripheralId {
    pub model_number: ModelNumber,
    pub serial_number: SerialNumber,
}
