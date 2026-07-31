//! Rev7 component and measurement uncertainty definitions.
//!
//! This module owns the hardware-specific nominal values, rated uncertainties,
//! and thermal sensitivities used by the host-side uncertainty analysis. The
//! numerical differentiation and plotting remain in the executable example.
//!
//! References:
//!   \[1\] Texas Instruments, *INA826 Precision Instrumentation Amplifier*,
//!   data sheet.
//!   \[2\] Texas Instruments, *OPA196 36-V, Precision, Rail-to-Rail Input/Output,
//!   Low Offset Voltage Operational Amplifier*, data sheet.

/// Number of independent inputs in the rev7 +/-35 mV frontend uncertainty model.
pub const FRONTEND_35MV_UNCERTAINTY_INPUT_COUNT: usize = 10;

/// Human-readable component names in uncertainty-vector order.
pub const FRONTEND_35MV_UNCERTAINTY_INPUT_NAMES: [&str; FRONTEND_35MV_UNCERTAINTY_INPUT_COUNT] = [
    "Input voltage",
    "Gain-set resistor",
    "Amplifier gain",
    "Amplifier input offset",
    "Amplifier output offset",
    "Amplifier reference",
    "Filter amplifier input offset",
    "Filter amplifier bias current",
    "Filter resistor",
    "OVP clamp leakage",
];

/// Inputs to one linearized +/-35 mV frontend uncertainty evaluation.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Frontend35MvUncertaintyInputs {
    /// Nominal model inputs with shape
    /// `(FRONTEND_35MV_UNCERTAINTY_INPUT_COUNT,)`.
    pub nominal: [f64; FRONTEND_35MV_UNCERTAINTY_INPUT_COUNT],
    /// Absolute rated input uncertainties with the units of each nominal value.
    pub uncertainty: [f64; FRONTEND_35MV_UNCERTAINTY_INPUT_COUNT],
    /// Absolute input sensitivity per degree Celsius with shape
    /// `(FRONTEND_35MV_UNCERTAINTY_INPUT_COUNT,)`.
    pub thermal_sensitivity_per_c: [f64; FRONTEND_35MV_UNCERTAINTY_INPUT_COUNT],
}

/// Return component specifications for the rev7 +/-35 mV frontend model.
///
/// Vector order is input voltage, INA826 gain-set resistance, INA826 fractional
/// gain error, INA826 input offset, INA826 output offset, amplifier reference,
/// OPA196 input offset, OPA196 bias current, filter resistance, and protection
/// clamp leakage.
///
/// Args:
///   input_v: Nominal frontend input voltage scalar in `V`.
///
/// Returns:
///   Nominal values, absolute rated uncertainties, and thermal sensitivities
///   with shape `(FRONTEND_35MV_UNCERTAINTY_INPUT_COUNT,)`.
pub fn frontend_35mv_uncertainty_inputs(input_v: f64) -> Frontend35MvUncertaintyInputs {
    Frontend35MvUncertaintyInputs {
        nominal: [
            input_v, // v
            2.0e3,   // rg
            0.0,     // fg
            0.0,     // voi
            0.0,     // voo
            1.024,   // voref
            0.0,     // voif
            5.0e-12, // ibf
            10.0e3,  // rf
            0.0,     // iovp
        ],
        uncertainty: [
            0.0,             // v
            0.01e-2 * 2.0e3, // rg
            0.03e-2,         // fg
            40.0e-6,         // voi
            200.0e-6,        // voo
            0.05e-2 * 1.024, // voref
            25.0e-6,         // voif
            5.0e-12,         // ibf
            0.01 * 10.0e3,   // rf
            1.0e-9,          // iovp
        ],
        thermal_sensitivity_per_c: [
            0.0,              // v
            5.0e-6 * 2.0e3,   // rg
            10.0e-6,          // fg
            0.4e-6,           // voi
            2.0e-6,           // voo
            12.0e-6 * 1.024,  // voref
            0.5e-6,           // voif
            0.0,              // ibf
            50.0e-6 * 10.0e3, // rf
            0.0,              // iovp
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frontend_35mv_specification_preserves_input_and_component_order() {
        let inputs = frontend_35mv_uncertainty_inputs(-0.035);
        assert_eq!(inputs.nominal[0], -0.035);
        assert_eq!(inputs.nominal[1], 2.0e3);
        assert_eq!(inputs.nominal[5], 1.024);
        assert_eq!(inputs.nominal[8], 10.0e3);
        assert_eq!(inputs.uncertainty[9], 1.0e-9);
        assert_eq!(inputs.thermal_sensitivity_per_c[7], 0.0);
    }
}
