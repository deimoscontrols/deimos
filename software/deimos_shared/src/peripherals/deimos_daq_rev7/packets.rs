//! Setup packets and calibration image records.
//!
//! This module contains the binding, configuration, and operating packet
//! definitions.

mod operating;

pub use super::calc::{Calibration, LinearCalibration};
pub use operating::*;

use byte_struct::{ByteStruct, ByteStructLen, ByteStructUnspecifiedByteOrder};

use crate::{
    peripherals::PeripheralId,
    states::{AcknowledgeConfiguration, ConfiguringInput as BaseConfiguringInput},
};

/// Device-specific binding request.
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
    /// Builds a binding request with the required packet marker.
    ///
    /// Args:
    ///   configuring_timeout_ms: Maximum configuring-state inactivity
    ///     duration in `ms`.
    ///
    /// Returns:
    ///   Initialized binding request.
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

/// Device-specific binding response.
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
    /// Builds a binding response with the required packet marker.
    ///
    /// Args:
    ///   peripheral_id: Model and serial-number identity of the board.
    ///
    /// Returns:
    ///   Initialized binding response.
    pub fn new(peripheral_id: PeripheralId) -> Self {
        Self {
            magic: super::BINDING_OUTPUT_MAGIC,
            peripheral_id,
        }
    }

    /// Checks the packet marker and model number.
    ///
    /// Returns:
    ///   `true` when the response has the expected marker and identifies this
    ///   board model.
    pub fn is_valid(&self) -> bool {
        self.magic == super::BINDING_OUTPUT_MAGIC
            && self.peripheral_id.model_number == super::MODEL_NUMBER
    }
}

/// Configuration request with a direction-specific packet marker.
///
/// Fields are serialized contiguously in little-endian order.
#[derive(ByteStruct, Clone, Copy, Debug, Default)]
#[byte_struct_le]
pub struct ConfiguringInput {
    /// Direction- and state-specific packet marker.
    pub magic: u32,
    /// Nominal operating-cycle duration in `ns`.
    pub dt_ns: u32,
    /// Delay from accepted configuration to operating entry in `ns`.
    pub timeout_to_operating_ns: u32,
    /// Consecutive missed cycles allowed before loss-of-contact shutdown.
    pub loss_of_contact_limit: u16,
}

impl ConfiguringInput {
    /// Adds the device-specific packet marker to a generic configuration request.
    ///
    /// Args:
    ///   base: Generic Deimos configuration values.
    ///
    /// Returns:
    ///   Equivalent device-specific request.
    pub fn from_base(base: BaseConfiguringInput) -> Self {
        Self {
            magic: super::CONFIGURING_INPUT_MAGIC,
            dt_ns: base.dt_ns,
            timeout_to_operating_ns: base.timeout_to_operating_ns,
            loss_of_contact_limit: base.loss_of_contact_limit,
        }
    }

    /// Checks the packet marker and supported Deimos cycle period.
    ///
    /// Returns:
    ///   `true` for a marked configuration within the supported Deimos
    ///   cycle-rate range.
    pub fn is_valid(&self) -> bool {
        matches!(
            self.validation_acknowledgement(),
            Some(AcknowledgeConfiguration::Ack)
        )
    }

    /// Classifies a marked configuration request for its protocol response.
    ///
    /// Returns:
    ///   `None` when the packet marker is invalid and the datagram should be
    ///   ignored, or the specific acknowledgment to send for a marked request.
    pub fn validation_acknowledgement(&self) -> Option<AcknowledgeConfiguration> {
        if self.magic != super::CONFIGURING_INPUT_MAGIC {
            return None;
        }
        if self.dt_ns < super::DEIMOS_MIN_CYCLE_PERIOD_NS {
            return Some(AcknowledgeConfiguration::NakDtTooSmall);
        }
        if self.dt_ns > super::DEIMOS_MAX_CYCLE_PERIOD_NS {
            return Some(AcknowledgeConfiguration::NakDtTooLarge);
        }
        Some(AcknowledgeConfiguration::Ack)
    }
}

/// Configuration response carrying the firmware calibration status.
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
    ///   Initialized configuration response.
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
