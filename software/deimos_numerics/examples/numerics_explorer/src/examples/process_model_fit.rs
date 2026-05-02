use crate::demo_signal::gaussianish_signal;
use crate::plot_helpers::{LineSeries, build_line_plot, linspace, logspace};
use crate::plotly_support::use_plotly_chart;
use leptos::prelude::*;
use numerical::control::lti::{ContinuousTransferFunction, FopdtModel, SopdtModel};
use numerical::control::synthesis::{
    ProcessModelFitOptions, StepResponseData, fit_fopdt_from_step_response_with_options,
    fit_sopdt_from_step_response_with_options,
};
use plotly::Plot;
use plotly::common::{DashType, MarkerSymbol};

/// Interactive process-model fitting page comparing FOPDT and SOPDT fits on
/// matched and unmatched step-response data.
#[component]
pub fn ProcessModelFitPage() -> impl IntoView {
    let (source_kind, set_source_kind) = signal(FitSourceKind::Fopdt);
    let (noise_level, set_noise_level) = signal(0.02_f64);
    let (fit_tolerance, set_fit_tolerance) = signal(1.0e-3_f64);

    let (fopdt_gain, set_fopdt_gain) = signal(1.35_f64);
    let (fopdt_time_constant, set_fopdt_time_constant) = signal(3.0_f64);
    let (fopdt_delay, set_fopdt_delay) = signal(0.8_f64);

    let (sopdt_gain, set_sopdt_gain) = signal(1.10_f64);
    let (sopdt_time_constant_1, set_sopdt_time_constant_1) = signal(2.6_f64);
    let (sopdt_time_constant_2, set_sopdt_time_constant_2) = signal(0.85_f64);
    let (sopdt_delay, set_sopdt_delay) = signal(0.45_f64);

    let (higher_order_gain, set_higher_order_gain) = signal(1.30_f64);
    let (higher_order_zero_time_constant, set_higher_order_zero_time_constant) = signal(0.45_f64);
    let (higher_order_natural_frequency, set_higher_order_natural_frequency) = signal(1.20_f64);
    let (higher_order_damping_ratio, set_higher_order_damping_ratio) = signal(0.28_f64);
    let (higher_order_tail_lag, set_higher_order_tail_lag) = signal(1.80_f64);

    let inputs = move || ProcessFitInputs {
        source_kind: source_kind.get(),
        noise_level: noise_level.get(),
        fit_tolerance: fit_tolerance.get(),
        fopdt_gain: fopdt_gain.get(),
        fopdt_time_constant: fopdt_time_constant.get(),
        fopdt_delay: fopdt_delay.get(),
        sopdt_gain: sopdt_gain.get(),
        sopdt_time_constant_1: sopdt_time_constant_1.get(),
        sopdt_time_constant_2: sopdt_time_constant_2.get(),
        sopdt_delay: sopdt_delay.get(),
        higher_order_gain: higher_order_gain.get(),
        higher_order_zero_time_constant: higher_order_zero_time_constant.get(),
        higher_order_natural_frequency: higher_order_natural_frequency.get(),
        higher_order_damping_ratio: higher_order_damping_ratio.get(),
        higher_order_tail_lag: higher_order_tail_lag.get(),
    };

    let demo = Memo::new(move |_| {
        let inputs = inputs();
        run_process_fit_demo(
            inputs,
            ProcessModelFitOptions {
                tolerance: Some(inputs.fit_tolerance),
                patience: Some(6),
            },
        )
    });

    use_plotly_chart("process-fit-response-plot", move || {
        build_process_fit_plot(demo.get(), ProcessFitPlot::Response)
    });
    use_plotly_chart("process-fit-residual-plot", move || {
        build_process_fit_plot(demo.get(), ProcessFitPlot::Residual)
    });
    use_plotly_chart("process-fit-bode-mag-plot", move || {
        build_process_fit_plot(demo.get(), ProcessFitPlot::BodeMagnitude)
    });
    use_plotly_chart("process-fit-bode-phase-plot", move || {
        build_process_fit_plot(demo.get(), ProcessFitPlot::BodePhase)
    });

    let summary = move || process_fit_summary(demo.get());

    let source_controls = move || match source_kind.get() {
        FitSourceKind::Fopdt => view! {
            <>
                <div class="control-row">
                    <label for="process-fit-fopdt-gain">"Gain"</label>
                    <output>{move || format!("{:.2}", fopdt_gain.get())}</output>
                    <input
                        id="process-fit-fopdt-gain"
                        type="range"
                        min="0.4"
                        max="3.0"
                        step="0.05"
                        prop:value=move || fopdt_gain.get().to_string()
                        on:input=move |ev| {
                            if let Ok(value) = event_target_value(&ev).parse::<f64>() {
                                set_fopdt_gain.set(value.max(0.4));
                            }
                        }
                    />
                </div>

                <div class="control-row">
                    <label for="process-fit-fopdt-tau">"Time constant"</label>
                    <output>{move || format!("{:.2} s", fopdt_time_constant.get())}</output>
                    <input
                        id="process-fit-fopdt-tau"
                        type="range"
                        min="0.5"
                        max="10.0"
                        step="0.1"
                        prop:value=move || fopdt_time_constant.get().to_string()
                        on:input=move |ev| {
                            if let Ok(value) = event_target_value(&ev).parse::<f64>() {
                                set_fopdt_time_constant.set(value.max(0.5));
                            }
                        }
                    />
                </div>

                <div class="control-row">
                    <label for="process-fit-fopdt-delay">"Delay"</label>
                    <output>{move || format!("{:.2} s", fopdt_delay.get())}</output>
                    <input
                        id="process-fit-fopdt-delay"
                        type="range"
                        min="0.0"
                        max="3.0"
                        step="0.05"
                        prop:value=move || fopdt_delay.get().to_string()
                        on:input=move |ev| {
                            if let Ok(value) = event_target_value(&ev).parse::<f64>() {
                                set_fopdt_delay.set(value.max(0.0));
                            }
                        }
                    />
                </div>
            </>
        }
        .into_any(),
        FitSourceKind::Sopdt => view! {
            <>
                <div class="control-row">
                    <label for="process-fit-sopdt-gain">"Gain"</label>
                    <output>{move || format!("{:.2}", sopdt_gain.get())}</output>
                    <input
                        id="process-fit-sopdt-gain"
                        type="range"
                        min="0.4"
                        max="3.0"
                        step="0.05"
                        prop:value=move || sopdt_gain.get().to_string()
                        on:input=move |ev| {
                            if let Ok(value) = event_target_value(&ev).parse::<f64>() {
                                set_sopdt_gain.set(value.max(0.4));
                            }
                        }
                    />
                </div>

                <div class="control-row">
                    <label for="process-fit-sopdt-tau1">"Slow lag"</label>
                    <output>{move || format!("{:.2} s", sopdt_time_constant_1.get())}</output>
                    <input
                        id="process-fit-sopdt-tau1"
                        type="range"
                        min="0.6"
                        max="8.0"
                        step="0.1"
                        prop:value=move || sopdt_time_constant_1.get().to_string()
                        on:input=move |ev| {
                            if let Ok(value) = event_target_value(&ev).parse::<f64>() {
                                set_sopdt_time_constant_1.set(value.max(0.6));
                            }
                        }
                    />
                </div>

                <div class="control-row">
                    <label for="process-fit-sopdt-tau2">"Fast lag"</label>
                    <output>{move || format!("{:.2} s", sopdt_time_constant_2.get())}</output>
                    <input
                        id="process-fit-sopdt-tau2"
                        type="range"
                        min="0.2"
                        max="3.0"
                        step="0.05"
                        prop:value=move || sopdt_time_constant_2.get().to_string()
                        on:input=move |ev| {
                            if let Ok(value) = event_target_value(&ev).parse::<f64>() {
                                set_sopdt_time_constant_2.set(value.max(0.2));
                            }
                        }
                    />
                </div>

                <div class="control-row">
                    <label for="process-fit-sopdt-delay">"Delay"</label>
                    <output>{move || format!("{:.2} s", sopdt_delay.get())}</output>
                    <input
                        id="process-fit-sopdt-delay"
                        type="range"
                        min="0.0"
                        max="2.5"
                        step="0.05"
                        prop:value=move || sopdt_delay.get().to_string()
                        on:input=move |ev| {
                            if let Ok(value) = event_target_value(&ev).parse::<f64>() {
                                set_sopdt_delay.set(value.max(0.0));
                            }
                        }
                    />
                </div>
            </>
        }
        .into_any(),
        FitSourceKind::HigherOrder => view! {
            <>
                <div class="control-row">
                    <label for="process-fit-higher-gain">"Gain"</label>
                    <output>{move || format!("{:.2}", higher_order_gain.get())}</output>
                    <input
                        id="process-fit-higher-gain"
                        type="range"
                        min="0.5"
                        max="2.8"
                        step="0.05"
                        prop:value=move || higher_order_gain.get().to_string()
                        on:input=move |ev| {
                            if let Ok(value) = event_target_value(&ev).parse::<f64>() {
                                set_higher_order_gain.set(value.max(0.5));
                            }
                        }
                    />
                </div>

                <div class="control-row">
                    <label for="process-fit-higher-zero">"Zero time constant"</label>
                    <output>
                        {move || format!("{:.2} s", higher_order_zero_time_constant.get())}
                    </output>
                    <input
                        id="process-fit-higher-zero"
                        type="range"
                        min="0.0"
                        max="1.2"
                        step="0.05"
                        prop:value=move || higher_order_zero_time_constant.get().to_string()
                        on:input=move |ev| {
                            if let Ok(value) = event_target_value(&ev).parse::<f64>() {
                                set_higher_order_zero_time_constant.set(value.max(0.0));
                            }
                        }
                    />
                </div>

                <div class="control-row">
                    <label for="process-fit-higher-wn">"Natural frequency"</label>
                    <output>
                        {move || format!("{:.2} rad/s", higher_order_natural_frequency.get())}
                    </output>
                    <input
                        id="process-fit-higher-wn"
                        type="range"
                        min="0.4"
                        max="2.2"
                        step="0.05"
                        prop:value=move || higher_order_natural_frequency.get().to_string()
                        on:input=move |ev| {
                            if let Ok(value) = event_target_value(&ev).parse::<f64>() {
                                set_higher_order_natural_frequency.set(value.max(0.4));
                            }
                        }
                    />
                </div>

                <div class="control-row">
                    <label for="process-fit-higher-zeta">"Damping ratio"</label>
                    <output>{move || format!("{:.2}", higher_order_damping_ratio.get())}</output>
                    <input
                        id="process-fit-higher-zeta"
                        type="range"
                        min="0.12"
                        max="1.20"
                        step="0.02"
                        prop:value=move || higher_order_damping_ratio.get().to_string()
                        on:input=move |ev| {
                            if let Ok(value) = event_target_value(&ev).parse::<f64>() {
                                set_higher_order_damping_ratio.set(value.max(0.12));
                            }
                        }
                    />
                </div>

                <div class="control-row">
                    <label for="process-fit-higher-tail">"Tail lag"</label>
                    <output>{move || format!("{:.2} s", higher_order_tail_lag.get())}</output>
                    <input
                        id="process-fit-higher-tail"
                        type="range"
                        min="0.4"
                        max="4.0"
                        step="0.05"
                        prop:value=move || higher_order_tail_lag.get().to_string()
                        on:input=move |ev| {
                            if let Ok(value) = event_target_value(&ev).parse::<f64>() {
                                set_higher_order_tail_lag.set(value.max(0.4));
                            }
                        }
                    />
                </div>
            </>
        }
        .into_any(),
    };

    view! {
        <div class="page">
            <header class="page-header">
                <p class="eyebrow">"Identification"</p>
                <h1>"FOPDT / SOPDT Fitting"</h1>
                <p>
                    "Sampled step data is generated from either matched low-order process models or a configurable"
                    " higher-order plant, then both the FOPDT and SOPDT fitting utilities are run on the same record."
                </p>
            </header>

            <div class="control-layout">
                <aside class="control-card">
                    <section>
                        <h2>"Source data"</h2>
                        <p class="section-copy">
                            "Matched sources show the intended process-model workflow. The higher-order source lets you"
                            " dial in underdamped or long-tail behavior and see how the low-order surrogates respond."
                            " The fit tolerance controls how hard the nonlinear least-squares refinement works before"
                            " the page redraws."
                        </p>

                        <div class="control-row">
                            <label for="process-fit-source-kind">"Source plant"</label>
                            <select
                                id="process-fit-source-kind"
                                prop:value=move || source_kind.get().as_key().to_string()
                                on:change=move |ev| {
                                    set_source_kind.set(FitSourceKind::from_key(&event_target_value(&ev)));
                                }
                            >
                                <option value="fopdt">"Matched FOPDT source"</option>
                                <option value="sopdt">"Matched SOPDT source"</option>
                                <option value="higher-order">"Unmatched higher-order source"</option>
                            </select>
                        </div>

                        {source_controls}

                        <div class="control-row">
                            <label for="process-fit-noise">"Output noise"</label>
                            <output>{move || format!("{:.3}", noise_level.get())}</output>
                            <input
                                id="process-fit-noise"
                                type="range"
                                min="0.0"
                                max="0.12"
                                step="0.0025"
                                prop:value=move || noise_level.get().to_string()
                                on:input=move |ev| {
                                    if let Ok(value) = event_target_value(&ev).parse::<f64>() {
                                        set_noise_level.set(value.max(0.0));
                                    }
                                }
                            />
                        </div>

                        <div class="control-row">
                            <label for="process-fit-tolerance">"Fit tolerance"</label>
                            <output>{move || format!("{:.1e}", fit_tolerance.get())}</output>
                            <input
                                id="process-fit-tolerance"
                                type="range"
                                min="0.0001"
                                max="0.05"
                                step="0.0001"
                                prop:value=move || fit_tolerance.get().to_string()
                                on:input=move |ev| {
                                    if let Ok(value) = event_target_value(&ev).parse::<f64>() {
                                        set_fit_tolerance.set(value.clamp(1.0e-4, 5.0e-2));
                                    }
                                }
                            />
                        </div>
                    </section>

                    <section>
                        <h2>"Interpretation"</h2>
                        <p class="section-copy">{move || interpretation_copy(source_kind.get())}</p>
                    </section>

                    <section>
                        <h2>"Run summary"</h2>
                        <p class="section-copy">{summary}</p>
                    </section>
                </aside>

                <div class="plots-grid wide">
                    <article class="plot-card">
                        <div class="plot-header">
                            <div>
                                <h2>"Process-model fit traces"</h2>
                                <p>"Fit comparison and residual view for the same sampled step-response record."</p>
                            </div>
                        </div>
                        <div class="plot-subsection">
                            <div class="plot-header">
                                <div>
                                    <h2>"Step-response fit"</h2>
                                    <p>"Noisy sampled data, the true source response, and both fitted low-order surrogates."</p>
                                </div>
                            </div>
                            <div id="process-fit-response-plot" class="plot-surface"></div>
                        </div>

                        <div class="plot-subsection">
                            <div class="plot-header">
                                <div>
                                    <h2>"Residuals"</h2>
                                    <p>"Signed fit error against the sampled data used by the fitting routines."</p>
                                </div>
                            </div>
                            <div id="process-fit-residual-plot" class="plot-surface"></div>
                        </div>

                        <div class="plot-subsection">
                            <div class="plot-header">
                                <div>
                                    <h2>"Bode magnitude"</h2>
                                    <p>"Frequency-response magnitude for the true source and both fitted process models."</p>
                                </div>
                            </div>
                            <div id="process-fit-bode-mag-plot" class="plot-surface"></div>
                        </div>

                        <div class="plot-subsection">
                            <div class="plot-header">
                                <div>
                                    <h2>"Bode phase"</h2>
                                    <p>"Continuous-time phase comparison on the same source-versus-fit frequency grid."</p>
                                </div>
                            </div>
                            <div id="process-fit-bode-phase-plot" class="plot-surface"></div>
                        </div>
                    </article>
                </div>
            </div>
        </div>
    }
}

#[derive(Clone, Copy)]
enum ProcessFitPlot {
    Response,
    Residual,
    BodeMagnitude,
    BodePhase,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FitSourceKind {
    Fopdt,
    Sopdt,
    HigherOrder,
}

impl FitSourceKind {
    fn as_key(self) -> &'static str {
        match self {
            Self::Fopdt => "fopdt",
            Self::Sopdt => "sopdt",
            Self::HigherOrder => "higher-order",
        }
    }

    fn from_key(key: &str) -> Self {
        match key {
            "sopdt" => Self::Sopdt,
            "higher-order" => Self::HigherOrder,
            _ => Self::Fopdt,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Fopdt => "matched FOPDT source",
            Self::Sopdt => "matched SOPDT source",
            Self::HigherOrder => "unmatched higher-order source",
        }
    }
}

#[derive(Clone, Copy)]
struct ProcessFitInputs {
    source_kind: FitSourceKind,
    noise_level: f64,
    fit_tolerance: f64,
    fopdt_gain: f64,
    fopdt_time_constant: f64,
    fopdt_delay: f64,
    sopdt_gain: f64,
    sopdt_time_constant_1: f64,
    sopdt_time_constant_2: f64,
    sopdt_delay: f64,
    higher_order_gain: f64,
    higher_order_zero_time_constant: f64,
    higher_order_natural_frequency: f64,
    higher_order_damping_ratio: f64,
    higher_order_tail_lag: f64,
}

impl Default for ProcessFitInputs {
    fn default() -> Self {
        Self {
            source_kind: FitSourceKind::Fopdt,
            noise_level: 0.02,
            fit_tolerance: 1.0e-3,
            fopdt_gain: 1.35,
            fopdt_time_constant: 3.0,
            fopdt_delay: 0.8,
            sopdt_gain: 1.10,
            sopdt_time_constant_1: 2.6,
            sopdt_time_constant_2: 0.85,
            sopdt_delay: 0.45,
            higher_order_gain: 1.30,
            higher_order_zero_time_constant: 0.45,
            higher_order_natural_frequency: 1.20,
            higher_order_damping_ratio: 0.28,
            higher_order_tail_lag: 1.80,
        }
    }
}

#[derive(Clone, PartialEq)]
struct ProcessFitDemo {
    times: Vec<f64>,
    measured_output: Vec<f64>,
    true_output: Vec<f64>,
    fopdt_output: Vec<f64>,
    sopdt_output: Vec<f64>,
    fopdt_residual: Vec<f64>,
    sopdt_residual: Vec<f64>,
    bode_frequencies: Vec<f64>,
    true_bode_magnitude_db: Vec<f64>,
    fopdt_bode_magnitude_db: Vec<f64>,
    sopdt_bode_magnitude_db: Vec<f64>,
    true_bode_phase_deg: Vec<f64>,
    fopdt_bode_phase_deg: Vec<f64>,
    sopdt_bode_phase_deg: Vec<f64>,
    source_kind: FitSourceKind,
    fopdt_model: FopdtModel<f64>,
    sopdt_model: SopdtModel<f64>,
    fit_tolerance: f64,
    fopdt_objective: f64,
    sopdt_objective: f64,
    fopdt_true_rms: f64,
    sopdt_true_rms: f64,
}

fn build_process_fit_plot(result: Result<ProcessFitDemo, String>, which: ProcessFitPlot) -> Plot {
    match result {
        Ok(demo) => match which {
            ProcessFitPlot::Response => build_line_plot(
                "Process-model fitting",
                "time (s)",
                "output",
                false,
                vec![
                    LineSeries::markers("sampled data", demo.times.clone(), demo.measured_output)
                        .with_marker_symbol(MarkerSymbol::Circle)
                        .with_marker_size(6)
                        .with_opacity(0.5),
                    LineSeries::lines("true source", demo.times.clone(), demo.true_output)
                        .with_dash(DashType::Dot),
                    LineSeries::lines("FOPDT fit", demo.times.clone(), demo.fopdt_output)
                        .with_dash(DashType::Solid),
                    LineSeries::lines("SOPDT fit", demo.times, demo.sopdt_output)
                        .with_dash(DashType::Dash),
                ],
            ),
            ProcessFitPlot::Residual => build_line_plot(
                "Fit residuals",
                "time (s)",
                "fit - sampled data",
                false,
                vec![
                    LineSeries::lines("FOPDT residual", demo.times.clone(), demo.fopdt_residual)
                        .with_dash(DashType::Solid),
                    LineSeries::lines("SOPDT residual", demo.times, demo.sopdt_residual)
                        .with_dash(DashType::Dash),
                ],
            ),
            ProcessFitPlot::BodeMagnitude => build_line_plot(
                "Process-model bode magnitude",
                "angular frequency",
                "magnitude (dB)",
                true,
                vec![
                    LineSeries::lines(
                        "true source",
                        demo.bode_frequencies.clone(),
                        demo.true_bode_magnitude_db,
                    )
                    .with_dash(DashType::Dot),
                    LineSeries::lines(
                        "FOPDT fit",
                        demo.bode_frequencies.clone(),
                        demo.fopdt_bode_magnitude_db,
                    )
                    .with_dash(DashType::Solid),
                    LineSeries::lines(
                        "SOPDT fit",
                        demo.bode_frequencies,
                        demo.sopdt_bode_magnitude_db,
                    )
                    .with_dash(DashType::Dash),
                ],
            ),
            ProcessFitPlot::BodePhase => build_line_plot(
                "Process-model bode phase",
                "angular frequency",
                "phase (deg)",
                true,
                vec![
                    LineSeries::lines(
                        "true source",
                        demo.bode_frequencies.clone(),
                        demo.true_bode_phase_deg,
                    )
                    .with_dash(DashType::Dot),
                    LineSeries::lines(
                        "FOPDT fit",
                        demo.bode_frequencies.clone(),
                        demo.fopdt_bode_phase_deg,
                    )
                    .with_dash(DashType::Solid),
                    LineSeries::lines(
                        "SOPDT fit",
                        demo.bode_frequencies,
                        demo.sopdt_bode_phase_deg,
                    )
                    .with_dash(DashType::Dash),
                ],
            ),
        },
        Err(message) => build_line_plot(&message, "", "", false, Vec::new()),
    }
}

fn process_fit_summary(result: Result<ProcessFitDemo, String>) -> String {
    match result {
        Ok(demo) => format!(
            "On the {} with LM tolerance {:.1e}, FOPDT objective = {:.4} and true-source RMS error = {:.4}; fitted model = K {:.3}, tau {:.3} s, delay {:.3} s. SOPDT objective = {:.4} and true-source RMS error = {:.4}; fitted model = K {:.3}, tau1 {:.3} s, tau2 {:.3} s, delay {:.3} s.",
            demo.source_kind.label(),
            demo.fit_tolerance,
            demo.fopdt_objective,
            demo.fopdt_true_rms,
            demo.fopdt_model.gain,
            demo.fopdt_model.time_constant,
            demo.fopdt_model.delay,
            demo.sopdt_objective,
            demo.sopdt_true_rms,
            demo.sopdt_model.gain,
            demo.sopdt_model.time_constant_1,
            demo.sopdt_model.time_constant_2,
            demo.sopdt_model.delay,
        ),
        Err(err) => format!("Process-model fitting failed: {err}"),
    }
}

fn interpretation_copy(source_kind: FitSourceKind) -> &'static str {
    match source_kind {
        FitSourceKind::Fopdt => {
            "The source is already FOPDT, so the FOPDT fit should usually be the cleanest description. Change gain, lag, and delay to see how robust the refinement remains under different step shapes."
        }
        FitSourceKind::Sopdt => {
            "The source is SOPDT with two real lags, so the SOPDT fit should usually leave less residual structure. Pushing the lag separation makes it easier to see where FOPDT starts to lose shape."
        }
        FitSourceKind::HigherOrder => {
            "The higher-order source uses an underdamped second-order mode followed by a slower lag. Lower damping and stronger zeros make the low-order process surrogates work harder, so the mismatch becomes easier to see."
        }
    }
}

fn run_process_fit_demo(
    inputs: ProcessFitInputs,
    fit_options: ProcessModelFitOptions,
) -> Result<ProcessFitDemo, String> {
    let dt = 0.1;
    let step_time = 0.5;
    let duration = source_duration(inputs);
    let times = linspace(0.0, duration, (duration / dt).round() as usize + 1);
    let input = times
        .iter()
        .map(|&time| if time >= step_time { 1.0 } else { 0.0 })
        .collect::<Vec<_>>();

    let true_output = true_source_response(inputs, &times, step_time)?;
    let measured_output = true_output
        .iter()
        .enumerate()
        .map(|(index, &value)| value + inputs.noise_level * gaussianish_signal(index, 0xfeed_beef))
        .collect::<Vec<_>>();

    let fit_end_time = step_time + fit_window_duration(inputs).min(duration - step_time);
    let fit_len = times
        .iter()
        .position(|&time| time > fit_end_time)
        .unwrap_or(times.len())
        .max(4);
    let data = StepResponseData::new(
        times[..fit_len].to_vec(),
        input[..fit_len].to_vec(),
        measured_output[..fit_len].to_vec(),
    )
    .map_err(|err| err.to_string())?;
    let fopdt_fit = fit_fopdt_from_step_response_with_options(&data, fit_options)
        .map_err(|err| err.to_string())?;
    let sopdt_fit = fit_sopdt_from_step_response_with_options(&data, fit_options)
        .map_err(|err| err.to_string())?;
    let bode_frequencies = process_fit_frequency_grid(inputs);
    let true_bode = true_source_bode(inputs, &bode_frequencies)?;
    let fopdt_bode = fopdt_fit
        .model
        .bode_data(&bode_frequencies)
        .map_err(|err| err.to_string())?;
    let sopdt_bode = sopdt_fit
        .model
        .bode_data(&bode_frequencies)
        .map_err(|err| err.to_string())?;

    let fopdt_output = times
        .iter()
        .map(|&time| {
            fopdt_fit
                .model
                .step_response_value((time - step_time).max(0.0), 1.0, 0.0)
        })
        .collect::<Vec<_>>();
    let sopdt_output = times
        .iter()
        .map(|&time| {
            sopdt_fit
                .model
                .step_response_value((time - step_time).max(0.0), 1.0, 0.0)
        })
        .collect::<Vec<_>>();

    let fopdt_residual = fopdt_output
        .iter()
        .zip(measured_output.iter())
        .map(|(fit, measured)| fit - measured)
        .collect::<Vec<_>>();
    let sopdt_residual = sopdt_output
        .iter()
        .zip(measured_output.iter())
        .map(|(fit, measured)| fit - measured)
        .collect::<Vec<_>>();
    let fopdt_true_rms = rms_error(&fopdt_output, &true_output);
    let sopdt_true_rms = rms_error(&sopdt_output, &true_output);

    Ok(ProcessFitDemo {
        times,
        measured_output,
        true_output,
        fopdt_output,
        sopdt_output,
        fopdt_residual,
        sopdt_residual,
        bode_frequencies: true_bode.angular_frequencies,
        true_bode_magnitude_db: true_bode.magnitude_db,
        fopdt_bode_magnitude_db: fopdt_bode.magnitude_db,
        sopdt_bode_magnitude_db: sopdt_bode.magnitude_db,
        true_bode_phase_deg: true_bode.phase_deg,
        fopdt_bode_phase_deg: fopdt_bode.phase_deg,
        sopdt_bode_phase_deg: sopdt_bode.phase_deg,
        source_kind: inputs.source_kind,
        fopdt_model: fopdt_fit.model,
        sopdt_model: sopdt_fit.model,
        fit_tolerance: inputs.fit_tolerance,
        fopdt_objective: fopdt_fit.objective,
        sopdt_objective: sopdt_fit.objective,
        fopdt_true_rms,
        sopdt_true_rms,
    })
}

fn process_fit_frequency_grid(inputs: ProcessFitInputs) -> Vec<f64> {
    let max_omega = match inputs.source_kind {
        FitSourceKind::Fopdt => (12.0 / inputs.fopdt_time_constant.max(0.1)).max(2.0),
        FitSourceKind::Sopdt => {
            let fastest_lag = inputs
                .sopdt_time_constant_1
                .min(inputs.sopdt_time_constant_2)
                .max(0.05);
            (12.0 / fastest_lag).max(2.0)
        }
        FitSourceKind::HigherOrder => {
            let fastest_lag = inputs.higher_order_tail_lag.max(0.1);
            (8.0 * inputs.higher_order_natural_frequency.max(0.25)).max(10.0 / fastest_lag)
        }
    };
    logspace(-3.0, max_omega.log10(), 260)
}

fn true_source_bode(
    inputs: ProcessFitInputs,
    angular_frequencies: &[f64],
) -> Result<numerical::control::lti::BodeData<f64>, String> {
    match inputs.source_kind {
        FitSourceKind::Fopdt => FopdtModel {
            gain: inputs.fopdt_gain,
            time_constant: inputs.fopdt_time_constant,
            delay: inputs.fopdt_delay,
        }
        .bode_data(angular_frequencies)
        .map_err(|err| err.to_string()),
        FitSourceKind::Sopdt => SopdtModel {
            gain: inputs.sopdt_gain,
            time_constant_1: inputs.sopdt_time_constant_1,
            time_constant_2: inputs.sopdt_time_constant_2,
            delay: inputs.sopdt_delay,
        }
        .bode_data(angular_frequencies)
        .map_err(|err| err.to_string()),
        FitSourceKind::HigherOrder => higher_order_transfer_function(
            inputs.higher_order_gain,
            inputs.higher_order_zero_time_constant,
            inputs.higher_order_natural_frequency,
            inputs.higher_order_damping_ratio,
            inputs.higher_order_tail_lag,
        )?
        .bode_data(angular_frequencies)
        .map_err(|err| err.to_string()),
    }
}

fn true_source_response(
    inputs: ProcessFitInputs,
    times: &[f64],
    step_time: f64,
) -> Result<Vec<f64>, String> {
    match inputs.source_kind {
        FitSourceKind::Fopdt => {
            let model = FopdtModel {
                gain: inputs.fopdt_gain,
                time_constant: inputs.fopdt_time_constant,
                delay: inputs.fopdt_delay,
            };
            Ok(times
                .iter()
                .map(|&time| model.step_response_value((time - step_time).max(0.0), 1.0, 0.0))
                .collect::<Vec<_>>())
        }
        FitSourceKind::Sopdt => {
            let model = SopdtModel {
                gain: inputs.sopdt_gain,
                time_constant_1: inputs.sopdt_time_constant_1,
                time_constant_2: inputs.sopdt_time_constant_2,
                delay: inputs.sopdt_delay,
            };
            Ok(times
                .iter()
                .map(|&time| model.step_response_value((time - step_time).max(0.0), 1.0, 0.0))
                .collect::<Vec<_>>())
        }
        FitSourceKind::HigherOrder => higher_order_response(
            inputs.higher_order_gain,
            inputs.higher_order_zero_time_constant,
            inputs.higher_order_natural_frequency,
            inputs.higher_order_damping_ratio,
            inputs.higher_order_tail_lag,
            times,
            step_time,
        ),
    }
}

fn higher_order_response(
    gain: f64,
    zero_time_constant: f64,
    natural_frequency: f64,
    damping_ratio: f64,
    tail_lag: f64,
    times: &[f64],
    step_time: f64,
) -> Result<Vec<f64>, String> {
    let plant = higher_order_transfer_function(
        gain,
        zero_time_constant,
        natural_frequency,
        damping_ratio,
        tail_lag,
    )?
    .to_state_space()
    .map_err(|err| err.to_string())?;
    let relative_times = times
        .iter()
        .map(|&time| (time - step_time).max(0.0))
        .collect::<Vec<_>>();
    let response = plant
        .step_response(&relative_times)
        .map_err(|err| err.to_string())?;
    Ok(response.values.iter().map(|block| block[(0, 0)]).collect())
}

fn higher_order_transfer_function(
    gain: f64,
    zero_time_constant: f64,
    natural_frequency: f64,
    damping_ratio: f64,
    tail_lag: f64,
) -> Result<ContinuousTransferFunction<f64>, String> {
    let wn2 = natural_frequency * natural_frequency;
    let numerator = vec![gain * wn2 * zero_time_constant, gain * wn2];
    let denominator = vec![
        tail_lag,
        1.0 + 2.0 * damping_ratio * natural_frequency * tail_lag,
        2.0 * damping_ratio * natural_frequency + wn2 * tail_lag,
        wn2,
    ];
    ContinuousTransferFunction::continuous(numerator, denominator).map_err(|err| err.to_string())
}

fn source_duration(inputs: ProcessFitInputs) -> f64 {
    match inputs.source_kind {
        FitSourceKind::Fopdt => {
            (10.0 * (inputs.fopdt_time_constant + inputs.fopdt_delay)).clamp(18.0, 42.0)
        }
        FitSourceKind::Sopdt => (10.0
            * (inputs.sopdt_time_constant_1 + inputs.sopdt_time_constant_2 + inputs.sopdt_delay))
            .clamp(20.0, 44.0),
        FitSourceKind::HigherOrder => (8.0 * inputs.higher_order_tail_lag
            + 8.0 * (2.0 * std::f64::consts::PI / inputs.higher_order_natural_frequency))
            .clamp(20.0, 44.0),
    }
}

fn fit_window_duration(inputs: ProcessFitInputs) -> f64 {
    match inputs.source_kind {
        FitSourceKind::Fopdt => 2.0 * (inputs.fopdt_time_constant + inputs.fopdt_delay),
        FitSourceKind::Sopdt => {
            2.0 * (inputs.sopdt_time_constant_1 + inputs.sopdt_time_constant_2 + inputs.sopdt_delay)
        }
        FitSourceKind::HigherOrder => {
            let oscillatory_envelope = 1.0
                / (inputs.higher_order_damping_ratio * inputs.higher_order_natural_frequency)
                    .max(0.1);
            2.0 * (inputs.higher_order_tail_lag
                + oscillatory_envelope
                + inputs.higher_order_zero_time_constant)
        }
    }
}

fn rms_error(lhs: &[f64], rhs: &[f64]) -> f64 {
    let mean_square = lhs
        .iter()
        .zip(rhs.iter())
        .map(|(lhs, rhs)| {
            let err = lhs - rhs;
            err * err
        })
        .sum::<f64>()
        / (lhs.len() as f64);
    mean_square.sqrt()
}

#[cfg(test)]
mod tests {
    use super::{FitSourceKind, ProcessFitInputs, run_process_fit_demo};
    use numerical::control::synthesis::ProcessModelFitOptions;

    #[test]
    fn process_fit_demo_runs_for_all_sources() {
        for source in [
            FitSourceKind::Fopdt,
            FitSourceKind::Sopdt,
            FitSourceKind::HigherOrder,
        ] {
            let mut inputs = ProcessFitInputs::default();
            inputs.source_kind = source;
            let demo = run_process_fit_demo(
                inputs,
                ProcessModelFitOptions {
                    tolerance: Some(1.0e-3),
                    patience: Some(6),
                },
            )
            .unwrap();
            assert_eq!(demo.times.len(), demo.true_output.len());
            assert_eq!(demo.times.len(), demo.fopdt_output.len());
            assert_eq!(demo.times.len(), demo.sopdt_output.len());
            assert_eq!(
                demo.bode_frequencies.len(),
                demo.true_bode_magnitude_db.len()
            );
            assert_eq!(
                demo.bode_frequencies.len(),
                demo.fopdt_bode_magnitude_db.len()
            );
            assert_eq!(
                demo.bode_frequencies.len(),
                demo.sopdt_bode_magnitude_db.len()
            );
            assert_eq!(demo.bode_frequencies.len(), demo.true_bode_phase_deg.len());
            assert_eq!(demo.bode_frequencies.len(), demo.fopdt_bode_phase_deg.len());
            assert_eq!(demo.bode_frequencies.len(), demo.sopdt_bode_phase_deg.len());
        }
    }
}
