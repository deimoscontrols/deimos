use std::{
    env,
    error::Error,
    f64::consts::TAU,
    fs, io,
    path::{Path, PathBuf},
    process::Command,
};

use deimos_numerics::control::lti::{
    design_digital_filter_tf, BodeData, ContinuousTransferFunction, DigitalFilterFamily,
    DigitalFilterSpec, DiscreteTransferFunction, FilterShape,
};
use deimos_shared::peripherals::deimos_daq_rev7::{
    adc_analog_frontend_transfer_functions, adc_fractional_delay_transfer_functions,
    adc_sampled_bode_data, ADC_FILTER_MAX_CUTOFF_RATIO, ADC_FILTER_ORDER, ADC_SAMPLE_RATE_HZ,
};
use plotly::{
    common::{Anchor, DashType, Font, Line, Mode, Title, Visible},
    layout::{Annotation, Axis, AxisRange, AxisType, Layout, Margin, Shape, ShapeLine, ShapeType},
    plotly_static::StaticExporterBuilder,
    prelude::ExporterSyncExt,
    ImageFormat, Plot, Scatter,
};
use serde_json::{json, Value};

const WIDTH: usize = 1200;
const HEIGHT: usize = 850;
const MAGNITUDE_DOMAIN_START: f64 = 0.57;
const MAGNITUDE_DOMAIN_END: f64 = 1.0;
const PHASE_DOMAIN_START: f64 = 0.0;
const PHASE_DOMAIN_END: f64 = 0.43;
const PREVIEW_DIR: &str = "target/rev7_bode_preview";
const FREQUENCY_POINTS: usize = 5_000;
const TRACES_PER_VARIANT: usize = 6;
const DEFAULT_REPORTING_RATE_HZ: f64 = 1_000.0;
const REPORTING_RATE_LEVELS_HZ: [f64; 10] = [
    5.0, 10.0, 20.0, 50.0, 100.0, 200.0, 500.0, 1_000.0, 2_000.0, 5_000.0,
];
const SALLEN_KEY_CAPACITANCE_F: f64 = 10.0e-9;
const SALLEN_KEY_100HZ_RESISTANCE_OHMS: f64 = 100.0e3;
const SALLEN_KEY_1KHZ_RESISTANCE_OHMS: f64 = 10.0e3;
const SALLEN_KEY_3KHZ_RESISTANCE_OHMS: f64 = 3.3e3;

struct FrontendGroup {
    name: &'static str,
    slug: &'static str,
    channel_idx: usize,
    analog_cutoff_hz: f64,
}

struct Theme {
    foreground: &'static str,
    grid: &'static str,
    background: &'static str,
}

struct BodeTraces<'a> {
    analog_frequencies_hz: &'a [f64],
    sampled_frequencies_hz: &'a [f64],
    combined: &'a BodeData<f64>,
    analog: &'a BodeData<f64>,
    digital: &'a BodeData<f64>,
}

struct FrequencyGrid<'a> {
    sampled_frequencies_hz: &'a [f64],
    analog_angular_frequencies: &'a [f64],
    sampled_angular_frequencies: &'a [f64],
}

struct FrontendTransferFunctions<'a> {
    analog: &'a ContinuousTransferFunction<f64>,
    fractional_delay: &'a DiscreteTransferFunction<f64>,
}

struct TraceStyle {
    name: &'static str,
    dash: DashType,
    width: f64,
}

struct PlotVariant {
    reporting_rate_hz: f64,
    combined: BodeData<f64>,
    analog: BodeData<f64>,
    digital: BodeData<f64>,
    title: String,
    shapes: Vec<Shape>,
    annotations: Vec<Annotation>,
}

fn main() -> Result<(), Box<dyn Error>> {
    let (theme, output_dir) = parse_args()?;
    let display_dir = output_dir
        .is_none()
        .then(|| env::current_dir().map(|current_dir| current_dir.join(PREVIEW_DIR)))
        .transpose()?;
    let html_dir = output_dir.as_ref().or(display_dir.as_ref());
    if let Some(html_dir) = html_dir {
        fs::create_dir_all(html_dir)?;
    }

    let sample_rate_hz = ADC_SAMPLE_RATE_HZ;
    let analog_angular_frequencies = logspace(1.0e1, 1.0e6, FREQUENCY_POINTS);
    let analog_frequencies_hz = analog_angular_frequencies
        .iter()
        .map(|angular_frequency| angular_frequency / TAU)
        .collect::<Vec<_>>();
    let sampled_angular_frequencies = analog_angular_frequencies.clone();
    let sampled_frequencies_hz = sampled_angular_frequencies
        .iter()
        .map(|angular_frequency| angular_frequency / TAU)
        .collect::<Vec<_>>();

    let analog_transfer_functions = adc_analog_frontend_transfer_functions()?;
    let fractional_delay_transfer_functions =
        adc_fractional_delay_transfer_functions(sample_rate_hz)?;
    let mut exporter = if output_dir.is_some() {
        Some(
            StaticExporterBuilder::default()
                .webdriver_browser_caps(vec![
                    "--headless=new".to_string(),
                    "--no-sandbox".to_string(),
                    "--disable-dev-shm-usage".to_string(),
                    "--disable-gpu".to_string(),
                    "--disable-gpu-sandbox".to_string(),
                ])
                .build()?,
        )
    } else {
        None
    };
    let default_reporting_rate_idx = reporting_rate_index(DEFAULT_REPORTING_RATE_HZ)?;

    for group in frontend_groups() {
        let variants = build_variants(
            &theme,
            &group,
            sample_rate_hz,
            &FrequencyGrid {
                sampled_frequencies_hz: &sampled_frequencies_hz,
                analog_angular_frequencies: &analog_angular_frequencies,
                sampled_angular_frequencies: &sampled_angular_frequencies,
            },
            &FrontendTransferFunctions {
                analog: &analog_transfer_functions[group.channel_idx],
                fractional_delay: &fractional_delay_transfer_functions[group.channel_idx],
            },
        )?;
        let plot = build_plot(
            &theme,
            &analog_frequencies_hz,
            &sampled_frequencies_hz,
            &variants,
            default_reporting_rate_idx,
        );

        if let Some(output_dir) = &output_dir {
            let html_path = output_dir.join(format!("{}.html", group.slug));
            let svg_path = output_dir.join(format!("{}.svg", group.slug));
            write_interactive_html(
                &plot,
                &variants,
                default_reporting_rate_idx,
                &html_path,
                &theme,
            )?;
            exporter
                .as_mut()
                .expect("exporter exists for save mode")
                .write_image(&plot, &svg_path, ImageFormat::SVG, WIDTH, HEIGHT, 1.0)?;
            println!("wrote {}", html_path.display());
            println!("wrote {}", svg_path.display());
        } else {
            let html_dir = display_dir.as_ref().expect("display directory exists");
            let html_path = html_dir.join(format!("{}.html", group.slug));
            write_interactive_html(
                &plot,
                &variants,
                default_reporting_rate_idx,
                &html_path,
                &theme,
            )?;
            open_in_default_browser(&html_path)?;
            println!("opened {}", html_path.display());
        }
    }

    if let Some(exporter) = &mut exporter {
        exporter.close();
    }
    Ok(())
}

fn parse_args() -> Result<(Theme, Option<PathBuf>), Box<dyn Error>> {
    let args = env::args().collect::<Vec<_>>();
    if !(2..=3).contains(&args.len()) {
        return Err(format!(
            "usage: {} <dark|light> [output-dir]",
            args.first().map(String::as_str).unwrap_or("rev7_bode")
        )
        .into());
    }

    let theme = match args[1].as_str() {
        "dark" => Theme {
            foreground: "#ffffff",
            grid: "rgba(255,255,255,0.22)",
            background: "#1e2129",
        },
        "light" => Theme {
            foreground: "#000000",
            grid: "rgba(0,0,0,0.18)",
            background: "rgba(0,0,0,0)",
        },
        other => {
            return Err(format!("unknown theme {other:?}; expected \"dark\" or \"light\"").into())
        }
    };

    Ok((theme, args.get(2).map(|path| Path::new(path).to_path_buf())))
}

fn open_in_default_browser(path: &Path) -> Result<(), Box<dyn Error>> {
    let status = open_command(path).status()?;
    if !status.success() {
        return Err(format!("failed to open {} with the default browser", path.display()).into());
    }
    Ok(())
}

#[cfg(all(unix, not(target_os = "android"), not(target_os = "macos")))]
fn open_command(path: &Path) -> Command {
    let mut command = Command::new("xdg-open");
    command.arg(path);
    command
}

#[cfg(target_os = "macos")]
fn open_command(path: &Path) -> Command {
    let mut command = Command::new("open");
    command.arg(path);
    command
}

#[cfg(target_os = "windows")]
fn open_command(path: &Path) -> Command {
    let mut command = Command::new("explorer");
    command.arg(path);
    command
}

fn frontend_groups() -> [FrontendGroup; 3] {
    [
        FrontendGroup {
            name: "Deimos DAQ Rev7 100Hz Analog Frontend",
            slug: "rev7_bode_100hz_frontend",
            channel_idx: 2,
            analog_cutoff_hz: sallen_key_cutoff_hz(SALLEN_KEY_100HZ_RESISTANCE_OHMS),
        },
        FrontendGroup {
            name: "Deimos DAQ Rev7 1kHz Analog Frontend",
            slug: "rev7_bode_1khz_frontend",
            channel_idx: 10,
            analog_cutoff_hz: sallen_key_cutoff_hz(SALLEN_KEY_1KHZ_RESISTANCE_OHMS),
        },
        FrontendGroup {
            name: "Deimos DAQ Rev7 3kHz Analog Frontend",
            slug: "rev7_bode_3khz_frontend",
            channel_idx: 3,
            analog_cutoff_hz: sallen_key_cutoff_hz(SALLEN_KEY_3KHZ_RESISTANCE_OHMS),
        },
    ]
}

const fn sallen_key_cutoff_hz(resistance_ohms: f64) -> f64 {
    1.0 / (TAU * resistance_ohms * SALLEN_KEY_CAPACITANCE_F)
}

fn digital_filter_transfer_function(
    cutoff_ratio: f64,
    sample_rate_hz: f64,
) -> Result<DiscreteTransferFunction<f64>, Box<dyn Error>> {
    Ok(design_digital_filter_tf(&DigitalFilterSpec::new(
        ADC_FILTER_ORDER,
        DigitalFilterFamily::Butterworth,
        FilterShape::Lowpass {
            cutoff: cutoff_ratio * sample_rate_hz * TAU,
        },
        sample_rate_hz,
    )?)?)
}

fn build_variants(
    theme: &Theme,
    group: &FrontendGroup,
    sample_rate_hz: f64,
    frequency_grid: &FrequencyGrid<'_>,
    frontend_transfer_functions: &FrontendTransferFunctions<'_>,
) -> Result<Vec<PlotVariant>, Box<dyn Error>> {
    let analog_bode = frontend_transfer_functions
        .analog
        .bode_data(frequency_grid.analog_angular_frequencies)?;
    let mut variants = Vec::with_capacity(REPORTING_RATE_LEVELS_HZ.len());

    for reporting_rate_hz in REPORTING_RATE_LEVELS_HZ {
        let cutoff_ratio = cutoff_ratio_for_reporting_rate(reporting_rate_hz, sample_rate_hz);
        let digital_filter_transfer_function =
            digital_filter_transfer_function(cutoff_ratio, sample_rate_hz)?;
        let digital = frontend_transfer_functions
            .fractional_delay
            .mul(&digital_filter_transfer_function)?;
        let digital_bode = digital.bode_data(frequency_grid.sampled_angular_frequencies)?;
        let combined_bode = adc_sampled_bode_data(
            cutoff_ratio,
            sample_rate_hz,
            frequency_grid.sampled_frequencies_hz,
        )?[group.channel_idx]
            .clone();
        let title = plot_title(group);
        let (shapes, annotations) =
            reference_marks(theme, group, sample_rate_hz, reporting_rate_hz);

        variants.push(PlotVariant {
            reporting_rate_hz,
            combined: combined_bode,
            analog: analog_bode.clone(),
            digital: digital_bode,
            title,
            shapes,
            annotations,
        });
    }

    Ok(variants)
}

fn cutoff_ratio_for_reporting_rate(reporting_rate_hz: f64, sample_rate_hz: f64) -> f64 {
    (reporting_rate_hz / sample_rate_hz).min(ADC_FILTER_MAX_CUTOFF_RATIO)
}

fn reporting_rate_index(reporting_rate_hz: f64) -> Result<usize, Box<dyn Error>> {
    REPORTING_RATE_LEVELS_HZ
        .iter()
        .position(|level| (*level - reporting_rate_hz).abs() < f64::EPSILON)
        .ok_or_else(|| {
            format!("reporting rate {reporting_rate_hz} Hz is not in the slider levels").into()
        })
}

fn plot_title(group: &FrontendGroup) -> String {
    group.name.to_string()
}

fn format_frequency(frequency_hz: f64) -> String {
    if frequency_hz >= 1_000.0 {
        format!("{:.3} kHz", frequency_hz / 1_000.0)
    } else {
        format!("{frequency_hz:.1} Hz")
    }
}

fn build_plot(
    theme: &Theme,
    analog_frequencies_hz: &[f64],
    sampled_frequencies_hz: &[f64],
    variants: &[PlotVariant],
    default_reporting_rate_idx: usize,
) -> Plot {
    let mut plot = Plot::new();
    for (idx, variant) in variants.iter().enumerate() {
        let visible = if idx == default_reporting_rate_idx {
            Visible::True
        } else {
            Visible::False
        };
        let bode_traces = BodeTraces {
            analog_frequencies_hz,
            sampled_frequencies_hz,
            combined: &variant.combined,
            analog: &variant.analog,
            digital: &variant.digital,
        };
        add_variant_traces(&mut plot, theme, &bode_traces, visible);
    }

    let default_variant = &variants[default_reporting_rate_idx];
    plot.set_layout(layout(theme, default_variant));
    plot
}

fn write_interactive_html(
    plot: &Plot,
    variants: &[PlotVariant],
    default_reporting_rate_idx: usize,
    path: &Path,
    theme: &Theme,
) -> Result<(), Box<dyn Error>> {
    let default_variant = variants.get(default_reporting_rate_idx).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "default reporting-rate index is out of bounds",
        )
    })?;
    let states = slider_states(variants)?;
    let states_json = serde_json::to_string(&states)?;
    let plot_id = "rev7-bode-plot";
    let inline_plot = plot.to_inline_html(Some(plot_id));
    let max_idx = variants.len().saturating_sub(1);
    let default_label = format_frequency(default_variant.reporting_rate_hz);
    let foreground = theme.foreground;
    let background = theme.background;

    let html = format!(
        r#"<!doctype html>
<html lang="en">
<head>
    <meta charset="utf-8" />
    <script src="https://cdn.jsdelivr.net/npm/mathjax@3.2.2/es5/tex-svg.js"></script>
    <script src="https://cdn.plot.ly/plotly-3.0.1.min.js"></script>
    <style>
        html, body {{
            margin: 0;
            background: {background};
        }}

        body {{
            color: {foreground};
            font-family: system-ui, -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif;
        }}

        .rev7-shell {{
            box-sizing: border-box;
            display: flex;
            gap: 12px;
            min-height: {height}px;
            padding: 0;
            background: {background};
        }}

        .rev7-slider-panel {{
            box-sizing: border-box;
            display: flex;
            flex: 0 0 84px;
            flex-direction: column;
            align-items: center;
            justify-content: center;
            gap: 12px;
            min-height: {height}px;
            color: {foreground};
            font-size: 12px;
            line-height: 1.2;
        }}

        .rev7-slider-label {{
            font-size: 14px;
            font-weight: 600;
            writing-mode: vertical-rl;
            transform: rotate(180deg);
        }}

        #reporting-rate-slider {{
            width: 28px;
            height: 560px;
            writing-mode: vertical-lr;
            direction: rtl;
            accent-color: {foreground};
        }}

        #reporting-rate-value {{
            min-width: 64px;
            text-align: center;
            font-variant-numeric: tabular-nums;
        }}

        .rev7-plot {{
            flex: 1 1 auto;
            min-width: 0;
            min-height: {height}px;
            height: {height}px;
        }}

        .rev7-plot .plotly-graph-div {{
            height: {height}px !important;
        }}
    </style>
</head>
<body>
    <div class="rev7-shell">
        <div class="rev7-slider-panel">
            <div class="rev7-slider-label">Reporting rate</div>
            <input id="reporting-rate-slider" type="range" orient="vertical"
                   min="0" max="{max_idx}" step="1" value="{default_reporting_rate_idx}" />
            <output id="reporting-rate-value">{default_label}</output>
        </div>
        <div class="rev7-plot">
            {inline_plot}
        </div>
    </div>
    <script type="text/javascript">
        const states = {states_json};
        const plotElement = document.getElementById("{plot_id}");
        const slider = document.getElementById("reporting-rate-slider");
        const valueLabel = document.getElementById("reporting-rate-value");

        function applyReportingRate(index) {{
            const state = states[index];
            Plotly.restyle(plotElement, {{ visible: state.visible }});
            Plotly.relayout(plotElement, {{
                "title.text": state.title,
                shapes: state.shapes,
                annotations: state.annotations,
            }});
            valueLabel.textContent = state.label;
        }}

        slider.addEventListener("input", (event) => {{
            applyReportingRate(Number(event.target.value));
        }});
    </script>
</body>
</html>
"#,
        default_label = default_label,
        default_reporting_rate_idx = default_reporting_rate_idx,
        background = background,
        foreground = foreground,
        height = HEIGHT,
        inline_plot = inline_plot,
        max_idx = max_idx,
        plot_id = plot_id,
        states_json = states_json,
    );

    fs::write(path, html)?;
    Ok(())
}

fn slider_states(variants: &[PlotVariant]) -> Result<Value, Box<dyn Error>> {
    let trace_count = variants.len() * TRACES_PER_VARIANT;
    let mut states = Vec::with_capacity(variants.len());

    for (variant_idx, variant) in variants.iter().enumerate() {
        let mut visible = vec![false; trace_count];
        let start = variant_idx * TRACES_PER_VARIANT;
        for trace_visible in &mut visible[start..start + TRACES_PER_VARIANT] {
            *trace_visible = true;
        }

        states.push(json!({
            "label": format_frequency(variant.reporting_rate_hz),
            "title": variant.title,
            "visible": visible,
            "shapes": serde_json::to_value(&variant.shapes)?,
            "annotations": serde_json::to_value(&variant.annotations)?,
        }));
    }

    Ok(Value::Array(states))
}

fn add_variant_traces(
    plot: &mut Plot,
    theme: &Theme,
    bode_traces: &BodeTraces<'_>,
    visible: Visible,
) {
    add_trace(
        plot,
        bode_traces.sampled_frequencies_hz,
        &bode_traces.combined.magnitude_db,
        TraceStyle {
            name: "Combined",
            dash: DashType::Solid,
            width: 3.0,
        },
        theme.foreground,
        false,
        visible.clone(),
    );
    add_trace(
        plot,
        bode_traces.analog_frequencies_hz,
        &bode_traces.analog.magnitude_db,
        TraceStyle {
            name: "Analog frontend",
            dash: DashType::Dot,
            width: 2.0,
        },
        theme.foreground,
        false,
        visible.clone(),
    );
    add_trace(
        plot,
        bode_traces.sampled_frequencies_hz,
        &bode_traces.digital.magnitude_db,
        TraceStyle {
            name: "Fractional delay +<br>digital Butterworth",
            dash: DashType::Dash,
            width: 2.0,
        },
        theme.foreground,
        false,
        visible.clone(),
    );
    add_trace(
        plot,
        bode_traces.sampled_frequencies_hz,
        &bode_traces.combined.phase_deg,
        TraceStyle {
            name: "Combined",
            dash: DashType::Solid,
            width: 3.0,
        },
        theme.foreground,
        true,
        visible.clone(),
    );
    add_trace(
        plot,
        bode_traces.analog_frequencies_hz,
        &bode_traces.analog.phase_deg,
        TraceStyle {
            name: "Analog frontend",
            dash: DashType::Dot,
            width: 2.0,
        },
        theme.foreground,
        true,
        visible.clone(),
    );
    add_trace(
        plot,
        bode_traces.sampled_frequencies_hz,
        &bode_traces.digital.phase_deg,
        TraceStyle {
            name: "Fractional delay +<br>digital Butterworth",
            dash: DashType::Dash,
            width: 2.0,
        },
        theme.foreground,
        true,
        visible,
    );
}

fn add_trace(
    plot: &mut Plot,
    frequencies_hz: &[f64],
    y: &[f64],
    style: TraceStyle,
    color: &'static str,
    phase_axis: bool,
    visible: Visible,
) {
    let trace = Scatter::new(frequencies_hz.to_vec(), y.to_vec())
        .mode(Mode::Lines)
        .name(style.name)
        .visible(visible)
        .line(Line::new().color(color).dash(style.dash).width(style.width));

    if phase_axis {
        plot.add_trace(trace.x_axis("x2").y_axis("y2").show_legend(false));
    } else {
        plot.add_trace(trace);
    }
}

fn layout(theme: &Theme, variant: &PlotVariant) -> Layout {
    Layout::new()
        .title(Title::with_text(&variant.title))
        .width(WIDTH)
        .height(HEIGHT)
        .font(Font::new().color(theme.foreground))
        .margin(
            Margin::new()
                .left(90)
                .right(40)
                .top(70)
                .bottom(70)
                .auto_expand(true),
        )
        .paper_background_color(theme.background)
        .plot_background_color(theme.background)
        .x_axis(
            axis(theme, "Frequency [Hz]", true)
                .domain(&[0.0, 1.0])
                .anchor("y"),
        )
        .y_axis(
            axis(theme, "Magnitude [dB]", false)
                .domain(&[MAGNITUDE_DOMAIN_START, MAGNITUDE_DOMAIN_END])
                .anchor("x"),
        )
        .x_axis2(
            axis(theme, "Frequency [Hz]", true)
                .domain(&[0.0, 1.0])
                .anchor("y2"),
        )
        .y_axis2(
            axis(theme, "Phase [deg]", false)
                .domain(&[PHASE_DOMAIN_START, PHASE_DOMAIN_END])
                .anchor("x2")
                .range(AxisRange::new(-180.0, 10.0)),
        )
        .shapes(variant.shapes.clone())
        .annotations(variant.annotations.clone())
}

fn reference_marks(
    theme: &Theme,
    group: &FrontendGroup,
    sample_rate_hz: f64,
    reporting_rate_hz: f64,
) -> (Vec<Shape>, Vec<Annotation>) {
    let vertical_marks = [
        (sample_rate_hz, "Samplerate"),
        (sample_rate_hz / 2.0, "Nyquist"),
        (reporting_rate_hz, "Reporting rate & digital cutoff"),
        (group.analog_cutoff_hz, "Analog cutoff"),
    ];

    let mut shapes = Vec::with_capacity(vertical_marks.len() * 2 + 1);
    let mut annotations = Vec::with_capacity(vertical_marks.len() * 2 + 1);
    for (frequency_hz, label) in vertical_marks {
        shapes.push(vertical_line(
            theme,
            frequency_hz,
            "x",
            MAGNITUDE_DOMAIN_START,
            MAGNITUDE_DOMAIN_END,
        ));
        shapes.push(vertical_line(
            theme,
            frequency_hz,
            "x2",
            PHASE_DOMAIN_START,
            PHASE_DOMAIN_END,
        ));
        annotations.push(vertical_annotation(
            theme,
            frequency_hz,
            "x",
            MAGNITUDE_DOMAIN_START,
            Anchor::Bottom,
            label,
        ));
        annotations.push(vertical_annotation(
            theme,
            frequency_hz,
            "x2",
            PHASE_DOMAIN_END,
            Anchor::Top,
            label,
        ));
    }

    shapes.push(horizontal_phase_line(theme, -90.0));
    annotations.push(phase_annotation(theme, -90.0, "-90 deg phase lag"));
    (shapes, annotations)
}

fn vertical_line(
    theme: &Theme,
    frequency_hz: f64,
    x_ref: &str,
    y0_paper: f64,
    y1_paper: f64,
) -> Shape {
    Shape::new()
        .shape_type(ShapeType::Line)
        .x_ref(x_ref)
        .x0(frequency_hz)
        .x1(frequency_hz)
        .y_ref("paper")
        .y0(y0_paper)
        .y1(y1_paper)
        .opacity(0.8)
        .line(
            ShapeLine::new()
                .color(theme.foreground)
                .width(1.0)
                .dash(DashType::Solid),
        )
}

fn horizontal_phase_line(theme: &Theme, phase_deg: f64) -> Shape {
    Shape::new()
        .shape_type(ShapeType::Line)
        .x_ref("paper")
        .x0(0.0)
        .x1(1.0)
        .y_ref("y2")
        .y0(phase_deg)
        .y1(phase_deg)
        .opacity(0.5)
        .line(
            ShapeLine::new()
                .color(theme.foreground)
                .width(1.0)
                .dash(DashType::Solid),
        )
}

fn vertical_annotation(
    theme: &Theme,
    frequency_hz: f64,
    x_ref: &str,
    y_paper: f64,
    y_anchor: Anchor,
    label: &str,
) -> Annotation {
    Annotation::new()
        .text(label)
        .x_ref(x_ref)
        .x(frequency_hz.log10())
        .y_ref("paper")
        .y(y_paper)
        .x_anchor(Anchor::Left)
        .y_anchor(y_anchor)
        .text_angle(-90.0)
        .show_arrow(false)
        .opacity(1.0)
        .font(Font::new().color(theme.foreground).size(13))
}

fn phase_annotation(theme: &Theme, phase_deg: f64, label: &str) -> Annotation {
    Annotation::new()
        .text(label)
        .x_ref("paper")
        .x(0.99)
        .y_ref("y2")
        .y(phase_deg)
        .x_anchor(Anchor::Right)
        .y_anchor(Anchor::Bottom)
        .show_arrow(false)
        .opacity(1.0)
        .font(Font::new().color(theme.foreground).size(13))
}

fn axis(theme: &Theme, title: &str, log_scale: bool) -> Axis {
    let axis = Axis::new()
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
        .tick_font(Font::new().color(theme.foreground));

    if log_scale {
        axis.type_(AxisType::Log)
    } else {
        axis
    }
}

fn logspace(start_hz: f64, stop_hz: f64, count: usize) -> Vec<f64> {
    let start = start_hz.log10();
    let stop = stop_hz.log10();
    let step = (stop - start) / (count - 1) as f64;
    (0..count)
        .map(|idx| 10.0_f64.powf(start + idx as f64 * step))
        .collect()
}
