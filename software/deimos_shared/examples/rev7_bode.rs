use std::{
    env,
    error::Error,
    f64::consts::TAU,
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use deimos_numerics::control::lti::{
    design_digital_filter_tf, BodeData, DigitalFilterFamily, DigitalFilterSpec,
    DiscreteTransferFunction, FilterShape,
};
use deimos_shared::peripherals::deimos_daq_rev7::{
    adc_analog_frontend_transfer_functions, adc_fractional_delay_transfer_functions,
    adc_sampled_bode_data, ADC_FILTER_MAX_CUTOFF_RATIO, ADC_FILTER_ORDER, ADC_SAMPLE_RATE_HZ,
};
use plotly::{
    common::{DashType, Font, Line, Mode, Title},
    layout::{Axis, AxisType, Layout},
    plotly_static::StaticExporterBuilder,
    prelude::ExporterSyncExt,
    ImageFormat, Plot, Scatter,
};

const CUTOFF_RATIO: f64 = 0.1;
const WIDTH: usize = 1200;
const HEIGHT: usize = 850;
const PREVIEW_DIR: &str = "target/rev7_bode_preview";
const FREQUENCY_POINTS: usize = 5_000;

struct FrontendGroup {
    name: &'static str,
    slug: &'static str,
    channel_idx: usize,
}

struct Theme {
    foreground: &'static str,
    grid: &'static str,
}

struct BodeTraces<'a> {
    analog_frequencies_hz: &'a [f64],
    sampled_frequencies_hz: &'a [f64],
    combined: &'a BodeData<f64>,
    analog: &'a BodeData<f64>,
    digital: &'a BodeData<f64>,
}

struct TraceStyle {
    name: &'static str,
    dash: DashType,
    width: f64,
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
    let cutoff_ratio = CUTOFF_RATIO.min(ADC_FILTER_MAX_CUTOFF_RATIO);
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
    let digital_filter_transfer_function =
        digital_filter_transfer_function(cutoff_ratio, sample_rate_hz)?;
    let combined_bode_data =
        adc_sampled_bode_data(cutoff_ratio, sample_rate_hz, &sampled_frequencies_hz)?;
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

    for group in frontend_groups() {
        let digital = fractional_delay_transfer_functions[group.channel_idx]
            .mul(&digital_filter_transfer_function)?;
        let analog_bode =
            analog_transfer_functions[group.channel_idx].bode_data(&analog_angular_frequencies)?;
        let digital_bode = digital.bode_data(&sampled_angular_frequencies)?;
        let combined_bode = &combined_bode_data[group.channel_idx];

        let bode_traces = BodeTraces {
            analog_frequencies_hz: &analog_frequencies_hz,
            sampled_frequencies_hz: &sampled_frequencies_hz,
            combined: combined_bode,
            analog: &analog_bode,
            digital: &digital_bode,
        };
        let plot = build_plot(&theme, &group, sample_rate_hz, cutoff_ratio, &bode_traces);

        if let Some(output_dir) = &output_dir {
            let html_path = output_dir.join(format!("{}.html", group.slug));
            let svg_path = output_dir.join(format!("{}.svg", group.slug));
            plot.write_html(&html_path);
            exporter
                .as_mut()
                .expect("exporter exists for save mode")
                .write_image(&plot, &svg_path, ImageFormat::SVG, WIDTH, HEIGHT, 1.0)?;
            println!("wrote {}", html_path.display());
            println!("wrote {}", svg_path.display());
        } else {
            let html_dir = display_dir.as_ref().expect("display directory exists");
            let html_path = html_dir.join(format!("{}.html", group.slug));
            plot.write_html(&html_path);
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
        },
        "light" => Theme {
            foreground: "#000000",
            grid: "rgba(0,0,0,0.18)",
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
            name: "Rev7 100 Hz frontend, board temperature representative",
            slug: "rev7_bode_100hz_frontend",
            channel_idx: 2,
        },
        FrontendGroup {
            name: "Rev7 1 kHz frontend, thermocouple / 25.7x representative",
            slug: "rev7_bode_1khz_frontend",
            channel_idx: 10,
        },
        FrontendGroup {
            name: "Rev7 3 kHz frontend, analog-input representative",
            slug: "rev7_bode_3khz_frontend",
            channel_idx: 3,
        },
    ]
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

fn build_plot(
    theme: &Theme,
    group: &FrontendGroup,
    sample_rate_hz: f64,
    cutoff_ratio: f64,
    bode_traces: &BodeTraces<'_>,
) -> Plot {
    let mut plot = Plot::new();

    add_trace(
        &mut plot,
        bode_traces.sampled_frequencies_hz,
        &bode_traces.combined.magnitude_db,
        TraceStyle {
            name: "Combined",
            dash: DashType::Solid,
            width: 3.0,
        },
        theme.foreground,
        false,
    );
    add_trace(
        &mut plot,
        bode_traces.analog_frequencies_hz,
        &bode_traces.analog.magnitude_db,
        TraceStyle {
            name: "Analog frontend",
            dash: DashType::Dot,
            width: 2.0,
        },
        theme.foreground,
        false,
    );
    add_trace(
        &mut plot,
        bode_traces.sampled_frequencies_hz,
        &bode_traces.digital.magnitude_db,
        TraceStyle {
            name: "Fractional delay + digital Butterworth",
            dash: DashType::Dash,
            width: 2.0,
        },
        theme.foreground,
        false,
    );
    add_trace(
        &mut plot,
        bode_traces.sampled_frequencies_hz,
        &bode_traces.combined.phase_deg,
        TraceStyle {
            name: "Combined",
            dash: DashType::Solid,
            width: 3.0,
        },
        theme.foreground,
        true,
    );
    add_trace(
        &mut plot,
        bode_traces.analog_frequencies_hz,
        &bode_traces.analog.phase_deg,
        TraceStyle {
            name: "Analog frontend",
            dash: DashType::Dot,
            width: 2.0,
        },
        theme.foreground,
        true,
    );
    add_trace(
        &mut plot,
        bode_traces.sampled_frequencies_hz,
        &bode_traces.digital.phase_deg,
        TraceStyle {
            name: "Fractional delay + digital Butterworth",
            dash: DashType::Dash,
            width: 2.0,
        },
        theme.foreground,
        true,
    );

    plot.set_layout(layout(
        theme,
        &format!(
            "{} (fs = {:.0} Hz, cutoff ratio = {:.3})",
            group.name, sample_rate_hz, cutoff_ratio
        ),
    ));
    plot
}

fn add_trace(
    plot: &mut Plot,
    frequencies_hz: &[f64],
    y: &[f64],
    style: TraceStyle,
    color: &'static str,
    phase_axis: bool,
) {
    let trace = Scatter::new(frequencies_hz.to_vec(), y.to_vec())
        .mode(Mode::Lines)
        .name(style.name)
        .line(Line::new().color(color).dash(style.dash).width(style.width));

    if phase_axis {
        plot.add_trace(trace.x_axis("x2").y_axis("y2").show_legend(false));
    } else {
        plot.add_trace(trace);
    }
}

fn layout(theme: &Theme, title: &str) -> Layout {
    Layout::new()
        .title(Title::with_text(title))
        .width(WIDTH)
        .height(HEIGHT)
        .font(Font::new().color(theme.foreground))
        .paper_background_color("rgba(0,0,0,0)")
        .plot_background_color("rgba(0,0,0,0)")
        .x_axis(axis(theme, "Frequency [Hz]", true).domain(&[0.0, 1.0]))
        .y_axis(axis(theme, "Magnitude [dB]", false).domain(&[0.57, 1.0]))
        .x_axis2(axis(theme, "Frequency [Hz]", true).domain(&[0.0, 1.0]))
        .y_axis2(axis(theme, "Phase [deg]", false).domain(&[0.0, 0.43]))
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
