/// Configuration I/O is the same for all devices.
/// If further configuration for a custom device is
/// required, it must be implemented outside this system.
use crate::enum_with_unknown;
use byte_struct::*;
pub use byte_struct::{ByteStruct, ByteStructLen};

/// Input from the controller to the peripheral during `configuring` state
#[derive(ByteStruct, Clone, Copy, Debug, Default)]
#[byte_struct_le]
pub struct ConfiguringInput {
    /// Sample / control time step
    pub dt_ns: u32,

    /// Buffering mode vs. roundtrip control mode
    pub mode: Mode,

    /// Delay before entering Operating,
    /// during which the controller makes sure each module
    /// acknowledges its configuration
    pub timeout_to_operating_ns: u32,

    /// During `Operating` state, how many missed packets in a row should
    /// signal that we have lost contact with the controller?
    pub loss_of_contact_limit: u16,
}

/// Output from the peripheral to the controller during `configuring` state
#[derive(ByteStruct, Clone, Copy, Debug, Default)]
#[byte_struct_le]
pub struct ConfiguringOutput {
    /// Might not acknowledge if an error occurs
    pub acknowledge: AcknowledgeConfiguration,
}

enum_with_unknown!(
    /// Reason for a peripheral explicitly rejecting its configuration
    #[derive(Default)]
    #[non_exhaustive]
    pub enum AcknowledgeConfiguration(u8) {
        #[default]
        Ack = 0b1111_1111,
        Nak = 0,
        NakDtTooSmall = 1,
        NakDtTooLarge = 2,
    }
);

impl ByteStructLen for AcknowledgeConfiguration {
    const BYTE_LEN: usize = 1;
}

impl ByteStruct for AcknowledgeConfiguration {
    fn read_bytes(bytes: &[u8]) -> Self {
        Self::from(bytes[0])
    }

    fn write_bytes(&self, bytes: &mut [u8]) {
        bytes[0] = u8::from(*self);
    }
}

enum_with_unknown!(
    #[derive(Default)]
    #[non_exhaustive]
    pub enum Mode(u8) {
        #[default]
        Roundtrip = 0,
        Buffering = 0b1111_1111,
    }
);

impl ByteStructLen for Mode {
    const BYTE_LEN: usize = 1;
}

impl ByteStruct for Mode {
    fn read_bytes(bytes: &[u8]) -> Self {
        Self::from(bytes[0])
    }

    fn write_bytes(&self, bytes: &mut [u8]) {
        bytes[0] = u8::from(*self);
    }
}
