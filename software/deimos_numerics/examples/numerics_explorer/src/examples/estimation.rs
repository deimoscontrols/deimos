use crate::demo_signal::{colored_signal, gaussianish_signal};
use crate::plot_helpers::{LineSeries, build_line_plot};
use crate::plotly_support::use_plotly_chart;
use faer::Mat;
use leptos::prelude::*;
use numerical::control::estimation::{DiscreteKalmanFilter, SteadyStateKalmanFilter};
use numerical::control::lti::DiscreteStateSpace;
use plotly::Plot;

/// Interactive linear-estimation page comparing full and steady-state Kalman
/// filters on the same measured signal.
#[component]
pub fn EstimationPage() -> impl IntoView {
    let (true_process_noise, set_true_process_noise) = signal(0.18_f64);
    let (true_measurement_noise, set_true_measurement_noise) = signal(0.35_f64);
    let (assumed_process_noise, set_assumed_process_noise) = signal(0.18_f64);
    let (assumed_measurement_noise, set_assumed_measurement_noise) = signal(0.35_f64);
    let (pin_assumptions, set_pin_assumptions) = signal(true);

    let effective_assumed_process_noise = Memo::new(move |_| {
        if pin_assumptions.get() {
            true_process_noise.get()
        } else {
            assumed_process_noise.get()
        }
    });
    let effective_assumed_measurement_noise = Memo::new(move |_| {
        if pin_assumptions.get() {
            true_measurement_noise.get()
        } else {
            assumed_measurement_noise.get()
        }
    });

    use_plotly_chart("estimator-position-plot", move || {
        build_estimation_plot(
            true_process_noise.get(),
            true_measurement_noise.get(),
            effective_assumed_process_noise.get(),
            effective_assumed_measurement_noise.get(),
            EstimationPlot::State,
        )
    });
    use_plotly_chart("estimator-variance-plot", move || {
        build_estimation_plot(
            true_process_noise.get(),
            true_measurement_noise.get(),
            effective_assumed_process_noise.get(),
            effective_assumed_measurement_noise.get(),
            EstimationPlot::Variance,
        )
    });

    let summary = move || {
        estimation_summary(
            true_process_noise.get(),
            true_measurement_noise.get(),
            effective_assumed_process_noise.get(),
            effective_assumed_measurement_noise.get(),
        )
    };

    view! {
        <div class="page">
            <header class="page-header">
                <p class="eyebrow">"Estimation"</p>
                <h1>"Discrete Kalman Workbench"</h1>
                <p>
                    "A noisy constant-velocity sensor stream is filtered with both the full recursive"
                    " discrete Kalman filter and its fixed-gain steady-state limit. The plots are generated"
                    " directly from the runtime estimator APIs."
                </p>
            </header>

            <div class="control-layout">
                <aside class="control-card">
                    <section>
                        <h2>"Settings"</h2>
                        <p class="section-copy">
                            "These sliders control the synthetic plant disturbance and sensor corruption used to"
                            " generate the truth and measurement traces, plus the Kalman covariances assumed by"
                            " the two filters."
                        </p>

                        <div class="control-row">
                            <label for="kalman-true-process-noise">"True process noise"</label>
                            <output>{move || format!("{:.3}", true_process_noise.get())}</output>
                            <input
                                id="kalman-true-process-noise"
                                type="range"
                                min="0.02"
                                max="2.00"
                                step="0.01"
                                prop:value=move || true_process_noise.get().to_string()
                                on:input=move |ev| {
                                    if let Ok(value) = event_target_value(&ev).parse::<f64>() {
                                        set_true_process_noise.set(value.max(0.02));
                                    }
                                }
                            />
                        </div>

                        <div class="control-row">
                            <label for="kalman-true-measurement-noise">"True measurement noise"</label>
                            <output>{move || format!("{:.3}", true_measurement_noise.get())}</output>
                            <input
                                id="kalman-true-measurement-noise"
                                type="range"
                                min="0.05"
                                max="2.00"
                                step="0.01"
                                prop:value=move || true_measurement_noise.get().to_string()
                                on:input=move |ev| {
                                    if let Ok(value) = event_target_value(&ev).parse::<f64>() {
                                        set_true_measurement_noise.set(value.max(0.05));
                                    }
                                }
                            />
                        </div>
                    </section>

                    <section>
                        <h2>"Filter assumptions"</h2>
                        <p class="section-copy">
                            "The recursive and steady-state Kalman filters still interpret the disturbance and sensor"
                            " error through Gaussian covariances `W` and `V`. Pinning the assumptions to truth gives the"
                            " matched-model baseline; unlocking them lets you study mismatch directly."
                        </p>

                        <div class="control-row">
                            <label for="kalman-pin-assumptions">"Pin assumptions to truth"</label>
                            <input
                                id="kalman-pin-assumptions"
                                type="checkbox"
                                prop:checked=move || pin_assumptions.get()
                                on:change=move |ev| set_pin_assumptions.set(event_target_checked(&ev))
                            />
                        </div>

                        <div class="control-row">
                            <label for="kalman-assumed-process-noise">"Assumed process noise"</label>
                            <output>{move || format!("{:.3}", effective_assumed_process_noise.get())}</output>
                            <input
                                id="kalman-assumed-process-noise"
                                type="range"
                                min="0.02"
                                max="2.00"
                                step="0.01"
                                prop:value=move || effective_assumed_process_noise.get().to_string()
                                prop:disabled=move || pin_assumptions.get()
                                on:input=move |ev| {
                                    if let Ok(value) = event_target_value(&ev).parse::<f64>() {
                                        set_assumed_process_noise.set(value.max(0.02));
                                    }
                                }
                            />
                        </div>

                        <div class="control-row">
                            <label for="kalman-assumed-measurement-noise">"Assumed measurement noise"</label>
                            <output>{move || format!("{:.3}", effective_assumed_measurement_noise.get())}</output>
                            <input
                                id="kalman-assumed-measurement-noise"
                                type="range"
                                min="0.05"
                                max="2.00"
                                step="0.01"
                                prop:value=move || effective_assumed_measurement_noise.get().to_string()
                                prop:disabled=move || pin_assumptions.get()
                                on:input=move |ev| {
                                    if let Ok(value) = event_target_value(&ev).parse::<f64>() {
                                        set_assumed_measurement_noise.set(value.max(0.05));
                                    }
                                }
                            />
                        </div>
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
                                <h2>"Kalman filter traces"</h2>
                                <p>"State and covariance views of the same recursive versus steady-state comparison."</p>
                            </div>
                        </div>
                        <div class="plot-subsection">
                            <div class="plot-header">
                                <div>
                                    <h2>"Position estimate"</h2>
                                    <p>"Truth, noisy measurement, recursive Kalman estimate, and fixed-gain estimate."</p>
                                </div>
                            </div>
                            <div id="estimator-position-plot" class="plot-surface"></div>
                        </div>

                        <div class="plot-subsection">
                            <div class="plot-header">
                                <div>
                                    <h2>"Position variance"</h2>
                                    <p>"Recursive posterior variance converging toward the steady-state observer covariance."</p>
                                </div>
                            </div>
                            <div id="estimator-variance-plot" class="plot-surface"></div>
                        </div>
                    </article>

                    <section class="home-grid">
                        <article class="home-card">
                            <h2>"Plant model"</h2>
                            <p class="section-copy">
                                "The plant is a sampled constant-velocity model with state `[position, velocity]^T`."
                                " It evolves as `x_(k+1) = A x_k + B (u_k + d_k)`, where `u_k` is a smooth deterministic"
                                " command and `d_k` is an acceleration disturbance injected through the same input channel."
                            </p>
                        </article>

                        <article class="home-card">
                            <h2>"Noise generation"</h2>
                            <p class="section-copy">
                                "The process disturbance `d_k` is a smooth colored sequence scaled by the truth-side"
                                " process-noise slider. The measurement error `n_k` mixes deterministic Gaussian-like"
                                " jitter with a smaller colored bias term, so the raw measurement looks like a genuinely"
                                " noisy sensor instead of a second smooth trajectory."
                            </p>
                        </article>

                        <article class="home-card">
                            <h2>"How To Read It"</h2>
                            <p class="section-copy">
                                "Process noise means unmodeled acceleration disturbance, so it changes the truth trajectory."
                                " Measurement noise means sensor corruption, so it changes only the observed trace. Raising"
                                " assumed process noise makes the filters more reactive; raising assumed measurement noise"
                                " makes them smoother. Unlock the assumptions to study mismatch directly."
                            </p>
                        </article>
                    </section>
                </div>
            </div>
        </div>
    }
}

#[derive(Clone, Copy)]
enum EstimationPlot {
    State,
    Variance,
}

struct EstimationDemo {
    times: Vec<f64>,
    truth_position: Vec<f64>,
    measured_position: Vec<f64>,
    kalman_position: Vec<f64>,
    steady_position: Vec<f64>,
    kalman_variance: Vec<f64>,
    steady_variance: Vec<f64>,
    final_kalman_error: f64,
    final_steady_error: f64,
}

fn build_estimation_plot(
    true_process_noise: f64,
    true_measurement_noise: f64,
    assumed_process_noise: f64,
    assumed_measurement_noise: f64,
    which: EstimationPlot,
) -> Plot {
    match run_estimation_demo(
        true_process_noise,
        true_measurement_noise,
        assumed_process_noise,
        assumed_measurement_noise,
    ) {
        Ok(demo) => match which {
            EstimationPlot::State => build_line_plot(
                "Position estimate",
                "time (s)",
                "position",
                false,
                vec![
                    LineSeries::lines("truth", demo.times.clone(), demo.truth_position),
                    LineSeries::lines("measurement", demo.times.clone(), demo.measured_position)
                        .with_line_width(1.0)
                        .with_opacity(1.0),
                    LineSeries::lines("kalman", demo.times.clone(), demo.kalman_position),
                    LineSeries::lines("steady-state", demo.times, demo.steady_position),
                ],
            ),
            EstimationPlot::Variance => build_line_plot(
                "Posterior position variance",
                "time (s)",
                "variance",
                false,
                vec![
                    LineSeries::lines("kalman P[0,0]", demo.times.clone(), demo.kalman_variance),
                    LineSeries::lines("steady-state P[0,0]", demo.times, demo.steady_variance),
                ],
            ),
        },
        Err(message) => build_line_plot(&message, "time (s)", "", false, Vec::new()),
    }
}

fn estimation_summary(
    true_process_noise: f64,
    true_measurement_noise: f64,
    assumed_process_noise: f64,
    assumed_measurement_noise: f64,
) -> String {
    match run_estimation_demo(
        true_process_noise,
        true_measurement_noise,
        assumed_process_noise,
        assumed_measurement_noise,
    ) {
        Ok(demo) => format!(
            "Truth uses process {:.3} and measurement {:.3}; the filters assume process {:.3} and measurement {:.3}. Final position error: recursive {:.3}, steady-state {:.3}.",
            true_process_noise,
            true_measurement_noise,
            assumed_process_noise,
            assumed_measurement_noise,
            demo.final_kalman_error,
            demo.final_steady_error,
        ),
        Err(err) => format!("Estimator setup failed: {err}"),
    }
}

fn run_estimation_demo(
    true_process_noise: f64,
    true_measurement_noise: f64,
    assumed_process_noise: f64,
    assumed_measurement_noise: f64,
) -> Result<EstimationDemo, String> {
    let dt: f64 = 0.1;
    let system = DiscreteStateSpace::new(
        Mat::from_fn(2, 2, |row, col| match (row, col) {
            (0, 0) => 1.0,
            (0, 1) => dt,
            (1, 0) => 0.0,
            (1, 1) => 1.0,
            _ => 0.0,
        }),
        Mat::from_fn(2, 1, |row, _| if row == 0 { 0.5 * dt * dt } else { dt }),
        Mat::from_fn(1, 2, |_, col| if col == 0 { 1.0 } else { 0.0 }),
        Mat::zeros(1, 1),
        dt,
    )
    .map_err(|err| err.to_string())?;

    let q = assumed_process_noise * assumed_process_noise;
    let w = Mat::from_fn(2, 2, |row, col| match (row, col) {
        (0, 0) => 0.25 * dt.powi(4) * q,
        (0, 1) | (1, 0) => 0.5 * dt.powi(3) * q,
        (1, 1) => dt.powi(2) * q,
        _ => 0.0,
    });
    let v = Mat::from_fn(1, 1, |_, _| {
        assumed_measurement_noise * assumed_measurement_noise
    });
    let x0 = Mat::zeros(2, 1);
    let p0 = Mat::from_fn(2, 2, |row, col| {
        if row == col {
            if row == 0 { 4.0 } else { 1.0 }
        } else {
            0.0
        }
    });

    let mut kalman =
        DiscreteKalmanFilter::from_state_space(&system, w.clone(), v.clone(), x0.clone(), p0)
            .map_err(|err| err.to_string())?;
    let mut steady = SteadyStateKalmanFilter::from_dlqe(&system, w.as_ref(), v.as_ref(), x0)
        .map_err(|err| err.to_string())?;
    let steady_var = steady
        .steady_state_covariance()
        .map(|cov| cov[(0, 0)])
        .unwrap_or(0.0);

    let n_steps = 120;
    let mut truth = [0.0_f64, 0.0_f64];
    let mut times = Vec::with_capacity(n_steps);
    let mut truth_position = Vec::with_capacity(n_steps);
    let mut measured_position = Vec::with_capacity(n_steps);
    let mut kalman_position = Vec::with_capacity(n_steps);
    let mut steady_position = Vec::with_capacity(n_steps);
    let mut kalman_variance = Vec::with_capacity(n_steps);
    let mut steady_variance = Vec::with_capacity(n_steps);

    for step in 0..n_steps {
        let t = (step as f64) * dt;
        let command = 0.35 * (0.12 * (step as f64)).sin() + if step >= 45 { 0.18 } else { 0.0 };
        let disturbance = true_process_noise * colored_signal(step, 0.31);
        let measurement_noise = gaussianish_signal(step, 11) + 0.20 * colored_signal(step, 1.17);
        let measurement = truth[0] + true_measurement_noise * measurement_noise;

        let input = Mat::from_fn(1, 1, |_, _| command);
        let measurement_mat = Mat::from_fn(1, 1, |_, _| measurement);
        let kalman_update = kalman
            .step(input.as_ref(), measurement_mat.as_ref())
            .map_err(|err| err.to_string())?;
        let steady_update = steady
            .step(input.as_ref(), measurement_mat.as_ref())
            .map_err(|err| err.to_string())?;

        times.push(t);
        truth_position.push(truth[0]);
        measured_position.push(measurement);
        kalman_position.push(kalman_update.state[(0, 0)]);
        steady_position.push(steady_update.state[(0, 0)]);
        kalman_variance.push(kalman_update.covariance[(0, 0)]);
        steady_variance.push(steady_var);

        let applied_input = command + disturbance;
        let next_position = truth[0] + dt * truth[1] + 0.5 * dt * dt * applied_input;
        let next_velocity = truth[1] + dt * applied_input;
        truth = [next_position, next_velocity];
    }

    let final_truth = *truth_position.last().unwrap_or(&0.0);
    let final_kalman = *kalman_position.last().unwrap_or(&0.0);
    let final_steady = *steady_position.last().unwrap_or(&0.0);

    Ok(EstimationDemo {
        times,
        truth_position,
        measured_position,
        kalman_position,
        steady_position,
        kalman_variance,
        steady_variance,
        final_kalman_error: final_kalman - final_truth,
        final_steady_error: final_steady - final_truth,
    })
}
