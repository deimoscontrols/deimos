//! Integer id for each type of device
use super::ModelNumber;

/// Model number for local experimental usage.
/// Do not ship hardware using this model number.
pub const EXPERIMENTAL_MODEL_NUMBER: ModelNumber = 0;

/// Integrated analog frontend unit
pub const ANALOG_I_REV_2_MODEL_NUMBER: ModelNumber = 1;

/// Integrated analog frontend unit
pub const ANALOG_I_REV_3_MODEL_NUMBER: ModelNumber = 2;

/// Integrated analog frontend unit
pub const ANALOG_I_REV_4_MODEL_NUMBER: ModelNumber = 3;
