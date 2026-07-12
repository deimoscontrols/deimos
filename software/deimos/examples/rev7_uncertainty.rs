//! Uncertainty analysis for the Deimos DAQ Rev7 analog frontends.

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
    (rg.recip() * 49.4e3 + 1.0) * (fg + 1.0)
}

/// Voltage model for INA826
/// with no unbalanced input resistance -> no bias current error.
fn ina826<D: DualNum<f64> + Copy>(v: D, rg: D, fg: D, voi: D, voo: D, voref: D) -> D {
    let g = ina826_gain(rg, fg);
    let v_in = v + voi;
    v_in * g + voo + voref
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
fn frontend_35mv<D: DualNum<f64> + Copy>(x: SVector<D, 10>) -> D {
    let vf = ina826(x[0], x[1], x[2], x[3], x[4], x[5]);
    opa196_3khz_filt(vf, x[6], x[7], x[8], x[9])
}

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

/// Linearized uncertainty in output voltage of
/// the +/-35mV frontend at a given input voltage.
fn frontend_35mv_uncertainty(v: f64) -> (f64, SVector<f64, 10>, SVector<f64, 10>) {
    let nominal = SVector::<f64, 10>::from([
        v,     // v
        2e3,   // rg
        0.0,   // fg
        0.0,   // voi
        0.0,   // voo
        1.024, // voref
        0.0,   // voif
        5e-12, // ibf
        10e3,  // rf
        0.0,   // iovp
    ]);

    let uncertainty = SVector::<f64, 10>::from([
        0.0,             // v
        0.01e-2 * 2e3,   // rg
        0.03e-2,         // fg
        40e-6,           // voi
        200e-6,          // voo
        0.05e-2 * 1.024, // voref
        25e-6,           // voif
        5e-12,           // ibf
        0.01 * 10e3,     // rf
        1e-9,            // iovp
    ]);

    let (value, gradient) = gradient(frontend_35mv, &nominal);
    let uncertainty_components = gradient.component_mul(&uncertainty);

    (value, gradient, uncertainty_components)
}

fn main() {
    for input in [-35e-3, 0.0, 35e-3] {
        let (value, _gradient, uncertainty_components) = frontend_35mv_uncertainty(input);
        let output_uncertainty = uncertainty_components.norm();

        println!("{value} +/- {output_uncertainty}");
    }
}
