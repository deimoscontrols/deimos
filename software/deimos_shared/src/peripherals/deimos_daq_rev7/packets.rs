//! Rev7 setup packets and calibration image records.
//!
//! This module contains the rev7 binding, configuration, and operating packet
//! definitions.

mod operating;

pub use super::calc::{Calibration, LinearCalibration};
pub use operating::*;

use byte_struct::{ByteStruct, ByteStructLen, ByteStructUnspecifiedByteOrder};

use crate::{
    peripherals::PeripheralId,
    states::{AcknowledgeConfiguration, ConfiguringInput as BaseConfiguringInput, Mode},
};

/// Rev7-specific binding request. Older hardware continues to use the generic request.
///
/// Fields are serialized contiguously in little-endian order.
#[derive(ByteStruct, Clone, Copy, Debug, Default)]
#[byte_struct_le]
pub struct BindingInput {
    /// Direction- and state-specific packet marker.
    pub magic: u32,
    /// Maximum configuring-state inactivity duration in `ms`.
    pub configuring_timeout_ms: u16,
}

impl BindingInput {
    /// Builds a binding request with the required rev7 packet marker.
    ///
    /// Args:
    ///   configuring_timeout_ms: Maximum configuring-state inactivity
    ///     duration in `ms`.
    ///
    /// Returns:
    ///   Initialized rev7 binding request.
    pub fn new(configuring_timeout_ms: u16) -> Self {
        Self {
            magic: super::BINDING_INPUT_MAGIC,
            configuring_timeout_ms,
        }
    }

    /// Checks the direction- and state-specific packet marker.
    ///
    /// Returns:
    ///   `true` when `magic` matches [`super::BINDING_INPUT_MAGIC`].
    pub fn is_valid(&self) -> bool {
        self.magic == super::BINDING_INPUT_MAGIC
    }
}

/// Rev7-specific binding response.
///
/// Fields are serialized contiguously in little-endian order.
#[derive(ByteStruct, Clone, Copy, Debug, Default)]
#[byte_struct_le]
pub struct BindingOutput {
    /// Direction- and state-specific packet marker.
    pub magic: u32,
    /// Model and serial-number identity of the responding board.
    pub peripheral_id: PeripheralId,
}

impl BindingOutput {
    /// Builds a binding response with the required rev7 packet marker.
    ///
    /// Args:
    ///   peripheral_id: Model and serial-number identity of the board.
    ///
    /// Returns:
    ///   Initialized rev7 binding response.
    pub fn new(peripheral_id: PeripheralId) -> Self {
        Self {
            magic: super::BINDING_OUTPUT_MAGIC,
            peripheral_id,
        }
    }

    /// Checks the packet marker and rev7 model number.
    ///
    /// Returns:
    ///   `true` when the response is marked as a rev7 binding response and
    ///   identifies a rev7 board.
    pub fn is_valid(&self) -> bool {
        self.magic == super::BINDING_OUTPUT_MAGIC
            && self.peripheral_id.model_number == super::MODEL_NUMBER
    }
}

/// Rev7 configuration request with a direction-specific packet marker.
///
/// Fields are serialized contiguously in little-endian order.
#[derive(ByteStruct, Clone, Copy, Debug, Default)]
#[byte_struct_le]
pub struct ConfiguringInput {
    /// Direction- and state-specific packet marker.
    pub magic: u32,
    /// Nominal operating-cycle duration in `ns`.
    pub dt_ns: u32,
    /// Requested operating protocol mode.
    pub mode: Mode,
    /// Delay from accepted configuration to operating entry in `ns`.
    pub timeout_to_operating_ns: u32,
    /// Consecutive missed cycles allowed before loss-of-contact shutdown.
    pub loss_of_contact_limit: u16,
}

impl ConfiguringInput {
    /// Adds the rev7 packet marker to a generic configuration request.
    ///
    /// Args:
    ///   base: Generic Deimos configuration values.
    ///
    /// Returns:
    ///   Equivalent rev7-specific request.
    pub fn from_base(base: BaseConfiguringInput) -> Self {
        Self {
            magic: super::CONFIGURING_INPUT_MAGIC,
            dt_ns: base.dt_ns,
            mode: base.mode,
            timeout_to_operating_ns: base.timeout_to_operating_ns,
            loss_of_contact_limit: base.loss_of_contact_limit,
        }
    }

    /// Checks the packet marker and currently supported operating settings.
    ///
    /// Returns:
    ///   `true` for a marked roundtrip configuration within the supported
    ///   Deimos cycle-rate range.
    pub fn is_valid(&self) -> bool {
        self.magic == super::CONFIGURING_INPUT_MAGIC
            && self.dt_ns >= super::DEIMOS_MIN_CYCLE_PERIOD_NS
            && self.dt_ns <= super::DEIMOS_MAX_CYCLE_PERIOD_NS
            && matches!(self.mode, Mode::Roundtrip)
    }
}

/// Rev7 configuration response carrying the firmware calibration status.
///
/// Fields are serialized contiguously in little-endian order.
#[derive(ByteStruct, Clone, Copy, Debug, Default)]
#[byte_struct_le]
pub struct ConfiguringOutput {
    /// Direction- and state-specific packet marker.
    pub magic: u32,
    /// Firmware acceptance or rejection of the configuration.
    pub acknowledge: AcknowledgeConfiguration,
    /// Calibration status encoded as `0` for identity or `1` for calibrated.
    pub firmware_calibrated: u8,
}

impl ConfiguringOutput {
    /// Builds a configuration response with a canonical calibration flag.
    ///
    /// Args:
    ///   acknowledge: Firmware configuration result.
    ///   firmware_calibrated: Whether the installed coefficients were
    ///     produced by the calibration procedure.
    ///
    /// Returns:
    ///   Initialized rev7 configuration response.
    pub fn new(acknowledge: AcknowledgeConfiguration, firmware_calibrated: bool) -> Self {
        Self {
            magic: super::CONFIGURING_OUTPUT_MAGIC,
            acknowledge,
            firmware_calibrated: u8::from(firmware_calibrated),
        }
    }

    /// Checks the packet marker and calibration-flag encoding.
    ///
    /// Returns:
    ///   `true` when the marker matches and the flag is either `0` or `1`.
    pub fn is_valid(&self) -> bool {
        self.magic == super::CONFIGURING_OUTPUT_MAGIC && self.firmware_calibrated <= 1
    }
}
