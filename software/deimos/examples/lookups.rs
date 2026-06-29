//! Generate documentation plots for sensor lookup functions.
//!
//! The plots are written into the website assets folder so they can be viewed
//! from the generated documentation without adding plotting dependencies to the
//! runtime crate.

use std::fs;
use std::path::{Path, PathBuf};

use deimos::calc::{ktype_temp_k, ktype_voltage_v, pt100_resistance_ohm, pt100_temp_k};

const ZERO_C_K: f64 = 273.15;

const TC_FORWARD_MIN_C: f64 = -270.0;
const TC_MAX_C: f64 = 1370.0;
const TC_TEMP_STEP_C: f64 = 1.0;
const TC_VOLTAGE_STEP_MV: f64 = 0.05;

const RTD_MIN_C: f64 = -200.0;
const RTD_MAX_C: f64 = 850.0;
const RTD_TEMP_STEP_C: f64 = 1.0;
const RTD_RESISTANCE_STEP_OHM: f64 = 0.1;
const REPORT_MAX_WIDTH: &str = "40rem";

/// Generate Plotly HTML for the forward and inverse thermocouple and RTD lookups.
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let output_dir = output_dir();
    fs::create_dir_all(&output_dir)?;

    for sensor in sensor_reports() {
        for theme in themes() {
            let output_path =
                output_dir.join(format!("{}_{}.html", sensor.file_stem, theme.suffix));
            let html = render_html(&sensor, &theme)?;
            fs::write(&output_path, html)?;
            println!("Wrote {}", output_path.display());
        }
    }

    remove_legacy_combined_report(&output_dir)?;

    Ok(())
}

/// Return the website asset folder relative to the deimos crate.
fn output_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../deimos_website/docs/assets")
}

/// Build one standalone HTML report for a single sensor type and theme.
fn render_html(sensor: &SensorReport, theme: &Theme) -> Result<String, serde_json::Error> {
    let error_div = if sensor.lookup.inverse_error.is_some() {
        r#"        <div id="inverse-error" class="plot"></div>
"#
    } else {
        ""
    };
    let error_script = if let Some(error) = &sensor.lookup.inverse_error {
        format!(
            r#"
const inverseErrorX = {x};
const inverseErrorY = {y};

Plotly.newPlot(
    "inverse-error",
    [lineTrace(inverseErrorX, inverseErrorY)],
    layout("{title}", "{x_title}", "{y_title}"),
    config,
);
"#,
            x = to_json(&error.x)?,
            y = to_json(&error.y)?,
            title = error.title,
            x_title = error.x_title,
            y_title = error.y_title,
        )
    } else {
        String::new()
    };

    Ok(format!(
        r##"<!doctype html>
<html lang="en">
<head>
    <meta charset="utf-8">
    <title>{page_title}</title>
    <script src="https://cdn.plot.ly/plotly-3.0.1.min.js"></script>
    <style>
        :root {{
            color-scheme: {color_scheme};
            font-family: system-ui, -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif;
        }}
        body {{
            margin: 0;
            background: {body_background};
            color: {text_color};
        }}
        main {{
            max-width: {report_max_width};
            margin: 0 auto;
            padding: 2rem 1rem 3rem;
        }}
        h1 {{
            margin: 0 0 0.5rem;
            font-size: 1.75rem;
            line-height: 1.2;
        }}
        p {{
            margin: 0 0 1.5rem;
            color: {muted_text_color};
        }}
        .plot-grid {{
            display: grid;
            gap: 1rem;
        }}
        .plot {{
            min-height: 28rem;
            border: 1px solid {border_color};
            border-radius: 0.5rem;
        }}
    </style>
</head>
<body>
<main>
    <h1>{page_title}</h1>
    <p>{description}</p>
    <div class="plot-grid">
        <div id="forward" class="plot"></div>
        <div id="inverse" class="plot"></div>
        <div id="sensitivity" class="plot"></div>
{error_div}
    </div>
</main>
<script>
const forwardTempC = {forward_temperature_c};
const forwardOutput = {forward_output};
const inverseInput = {inverse_input};
const inverseTempC = {inverse_temperature_c};
const sensitivityTempC = {sensitivity_temperature_c};
const sensitivity = {sensitivity};

const config = {{
    responsive: true,
    displaylogo: false,
}};

function layout(title, xTitle, yTitle) {{
    return {{
        title: {{ text: title }},
        margin: {{ l: 70, r: 24, t: 64, b: 64 }},
        paper_bgcolor: "{paper_background}",
        plot_bgcolor: "{plot_background}",
        font: {{ color: "{text_color}" }},
        xaxis: {{
            title: {{ text: xTitle }},
            gridcolor: "{grid_color}",
            zerolinecolor: "{zero_line_color}",
        }},
        yaxis: {{
            title: {{ text: yTitle }},
            gridcolor: "{grid_color}",
            zerolinecolor: "{zero_line_color}",
        }},
        showlegend: false,
    }};
}}

function lineTrace(x, y) {{
    return {{
        x,
        y,
        type: "scatter",
        mode: "lines",
        line: {{ color: "{trace_color}", width: 2 }},
    }};
}}

Plotly.newPlot(
    "forward",
    [lineTrace(forwardTempC, forwardOutput)],
    layout("{forward_title}", "Temperature (C)", "{forward_y_title}"),
    config,
);

Plotly.newPlot(
    "inverse",
    [lineTrace(inverseInput, inverseTempC)],
    layout("{inverse_title}", "{inverse_x_title}", "Temperature (C)"),
    config,
);

Plotly.newPlot(
    "sensitivity",
    [lineTrace(sensitivityTempC, sensitivity)],
    layout("{sensitivity_title}", "{sensitivity_x_title}", "{sensitivity_y_title}"),
    config,
);
{error_script}
</script>
</body>
</html>
"##,
        page_title = sensor.page_title,
        description = sensor.description,
        report_max_width = REPORT_MAX_WIDTH,
        color_scheme = theme.color_scheme,
        body_background = theme.body_background,
        paper_background = theme.paper_background,
        plot_background = theme.plot_background,
        text_color = theme.text_color,
        muted_text_color = theme.muted_text_color,
        border_color = theme.border_color,
        grid_color = theme.grid_color,
        zero_line_color = theme.zero_line_color,
        trace_color = theme.trace_color,
        forward_temperature_c = to_json(&sensor.lookup.forward_temperature_c)?,
        forward_output = to_json(&sensor.lookup.forward_output)?,
        inverse_input = to_json(&sensor.lookup.inverse_input)?,
        inverse_temperature_c = to_json(&sensor.lookup.inverse_temperature_c)?,
        sensitivity_temperature_c = to_json(&sensor.lookup.sensitivity.x)?,
        sensitivity = to_json(&sensor.lookup.sensitivity.y)?,
        forward_title = sensor.forward_title,
        forward_y_title = sensor.forward_y_title,
        inverse_title = sensor.inverse_title,
        inverse_x_title = sensor.inverse_x_title,
        sensitivity_title = sensor.lookup.sensitivity.title,
        sensitivity_x_title = sensor.lookup.sensitivity.x_title,
        sensitivity_y_title = sensor.lookup.sensitivity.y_title,
        error_div = error_div,
        error_script = error_script,
    ))
}

/// Define each standalone sensor report generated by this example.
fn sensor_reports() -> Vec<SensorReport> {
    vec![
        SensorReport {
            file_stem: "ktype_lookup",
            page_title: "K-type Thermocouple Lookup",
            description: "K-type thermocouple curves from ITS-90 polynomial fits.",
            forward_title: "K-type Thermocouple Forward Lookup",
            forward_y_title: "Voltage (mV)",
            inverse_title: "K-type Thermocouple Inverse Lookup",
            inverse_x_title: "Voltage (mV)",
            lookup: thermocouple_lookup_data(),
        },
        SensorReport {
            file_stem: "pt100_lookup",
            page_title: "Pt100 RTD Lookup",
            description: "Pt100 RTD lookups based on DIN-43-760 tables.",
            forward_title: "Pt100 RTD Forward Lookup",
            forward_y_title: "Resistance (ohm)",
            inverse_title: "Pt100 RTD Inverse Lookup",
            inverse_x_title: "Resistance (ohm)",
            lookup: rtd_lookup_data(),
        },
    ]
}

/// Return the light and dark palettes used for lookup plot output.
fn themes() -> [Theme; 2] {
    [
        Theme {
            suffix: "light",
            color_scheme: "light",
            body_background: "radial-gradient(circle at top left, rgba(130, 50, 186, 0.16), transparent 32%), linear-gradient(180deg, #ffffff 0%, #f6f7fb 100%)",
            paper_background: "#ffffff",
            plot_background: "#ffffff",
            text_color: "#171922",
            muted_text_color: "#171922",
            border_color: "#d7dce2",
            grid_color: "#d8deea",
            zero_line_color: "#d8deea",
            trace_color: "#000000",
        },
        Theme {
            suffix: "dark",
            color_scheme: "dark",
            body_background: "radial-gradient(circle at top left, rgba(172, 55, 255, 0.24), transparent 30%), linear-gradient(180deg, #242833 0%, #1e2129 100%)",
            paper_background: "#2b2f3a",
            plot_background: "#2b2f3a",
            text_color: "#f2f0f6",
            muted_text_color: "#f2f0f6",
            border_color: "#47404f",
            grid_color: "#47404f",
            zero_line_color: "#47404f",
            trace_color: "#f2f0f6",
        },
    ]
}

/// Remove the old combined report generated by earlier versions of this example.
fn remove_legacy_combined_report(output_dir: &Path) -> Result<(), std::io::Error> {
    let legacy_path = output_dir.join("lookup_tables.html");
    if legacy_path.exists() {
        fs::remove_file(&legacy_path)?;
        println!("Removed {}", legacy_path.display());
    }

    Ok(())
}

/// Static metadata and sampled lookup points for one sensor report.
struct SensorReport {
    file_stem: &'static str,
    page_title: &'static str,
    description: &'static str,
    forward_title: &'static str,
    forward_y_title: &'static str,
    inverse_title: &'static str,
    inverse_x_title: &'static str,
    lookup: LookupData,
}

/// Color choices for one generated report theme.
struct Theme {
    suffix: &'static str,
    color_scheme: &'static str,
    body_background: &'static str,
    paper_background: &'static str,
    plot_background: &'static str,
    text_color: &'static str,
    muted_text_color: &'static str,
    border_color: &'static str,
    grid_color: &'static str,
    zero_line_color: &'static str,
    trace_color: &'static str,
}

/// Hold sampled points for a forward lookup and its inverse lookup.
struct LookupData {
    forward_temperature_c: Vec<f64>,
    forward_output: Vec<f64>,
    inverse_input: Vec<f64>,
    inverse_temperature_c: Vec<f64>,
    sensitivity: ExtraPlotData,
    inverse_error: Option<ExtraPlotData>,
}

/// Extra diagnostic plot data for lookup reports.
struct ExtraPlotData {
    title: &'static str,
    x_title: &'static str,
    y_title: &'static str,
    x: Vec<f64>,
    y: Vec<f64>,
}

/// Sample the K-type thermocouple forward and inverse conversion functions.
fn thermocouple_lookup_data() -> LookupData {
    let forward_temperature_c = inclusive_range(TC_FORWARD_MIN_C, TC_MAX_C, TC_TEMP_STEP_C);
    let forward_output = forward_temperature_c
        .iter()
        .map(|temperature_c| 1000.0 * ktype_voltage_v(temperature_c + ZERO_C_K))
        .collect::<Vec<_>>();

    let min_voltage_mv = 1000.0 * ktype_voltage_v(TC_FORWARD_MIN_C + ZERO_C_K);
    let max_voltage_mv = 1000.0 * ktype_voltage_v(TC_MAX_C + ZERO_C_K);
    let inverse_input = inclusive_range(min_voltage_mv, max_voltage_mv, TC_VOLTAGE_STEP_MV);
    let inverse_temperature_c = inverse_input
        .iter()
        .map(|voltage_mv| ktype_temp_k(voltage_mv / 1000.0) - ZERO_C_K)
        .collect::<Vec<_>>();
    let inverse_error_y = forward_temperature_c
        .iter()
        .zip(forward_output.iter())
        .map(|(&temperature_c, &voltage_mv)| {
            ktype_temp_k(voltage_mv / 1000.0) - ZERO_C_K - temperature_c
        })
        .collect();
    let sensitivity = sensitivity_from_forward_curve(&forward_temperature_c, &forward_output);

    LookupData {
        forward_temperature_c,
        forward_output,
        inverse_input,
        inverse_temperature_c,
        sensitivity: ExtraPlotData {
            title: "K-type Thermocouple Sensitivity",
            x_title: "Temperature (C)",
            y_title: "Sensitivity (K/mV)",
            x: inclusive_range(TC_FORWARD_MIN_C, TC_MAX_C, TC_TEMP_STEP_C),
            y: sensitivity,
        },
        inverse_error: Some(ExtraPlotData {
            title: "K-type Thermocouple Inverse Error",
            x_title: "Temperature (C)",
            y_title: "Temperature Error (K)",
            x: inclusive_range(TC_FORWARD_MIN_C, TC_MAX_C, TC_TEMP_STEP_C),
            y: inverse_error_y,
        }),
    }
}

/// Sample the Pt100 RTD forward and inverse conversion functions.
fn rtd_lookup_data() -> LookupData {
    let forward_temperature_c = inclusive_range(RTD_MIN_C, RTD_MAX_C, RTD_TEMP_STEP_C);
    let forward_output = forward_temperature_c
        .iter()
        .map(|temperature_c| pt100_resistance_ohm(temperature_c + ZERO_C_K))
        .collect::<Vec<_>>();

    let min_resistance_ohm = forward_output[0];
    let max_resistance_ohm = *forward_output.last().unwrap();
    let inverse_input = inclusive_range(
        min_resistance_ohm,
        max_resistance_ohm,
        RTD_RESISTANCE_STEP_OHM,
    );
    let inverse_temperature_c = inverse_input
        .iter()
        .map(|resistance_ohm| pt100_temp_k(*resistance_ohm) - ZERO_C_K)
        .collect();
    let sensitivity = sensitivity_from_forward_curve(&forward_temperature_c, &forward_output);

    LookupData {
        forward_temperature_c,
        forward_output,
        inverse_input,
        inverse_temperature_c,
        sensitivity: ExtraPlotData {
            title: "Pt100 RTD Sensitivity",
            x_title: "Temperature (C)",
            y_title: "Sensitivity (K/ohm)",
            x: inclusive_range(RTD_MIN_C, RTD_MAX_C, RTD_TEMP_STEP_C),
            y: sensitivity,
        },
        inverse_error: None,
    }
}

/// Estimate calc-direction sensitivity from the forward electrical-output curve.
fn sensitivity_from_forward_curve(temperature_c: &[f64], output: &[f64]) -> Vec<f64> {
    (0..temperature_c.len())
        .map(|idx| {
            let (low, high) = if idx == 0 {
                (0, 1)
            } else if idx == temperature_c.len() - 1 {
                (idx - 1, idx)
            } else {
                (idx - 1, idx + 1)
            };

            (temperature_c[high] - temperature_c[low]) / (output[high] - output[low])
        })
        .collect()
}

/// Serialize a numeric vector for embedding as a JavaScript array.
fn to_json(values: &[f64]) -> Result<String, serde_json::Error> {
    serde_json::to_string(values)
}

/// Build an inclusive floating-point range using a fixed positive step.
fn inclusive_range(start: f64, end: f64, step: f64) -> Vec<f64> {
    let mut values = Vec::new();
    let mut value = start;

    while value <= end {
        values.push(value);
        value += step;
    }

    if values.last().is_none_or(|last| *last < end) {
        values.push(end);
    }

    values
}
