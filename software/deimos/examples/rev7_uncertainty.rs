//! Uncertainty analysis for the Deimos DAQ Rev7 analog frontends.

// As of rev 7.0.1, 2026-07-12
//   | Component                    | Nominal Value | Error Rating | Thermal Sensitivity |
//   |------------------------------|---------------|--------------|---------------------|
//   | Amp gain set resistor        | 2 kohm        | 0.01%        | 5 ppm/C             |
//   | Amp gain                     | derived       | 0.03%        | 10 ppm/C            |
//   | Amp input offset             | 0 V           | 40 uV        | 0.4 uV/C            |
//   | Amp output offset            | 0 V           | 200 uV       | 2 uV/C              |
//   | Amp input bias current       | 35 nA         | 5 nA         |                     |
//   | Voltage reference for ADC    | 2.5 V         | 0.02%        | 2 ppm/C             |
//   | Voltage ref. for amp offset  | 1.024 V       | 0.05%        | 12 ppm/C            |
//   | Filter resistor              | 10 kohm       | 1%           | 50 ppm/C            |
//   | OVP jfet clamp leakage       | 1 nA @ 15V    |              |                     |
//   | Filter amp input offset      | 0 V           | 25 uV        | 0.5 uV/C            |
//   | Filter amp input bias current| 5pA           | 5 pA         |                     |

use nalgebra::SVector;
use num_dual::{DualNum, gradient};

// v:  inamp input voltage
// vf: filter block input voltage

// rg: inamp gainset resistor
// fg: inamp gain error factor
// voi: inamp input offset voltage
// voo: inamp output offset voltage
// voref: inamp output reference voltage

// voif: filter input offset voltage
// ibf:  filter bias current
// rf:   filter input resistance (each)
// iovp: filter over-voltage protection clamp leakage current

/// Gainset function for INA826
fn ina826_gain<D: DualNum<f64> + Copy>(rg: D, fg: D) -> D {
    (1.0 + (49.4e3 / rg)) * (1.0 + fg)
}

/// Voltage model for INA826
/// with no unbalanced input resistance -> no bias current error.
fn ina826<D: DualNum<f64> + Copy>(v: D, rg: D, fg: D, voi: D, voo: D, voref: D) -> D {
    let g = ina826_gain(rg, fg);
    let v_in = v + voi;
    let v_out = v_in * g + voo - voref;
    v_out
}

/// Voltage model for OPA196-based Sallen-Key filter
/// with roughly 3kHz cutoff.
fn opa196_3khz_filt<D: DualNum<f64> + Copy>(vf: D, voif: D, ibf: D, rf: D, iovp: D) -> D {
    // JFET clamp leakage current error.
    // In reality, this is extremely nonlinear and temperature-dependent, but
    // we can protect the absolute maximum of 1nA even though typical leakage
    // should be around the 10pA range.
    // Approx voltage drop across first filter resistor due to OVP leakage:
    let dvovp = iovp * rf; // One resistor on this path

    // Filter input bias current error (toward the +input).
    let dvbias = ibf * 2.0 * rf; // Two resistors on this path

    // Filter amp effective input voltage
    let vf_in = vf - dvovp - dvbias + voif;
    let vf_out = vf_in; // Unity gain; opamp output offset is included in input offset
    vf_out
}

/// +/-35mV frontend voltage model
fn frontend_35mv<D: DualNum<f64> + Copy>(
    v: D,
    rg: D,
    fg: D,
    voi: D,
    voo: D,
    voref: D,
    voif: D,
    ibf: D,
    rf: D,
    iovp: D,
) -> D {
    let vf = ina826(v, rg, fg, voi, voo, voref);
    opa196_3khz_filt(vf, voif, ibf, rf, iovp)
}

fn main() {}
