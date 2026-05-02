use crate::plotly_support::use_plotly_chart;
use leptos::prelude::*;
use numerical::control::lti::FopdtModel;
use plotly::{
    Layout, Plot, Scatter,
    common::{Line, Mode, Title},
    layout::{Axis, AxisType},
};

/// Interactive process-model page that exercises the LTI process-model helpers.
#[component]
pub fn LtiProcessModelsPage() -> impl IntoView {
    let (gain, set_gain) = signal(1.4_f64);
    let (time_constant, set_time_constant) = signal(3.5_f64);
    let (delay, set_delay) = signal(0.8_f64);

    use_plotly_chart("fopdt-step-plot", move || {
        let model = current_model(gain.get(), time_constant.get(), delay.get());
        build_step_plot(&model)
    });
    use_plotly_chart("fopdt-bode-mag-plot", move || {
        let model = current_model(gain.get(), time_constant.get(), delay.get());
        build_bode_plot(&model, BodeKind::Magnitude)
    });
    use_plotly_chart("fopdt-bode-phase-plot", move || {
        let model = current_model(gain.get(), time_constant.get(), delay.get());
        build_bode_plot(&model, BodeKind::Phase)
    });

    let reset = move |_| {
        set_gain.set(1.4);
        set_time_constant.set(3.5);
        set_delay.set(0.8);
    };

    view! {
        <div class="page">
            <header class="page-header">
                <p class="eyebrow">"LTI Analysis"</p>
                <h1>"Delayed Process Model Explorer"</h1>
                <p>
                    "This page exercises the explicit-delay process-model API directly in the browser."
                    " Adjust the gain, lag, and delay and the step and Bode plots are regenerated from "
                    "`numerical::control::lti::FopdtModel`."
                </p>
            </header>

            <div class="control-layout">
                <aside class="control-card">
                    <section>
                        <h2>"Model parameters"</h2>
                        <p class="section-copy">
                            "These map directly onto `FOPDT: K exp(-Ls) / (tau s + 1)`."
                        </p>

                        <div class="control-row">
                            <label for="fopdt-gain">"Gain"</label>
                            <output>{move || format!("{:.3}", gain.get())}</output>
                            <input
                                id="fopdt-gain"
                                type="range"
                                min="0.2"
                                max="4.0"
                                step="0.05"
                                prop:value=move || gain.get().to_string()
                                on:input=move |ev| {
                                    if let Ok(value) = event_target_value(&ev).parse::<f64>() {
                                        set_gain.set(value);
                                    }
                                }
                            />
                        </div>

                        <div class="control-row">
                            <label for="fopdt-tau">"Time constant"</label>
                            <output>{move || format!("{:.3} s", time_constant.get())}</output>
                            <input
                                id="fopdt-tau"
                                type="range"
                                min="0.3"
                                max="12.0"
                                step="0.1"
                                prop:value=move || time_constant.get().to_string()
                                on:input=move |ev| {
                                    if let Ok(value) = event_target_value(&ev).parse::<f64>() {
                                        set_time_constant.set(value);
                                    }
                                }
                            />
                        </div>

                        <div class="control-row">
                            <label for="fopdt-delay">"Delay"</label>
                            <output>{move || format!("{:.3} s", delay.get())}</output>
                            <input
                                id="fopdt-delay"
                                type="range"
                                min="0.0"
                                max="4.0"
                                step="0.05"
                                prop:value=move || delay.get().to_string()
                                on:input=move |ev| {
                                    if let Ok(value) = event_target_value(&ev).parse::<f64>() {
                                        set_delay.set(value);
                                    }
                                }
                            />
                        </div>
                    </section>

                    <section>
                        <h2>"Quick readout"</h2>
                        <div class="metric-grid">
                            <div class="metric">
                                <div class="metric-label">"DC gain"</div>
                                <div class="metric-value">{move || format!("{:.3}", gain.get())}</div>
                            </div>
                            <div class="metric">
                                <div class="metric-label">"Dominant lag"</div>
                                <div class="metric-value">
                                    {move || format!("{:.2} s", time_constant.get())}
                                </div>
                            </div>
                            <div class="metric">
                                <div class="metric-label">"Dead time"</div>
                                <div class="metric-value">{move || format!("{:.2} s", delay.get())}</div>
                            </div>
                        </div>
                    </section>

                    <section>
                        <button class="action-button" on:click=reset>
                            "Reset to reference values"
                        </button>
                    </section>
                </aside>

                <div class="plots-grid">
                    <article class="plot-card">
                        <div class="plot-header">
                            <div>
                                <h2>"Process-model responses"</h2>
                                <p>"Step and frequency-domain views of the same delayed first-order model."</p>
                            </div>
                        </div>
                        <div class="plot-subsection">
                            <div class="plot-header">
                                <div>
                                    <h2>"Step response"</h2>
                                    <p>"Absolute-time delayed step sampled directly from `FopdtModel`."</p>
                                </div>
                            </div>
                            <div id="fopdt-step-plot" class="plot-surface"></div>
                        </div>

                        <div class="plot-subsection">
                            <div class="plot-header">
                                <div>
                                    <h2>"Bode magnitude"</h2>
                                    <p>"Continuous-time response on a logarithmic angular-frequency grid."</p>
                                </div>
                            </div>
                            <div id="fopdt-bode-mag-plot" class="plot-surface"></div>
                        </div>

                        <div class="plot-subsection">
                            <div class="plot-header">
                                <div>
                                    <h2>"Bode phase"</h2>
                                    <p>"Unwrapped phase returned by the library plotting helpers."</p>
                                </div>
                            </div>
                            <div id="fopdt-bode-phase-plot" class="plot-surface"></div>
                        </div>
                    </article>
                </div>
            </div>
        </div>
    }
}

#[derive(Clone, Copy)]
enum BodeKind {
    Magnitude,
    Phase,
}

fn current_model(gain: f64, time_constant: f64, delay: f64) -> FopdtModel<f64> {
    FopdtModel {
        gain,
        time_constant,
        delay,
    }
}

fn build_step_plot(model: &FopdtModel<f64>) -> Plot {
    let sample_times = linspace(0.0, 24.0, 320);
    let values = model
        .step_response_values(&sample_times, 0.0, 1.0, 0.0)
        .unwrap_or_else(|_| vec![0.0; sample_times.len()]);
    let trace = Scatter::new(sample_times, values)
        .mode(Mode::Lines)
        .line(Line::new().color("#000000"))
        .name("step");

    let layout = Layout::new()
        .title(Title::with_text("FOPDT step response"))
        .x_axis(Axis::new().title(Title::with_text("time")))
        .y_axis(Axis::new().title(Title::with_text("output")));

    let mut plot = Plot::new();
    plot.add_trace(trace);
    plot.set_layout(layout);
    plot
}

fn build_bode_plot(model: &FopdtModel<f64>, kind: BodeKind) -> Plot {
    let angular_frequencies = logspace(-2.0, 1.7, 240);
    let bode = model.bode_data(&angular_frequencies).unwrap();
    let (title, axis_label, values) = match kind {
        BodeKind::Magnitude => ("FOPDT Bode magnitude", "magnitude (dB)", bode.magnitude_db),
        BodeKind::Phase => ("FOPDT Bode phase", "phase (deg)", bode.phase_deg),
    };

    let trace = Scatter::new(angular_frequencies, values)
        .mode(Mode::Lines)
        .line(Line::new().color("#000000"))
        .name(title);

    let layout = Layout::new()
        .title(Title::with_text(title))
        .x_axis(
            Axis::new()
                .type_(AxisType::Log)
                .title(Title::with_text("angular frequency")),
        )
        .y_axis(Axis::new().title(Title::with_text(axis_label)));

    let mut plot = Plot::new();
    plot.add_trace(trace);
    plot.set_layout(layout);
    plot
}

fn linspace(start: f64, stop: f64, n: usize) -> Vec<f64> {
    if n <= 1 {
        return vec![start];
    }
    let step = (stop - start) / ((n - 1) as f64);
    (0..n).map(|index| start + (index as f64) * step).collect()
}

fn logspace(log10_start: f64, log10_stop: f64, n: usize) -> Vec<f64> {
    linspace(log10_start, log10_stop, n)
        .into_iter()
        .map(|value| 10.0_f64.powf(value))
        .collect()
}
