use crate::demo_signal::step_then_tone_signal;
use crate::plot_helpers::{LineSeries, build_line_plot, logspace};
use crate::plotly_support::use_plotly_chart;
use leptos::prelude::*;
use numerical::control::lti::{Fir, SavGolSpec, design_savgol};
use plotly::Plot;

/// Interactive Savitzky-Golay filter-design page.
#[component]
pub fn SavGolDesignPage() -> impl IntoView {
    let (window_len, set_window_len) = signal(nine_tap_default());
    let (poly_order, set_poly_order) = signal(3_usize);
    let (sample_rate, set_sample_rate) = signal(20.0_f64);

    let inputs = move || SavGolInputs {
        window_len: window_len.get(),
        poly_order: poly_order.get(),
        sample_rate: sample_rate.get(),
    };
    let design = Memo::new(move |_| run_savgol_design(inputs()));

    use_plotly_chart("savgol-design-mag-plot", move || {
        build_savgol_plot(design.get(), SavGolPlotKind::Magnitude)
    });
    use_plotly_chart("savgol-design-phase-plot", move || {
        build_savgol_plot(design.get(), SavGolPlotKind::Phase)
    });
    use_plotly_chart("savgol-design-taps-plot", move || {
        build_savgol_plot(design.get(), SavGolPlotKind::Taps)
    });
    use_plotly_chart("savgol-design-time-plot", move || {
        build_savgol_plot(design.get(), SavGolPlotKind::TimeResponse)
    });

    let design_status = move || savgol_summary(design.get());

    view! {
        <div class="page">
            <header class="page-header">
                <p class="eyebrow">"Filter Design"</p>
                <h1>"Savitzky-Golay"</h1>
                <p>
                    "This page compares a Savitzky-Golay smoothing kernel against a same-window sliding mean,"
                    " using the FIR design and analysis paths directly from `numerical`."
                </p>
            </header>

            <div class="control-layout">
                <aside class="control-card">
                    <section>
                        <h2>"Design controls"</h2>
                        <p class="section-copy">
                            "The Savitzky-Golay design uses a centered least-squares polynomial fit with derivative order zero."
                            " The comparison filter is a same-window constant-tap sliding mean."
                        </p>

                        <div class="control-row">
                            <label for="savgol-window-len">"Window length"</label>
                            <output>{move || window_len.get().to_string()}</output>
                            <input
                                id="savgol-window-len"
                                type="range"
                                min="3"
                                max="201"
                                step="2"
                                prop:value=move || window_len.get().to_string()
                                on:input=move |ev| {
                                    if let Ok(value) = event_target_value(&ev).parse::<usize>() {
                                        let clamped = clamp_window_len(value);
                                        set_window_len.set(clamped);
                                        set_poly_order.update(|poly| {
                                            *poly = (*poly).min(clamped.saturating_sub(1));
                                        });
                                    }
                                }
                            />
                        </div>

                        <div class="control-row">
                            <label for="savgol-poly-order">"Polynomial order"</label>
                            <output>{move || poly_order.get().to_string()}</output>
                            <input
                                id="savgol-poly-order"
                                type="range"
                                min="0"
                                max=move || window_len.get().saturating_sub(1).to_string()
                                step="1"
                                prop:value=move || poly_order.get().to_string()
                                on:input=move |ev| {
                                    if let Ok(value) = event_target_value(&ev).parse::<usize>() {
                                        set_poly_order.set(value.min(window_len.get().saturating_sub(1)));
                                    }
                                }
                            />
                        </div>

                        <div class="control-row">
                            <label for="savgol-sample-rate">"Sample rate"</label>
                            <output>{move || format!("{:.2} samples/s", sample_rate.get())}</output>
                            <input
                                id="savgol-sample-rate"
                                type="range"
                                min="4.0"
                                max="40.0"
                                step="0.5"
                                prop:value=move || sample_rate.get().to_string()
                                on:input=move |ev| {
                                    if let Ok(value) = event_target_value(&ev).parse::<f64>() {
                                        set_sample_rate.set(value.clamp(4.0, 40.0));
                                    }
                                }
                            />
                        </div>
                    </section>

                    <section>
                        <h2>"Design summary"</h2>
                        <p class="section-copy">{design_status}</p>
                    </section>
                </aside>

                <div class="plots-grid wide">
                    <article class="plot-card">
                        <div class="plot-header">
                            <div>
                                <h2>"Frequency response and taps"</h2>
                                <p>
                                    "Magnitude and phase compare the Savitzky-Golay smoother against the same-window"
                                    " sliding mean. The tap plot shows the actual FIR weights."
                                </p>
                            </div>
                        </div>

                        <div class="plot-subsection">
                            <div class="plot-header">
                                <div>
                                    <h2>"Magnitude"</h2>
                                    <p>"Bode magnitude of both FIR smoothers on the same log-spaced frequency grid."</p>
                                </div>
                            </div>
                            <div id="savgol-design-mag-plot" class="plot-surface"></div>
                        </div>

                        <div class="plot-subsection">
                            <div class="plot-header">
                                <div>
                                    <h2>"Phase"</h2>
                                    <p>"Unwrapped phase for the same two linear-phase FIR filters."</p>
                                </div>
                            </div>
                            <div id="savgol-design-phase-plot" class="plot-surface"></div>
                        </div>

                        <div class="plot-subsection">
                            <div class="plot-header">
                                <div>
                                    <h2>"Time-domain response"</h2>
                                    <p>"A short step followed by a single-tone check at the Savitzky-Golay `-3 dB` cutoff."</p>
                                </div>
                            </div>
                            <div id="savgol-design-time-plot" class="plot-surface"></div>
                        </div>

                        <div class="plot-subsection">
                            <div class="plot-header">
                                <div>
                                    <h2>"Tap values"</h2>
                                    <p>"Tap weights versus centered sample offset, shown after the usual Bode plots."</p>
                                </div>
                            </div>
                            <div id="savgol-design-taps-plot" class="plot-surface"></div>
                        </div>
                    </article>
                </div>
            </div>
        </div>
    }
}

#[derive(Clone, Copy)]
struct SavGolInputs {
    window_len: usize,
    poly_order: usize,
    sample_rate: f64,
}

#[derive(Clone, PartialEq)]
struct SavGolData {
    window_len: usize,
    poly_order: usize,
    sample_rate: f64,
    sample_time: f64,
    group_delay_samples: f64,
    sg_dc_gain: f64,
    mean_dc_gain: f64,
    frequency_over_fs: Vec<f64>,
    sg_magnitude_db: Vec<f64>,
    mean_magnitude_db: Vec<f64>,
    sg_phase_deg: Vec<f64>,
    mean_phase_deg: Vec<f64>,
    effective_cutoff_over_fs: f64,
    simulation_times: Vec<f64>,
    simulation_input: Vec<f64>,
    sg_simulation_output: Vec<f64>,
    mean_simulation_output: Vec<f64>,
    tap_offsets: Vec<f64>,
    sg_taps: Vec<f64>,
    mean_taps: Vec<f64>,
}

#[derive(Clone, Copy)]
enum SavGolPlotKind {
    Magnitude,
    Phase,
    TimeResponse,
    Taps,
}

fn build_savgol_plot(result: Result<SavGolData, String>, kind: SavGolPlotKind) -> Plot {
    match result {
        Ok(data) => match kind {
            SavGolPlotKind::Magnitude => build_line_plot(
                "Savitzky-Golay vs sliding mean magnitude",
                "frequency / fs",
                "magnitude (dB)",
                true,
                vec![
                    LineSeries::lines(
                        "Savitzky-Golay",
                        data.frequency_over_fs.clone(),
                        data.sg_magnitude_db,
                    ),
                    LineSeries::lines(
                        "Sliding mean",
                        data.frequency_over_fs,
                        data.mean_magnitude_db,
                    )
                    .with_dash(plotly::common::DashType::Dash),
                ],
            ),
            SavGolPlotKind::Phase => build_line_plot(
                "Savitzky-Golay vs sliding mean phase",
                "frequency / fs",
                "phase (deg)",
                true,
                vec![
                    LineSeries::lines(
                        "Savitzky-Golay",
                        data.frequency_over_fs.clone(),
                        data.sg_phase_deg,
                    ),
                    LineSeries::lines("Sliding mean", data.frequency_over_fs, data.mean_phase_deg)
                        .with_dash(plotly::common::DashType::Dash),
                ],
            ),
            SavGolPlotKind::TimeResponse => build_line_plot(
                "Step plus tone response",
                "time (s)",
                "signal",
                false,
                vec![
                    LineSeries::lines(
                        "input",
                        data.simulation_times.clone(),
                        data.simulation_input,
                    ),
                    LineSeries::lines(
                        "Savitzky-Golay",
                        data.simulation_times.clone(),
                        data.sg_simulation_output,
                    ),
                    LineSeries::lines(
                        "Sliding mean",
                        data.simulation_times,
                        data.mean_simulation_output,
                    )
                    .with_dash(plotly::common::DashType::Dash),
                ],
            ),
            SavGolPlotKind::Taps => build_line_plot(
                "Tap values",
                "sample offset",
                "tap value",
                false,
                vec![
                    LineSeries::lines_markers(
                        "Savitzky-Golay",
                        data.tap_offsets.clone(),
                        data.sg_taps,
                    )
                    .with_marker_size(8),
                    LineSeries::lines_markers("Sliding mean", data.tap_offsets, data.mean_taps)
                        .with_dash(plotly::common::DashType::Dash)
                        .with_marker_size(8),
                ],
            ),
        },
        Err(message) => build_line_plot(&message, "", "", false, Vec::new()),
    }
}

fn run_savgol_design(inputs: SavGolInputs) -> Result<SavGolData, String> {
    let window_len = clamp_window_len(inputs.window_len);
    let poly_order = inputs.poly_order.min(window_len.saturating_sub(1));
    let sample_rate = inputs.sample_rate.clamp(4.0, 40.0);
    let sample_time = 1.0 / sample_rate;

    let sg_spec =
        SavGolSpec::new(window_len, poly_order, 0, sample_time).map_err(|err| err.to_string())?;
    let savgol = design_savgol(&sg_spec).map_err(|err| err.to_string())?;
    let mean = Fir::new(vec![1.0 / (window_len as f64); window_len], sample_time)
        .map_err(|err| err.to_string())?;

    let angular_frequencies = logspace(
        (1.0e-6 * sample_rate * core::f64::consts::TAU).log10(),
        (0.49 * sample_rate * core::f64::consts::TAU).log10(),
        520,
    );
    let savgol_bode = savgol
        .bode_data(&angular_frequencies)
        .map_err(|err| err.to_string())?;
    let mean_bode = mean
        .bode_data(&angular_frequencies)
        .map_err(|err| err.to_string())?;
    let frequency_over_fs = savgol_bode
        .angular_frequencies
        .iter()
        .map(|omega| *omega / (sample_rate * core::f64::consts::TAU))
        .collect::<Vec<_>>();
    let effective_cutoff_over_fs = approximate_cutoff_over_fs(
        &savgol_bode.angular_frequencies,
        &savgol_bode.magnitude_db,
        sample_rate,
    );
    let (simulation_times, simulation_input) =
        step_then_tone_signal(sample_rate, effective_cutoff_over_fs);
    let sg_simulation_output = savgol
        .filter_forward(&simulation_input)
        .map_err(|err| err.to_string())?
        .output;
    let mean_simulation_output = mean
        .filter_forward(&simulation_input)
        .map_err(|err| err.to_string())?
        .output;

    let half = (window_len / 2) as isize;
    let tap_offsets = (-half..=half)
        .map(|offset| offset as f64)
        .collect::<Vec<_>>();

    Ok(SavGolData {
        window_len,
        poly_order,
        sample_rate,
        sample_time,
        group_delay_samples: savgol.group_delay_samples(1.0e-12).unwrap_or(0.0),
        sg_dc_gain: savgol.dc_gain(),
        mean_dc_gain: mean.dc_gain(),
        frequency_over_fs,
        sg_magnitude_db: savgol_bode.magnitude_db,
        mean_magnitude_db: mean_bode.magnitude_db,
        sg_phase_deg: savgol_bode.phase_deg,
        mean_phase_deg: mean_bode.phase_deg,
        effective_cutoff_over_fs,
        simulation_times,
        simulation_input,
        sg_simulation_output,
        mean_simulation_output,
        tap_offsets,
        sg_taps: savgol.taps().to_vec(),
        mean_taps: mean.taps().to_vec(),
    })
}

fn savgol_summary(result: Result<SavGolData, String>) -> String {
    match result {
        Ok(data) => format!(
            "Window {} with polynomial order {} at fs {:.2} (dt {:.4}) gives both filters a group delay of {:.1} samples. DC gain is {:.3} for Savitzky-Golay and {:.3} for the sliding mean. The Savitzky-Golay `-3 dB` point is approximately {:.3e} fs and sets the single-tone simulation frequency.",
            data.window_len,
            data.poly_order,
            data.sample_rate,
            data.sample_time,
            data.group_delay_samples,
            data.sg_dc_gain,
            data.mean_dc_gain,
            data.effective_cutoff_over_fs,
        ),
        Err(message) => format!("Design failed: {message}"),
    }
}

fn clamp_window_len(value: usize) -> usize {
    value.clamp(3, 201) | 1
}

const fn nine_tap_default() -> usize {
    9
}

fn approximate_cutoff_over_fs(
    angular_frequencies: &[f64],
    magnitude_db: &[f64],
    sample_rate: f64,
) -> f64 {
    angular_frequencies
        .iter()
        .zip(magnitude_db.iter())
        .find_map(|(&omega, &magnitude_db)| {
            (magnitude_db <= -3.0).then_some(omega / (sample_rate * core::f64::consts::TAU))
        })
        .unwrap_or(0.25)
        .clamp(1.0e-6, 0.49)
}

#[cfg(test)]
mod tests {
    use super::{SavGolInputs, run_savgol_design};

    #[test]
    fn savgol_design_demo_runs() {
        let data = run_savgol_design(SavGolInputs {
            window_len: 9,
            poly_order: 3,
            sample_rate: 20.0,
        })
        .unwrap();

        assert_eq!(data.tap_offsets.len(), 9);
        assert_eq!(data.sg_taps.len(), 9);
        assert_eq!(data.mean_taps.len(), 9);
        assert!(!data.sg_magnitude_db.is_empty());
        assert!(!data.mean_phase_deg.is_empty());
        assert_eq!(data.simulation_input.len(), data.sg_simulation_output.len());
        assert_eq!(
            data.simulation_input.len(),
            data.mean_simulation_output.len()
        );
    }
}
