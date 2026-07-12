//! Uncertainty analysis for the Deimos DAQ Rev7 analog frontends.

use nalgebra::SVector;
use num_dual::{DualNum, gradient};
use plotly::{
    Configuration, Layout, Plot, Scatter,
    common::{DashType, Fill, Font, Line, Mode, Title},
    layout::{Axis, Legend, Margin},
};
use std::{fs, path::Path};

const SAMPLE_COUNT: usize = 51;
const INPUT_COUNT: usize = 10;
const INPUT_NAMES: [&str; INPUT_COUNT] = [
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
const FILL_COLORS: [&str; INPUT_COUNT] = [
    "rgba(0, 114, 178, 0.30)",
    "rgba(230, 159, 0, 0.30)",
    "rgba(0, 158, 115, 0.30)",
    "rgba(213, 94, 0, 0.30)",
    "rgba(204, 121, 167, 0.30)",
    "rgba(86, 180, 233, 0.30)",
    "rgba(148, 103, 189, 0.30)",
    "rgba(140, 86, 75, 0.30)",
    "rgba(127, 127, 127, 0.30)",
    "rgba(188, 189, 34, 0.30)",
];
const LINE_COLORS: [&str; INPUT_COUNT] = [
    "rgb(0, 114, 178)",
    "rgb(230, 159, 0)",
    "rgb(0, 158, 115)",
    "rgb(213, 94, 0)",
    "rgb(204, 121, 167)",
    "rgb(86, 180, 233)",
    "rgb(148, 103, 189)",
    "rgb(140, 86, 75)",
    "rgb(127, 127, 127)",
    "rgb(188, 189, 34)",
];

struct Theme {
    suffix: &'static str,
    color_scheme: &'static str,
    paper_background: &'static str,
    plot_background: &'static str,
    foreground: &'static str,
    grid: &'static str,
    thermal: &'static str,
}

struct PlotData<'a> {
    input_mv: &'a [f64],
    upper_uncertainty: &'a [f64],
    lower_uncertainty: &'a [f64],
    thermal_sensitivity_uv_per_c: &'a [f64],
    upper_boundaries: &'a [Vec<f64>; INPUT_COUNT],
    lower_boundaries: &'a [Vec<f64>; INPUT_COUNT],
}

fn themes() -> [Theme; 2] {
    [
        Theme {
            suffix: "light",
            color_scheme: "light",
            paper_background: "#ffffff",
            plot_background: "#ffffff",
            foreground: "#171922",
            grid: "#d8deea",
            thermal: "#00796b",
        },
        Theme {
            suffix: "dark",
            color_scheme: "dark",
            paper_background: "#2b2f3a",
            plot_background: "#2b2f3a",
            foreground: "#f2f0f6",
            grid: "#47404f",
            thermal: "#2dd4bf",
        },
    ]
}

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
fn frontend_35mv<D: DualNum<f64> + Copy>(x: SVector<D, INPUT_COUNT>) -> D {
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

/// Linearized uncertainty and thermal sensitivity in output voltage of
/// the +/-35mV frontend at a given input voltage.
fn frontend_35mv_uncertainty(
    v: f64,
) -> (
    f64,
    SVector<f64, INPUT_COUNT>,
    SVector<f64, INPUT_COUNT>,
    SVector<f64, INPUT_COUNT>,
) {
    let nominal = SVector::<f64, INPUT_COUNT>::from([
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

    let uncertainty = SVector::<f64, INPUT_COUNT>::from([
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

    let thermal_sensitivity = SVector::<f64, INPUT_COUNT>::from([
        0.0,           // v
        5e-6 * 2e3,    // rg
        10e-6,         // fg
        0.4e-6,        // voi
        2e-6,          // voo
        12e-6 * 1.024, // voref
        0.5e-6,        // voif
        0.0,           // ibf
        50e-6 * 10e3,  // rf
        0.0,           // iovp
    ]);

    let (value, gradient) = gradient(frontend_35mv, &nominal);
    let uncertainty_components = gradient.component_mul(&uncertainty);
    let thermal_sensitivity_components = gradient.component_mul(&thermal_sensitivity);

    (
        value,
        gradient,
        uncertainty_components,
        thermal_sensitivity_components,
    )
}

/// Allocate the total norm among components according to each component's
/// leave-one-out reduction in that norm.
fn uncertainty_blame(components: &SVector<f64, INPUT_COUNT>) -> SVector<f64, INPUT_COUNT> {
    let total_squared = components.norm_squared();
    if total_squared == 0.0 {
        return SVector::zeros();
    }

    let total = total_squared.sqrt();
    let marginal = components.map(|component| {
        let without_component = (total_squared - component * component).max(0.0).sqrt();
        total - without_component
    });
    let marginal_sum = marginal.sum();

    if marginal_sum > 0.0 {
        marginal / marginal_sum
    } else {
        SVector::zeros()
    }
}

fn add_uncertainty_bands(
    plot: &mut Plot,
    input_mv: &[f64],
    nominal: &[f64],
    boundaries: &[Vec<f64>; INPUT_COUNT],
    upper: bool,
) {
    plot.add_trace(
        Scatter::new(input_mv.to_vec(), nominal.to_vec())
            .mode(Mode::Lines)
            .line(Line::new().width(0.0))
            .show_legend(false),
    );

    let mut previous = nominal;
    for component in 0..INPUT_COUNT {
        let boundary = &boundaries[component];
        let has_area = boundary
            .iter()
            .zip(previous)
            .any(|(current, previous)| (current - previous).abs() > f64::EPSILON);
        previous = boundary;
        if !has_area {
            continue;
        }

        let trace = Scatter::new(input_mv.to_vec(), boundaries[component].clone())
            .mode(Mode::Lines)
            .name(INPUT_NAMES[component])
            .legend_group(INPUT_NAMES[component])
            .show_legend(upper)
            .fill(Fill::ToNextY)
            .fill_color(FILL_COLORS[component])
            .line(Line::new().color(LINE_COLORS[component]).width(0.5));
        plot.add_trace(trace);
    }
}

fn axis(theme: &Theme, title: &str) -> Axis {
    Axis::new()
        .title(Title::with_text(title))
        .color(theme.foreground)
        .show_line(true)
        .line_color(theme.foreground)
        .line_width(1)
        .show_grid(true)
        .grid_color(theme.grid)
        .grid_width(1)
        .zero_line(true)
        .zero_line_color(theme.grid)
        .auto_margin(true)
        .tick_font(Font::new().color(theme.foreground))
}

fn build_plot(theme: &Theme, data: &PlotData<'_>) -> Plot {
    let mut plot = Plot::new();
    plot.set_configuration(Configuration::new().responsive(true).display_logo(false));

    let zero_uncertainty = vec![0.0; SAMPLE_COUNT];
    add_uncertainty_bands(
        &mut plot,
        data.input_mv,
        &zero_uncertainty,
        data.upper_boundaries,
        true,
    );
    add_uncertainty_bands(
        &mut plot,
        data.input_mv,
        &zero_uncertainty,
        data.lower_boundaries,
        false,
    );

    plot.add_trace(
        Scatter::new(data.input_mv.to_vec(), data.upper_uncertainty.to_vec())
            .mode(Mode::Lines)
            .name("+ total uncertainty")
            .line(
                Line::new()
                    .color(theme.foreground)
                    .width(1.5)
                    .dash(DashType::Dash),
            ),
    );
    plot.add_trace(
        Scatter::new(data.input_mv.to_vec(), data.lower_uncertainty.to_vec())
            .mode(Mode::Lines)
            .name("- total uncertainty")
            .line(
                Line::new()
                    .color(theme.foreground)
                    .width(1.5)
                    .dash(DashType::Dash),
            ),
    );
    plot.add_trace(
        Scatter::new(
            data.input_mv.to_vec(),
            data.thermal_sensitivity_uv_per_c.to_vec(),
        )
        .mode(Mode::Lines)
        .name("Worst-case thermal sensitivity")
        .x_axis("x2")
        .y_axis("y2")
        .line(Line::new().color(theme.thermal).width(2.0)),
    );

    plot.set_layout(
        Layout::new()
            .title(Title::with_text("Rev7 +/-35 mV frontend uncertainty"))
            .height(900)
            .font(Font::new().color(theme.foreground))
            .paper_background_color(theme.paper_background)
            .plot_background_color(theme.plot_background)
            .margin(Margin::new().left(80).right(280).top(90).bottom(70))
            .legend(
                Legend::new()
                    .font(Font::new().color(theme.foreground))
                    .x(1.02)
                    .y(1.0),
            )
            .x_axis(
                axis(theme, "Input voltage [mV]")
                    .domain(&[0.0, 1.0])
                    .anchor("y"),
            )
            .y_axis(
                axis(theme, "Output uncertainty [mV]")
                    .domain(&[0.45, 1.0])
                    .anchor("x"),
            )
            .x_axis2(
                axis(theme, "Input voltage [mV]")
                    .domain(&[0.0, 1.0])
                    .anchor("y2"),
            )
            .y_axis2(
                axis(theme, "Thermal sensitivity [uV/C]")
                    .domain(&[0.0, 0.30])
                    .anchor("x2"),
            ),
    );

    plot
}

fn write_themed_html(plot: &Plot, path: &Path, theme: &Theme) {
    let page_style = format!(
        "<style>:root {{ color-scheme: {}; }} \
         html, body {{ margin: 0; background: {}; }}</style>",
        theme.color_scheme, theme.paper_background
    );
    let html = plot
        .to_html()
        .replace("</head>", &format!("{page_style}\n</head>"));
    fs::write(path, html).expect("failed to write uncertainty plot");
}

fn main() {
    let mut input_mv = Vec::with_capacity(SAMPLE_COUNT);
    let mut upper_uncertainty = Vec::with_capacity(SAMPLE_COUNT);
    let mut lower_uncertainty = Vec::with_capacity(SAMPLE_COUNT);
    let mut thermal_sensitivity_uv_per_c = Vec::with_capacity(SAMPLE_COUNT);
    let mut upper_boundaries: [Vec<f64>; INPUT_COUNT] =
        std::array::from_fn(|_| Vec::with_capacity(SAMPLE_COUNT));
    let mut lower_boundaries: [Vec<f64>; INPUT_COUNT] =
        std::array::from_fn(|_| Vec::with_capacity(SAMPLE_COUNT));

    for sample in 0..SAMPLE_COUNT {
        let fraction = sample as f64 / (SAMPLE_COUNT - 1) as f64;
        let input = -35e-3 + fraction * 70e-3;
        let (_value, _gradient, uncertainty_components, thermal_components) =
            frontend_35mv_uncertainty(input);
        let total_uncertainty = uncertainty_components.norm();
        let blame = uncertainty_blame(&uncertainty_components);

        input_mv.push(input * 1e3);
        lower_uncertainty.push(-total_uncertainty * 1e3);
        upper_uncertainty.push(total_uncertainty * 1e3);
        thermal_sensitivity_uv_per_c.push(thermal_components.abs().sum() * 1e6);

        let mut upper = 0.0;
        let mut lower = 0.0;
        for component in 0..INPUT_COUNT {
            let contribution = total_uncertainty * blame[component] * 1e3;
            upper += contribution;
            lower -= contribution;
            upper_boundaries[component].push(upper);
            lower_boundaries[component].push(lower);
        }
    }

    let data = PlotData {
        input_mv: &input_mv,
        upper_uncertainty: &upper_uncertainty,
        lower_uncertainty: &lower_uncertainty,
        thermal_sensitivity_uv_per_c: &thermal_sensitivity_uv_per_c,
        upper_boundaries: &upper_boundaries,
        lower_boundaries: &lower_boundaries,
    };
    let output_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../deimos_website/docs/assets");

    for theme in themes() {
        let plot = build_plot(&theme, &data);
        let output_path = output_dir.join(format!("rev7_uncertainty_{}.html", theme.suffix));
        write_themed_html(&plot, &output_path, &theme);
        println!("Wrote {}", output_path.display());
    }
}
