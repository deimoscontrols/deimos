use crate::plot_helpers::{LineSeries, build_line_plot, logspace};
use crate::plotly_support::use_plotly_chart;
use faer::Mat;
use leptos::prelude::*;
use numerical::control::identification::{EraError, EraParams, OkidParams, era_from_markov, okid};
use numerical::control::lti::DiscreteStateSpace;
use plotly::Plot;

/// Interactive identification page demonstrating an OKID -> ERA workflow on
/// simulated sampled input/output data.
#[component]
pub fn IdentificationPage() -> impl IntoView {
    let (plant_order, set_plant_order) = signal(3_usize);
    let (noise_level, set_noise_level) = signal(0.03_f64);
    let (observer_order, set_observer_order) = signal(6_usize);
    let (retained_order, set_retained_order) = signal(2_usize);
    let demo = Memo::new(move |_| {
        run_identification_demo(
            plant_order.get(),
            noise_level.get(),
            observer_order.get(),
            retained_order.get(),
        )
    });

    use_plotly_chart("identification-markov-plot", move || {
        build_identification_plot(demo.get(), IdentificationPlot::Markov)
    });
    use_plotly_chart("identification-step-plot", move || {
        build_identification_plot(demo.get(), IdentificationPlot::StepResponse)
    });
    use_plotly_chart("identification-error-plot", move || {
        build_identification_plot(demo.get(), IdentificationPlot::StepError)
    });
    use_plotly_chart("identification-bode-mag-plot", move || {
        build_identification_plot(demo.get(), IdentificationPlot::BodeMagnitude)
    });
    use_plotly_chart("identification-bode-phase-plot", move || {
        build_identification_plot(demo.get(), IdentificationPlot::BodePhase)
    });

    let summary = move || identification_summary(demo.get());

    view! {
        <div class="page">
            <header class="page-header">
                <p class="eyebrow">"Identification"</p>
                <h1>"OKID + ERA Recovery"</h1>
                <p>
                    "A planted discrete-time SISO system is excited with a deterministic rich input, OKID estimates"
                    " its Markov parameters from the resulting noisy input/output history, and ERA realizes a reduced"
                    " model from that recovered sequence."
                </p>
            </header>

            <div class="control-layout">
                <aside class="control-card">
                    <section>
                        <h2>"Identification controls"</h2>
                        <p class="section-copy">
                            "Plant order controls the planted discrete-time system size. Observer order controls the"
                            " lifted OKID regression horizon. Retained order controls the final ERA realization size."
                        </p>

                        <div class="control-row">
                            <label for="id-plant-order">"Plant order"</label>
                            <output>{move || plant_order.get().to_string()}</output>
                            <input
                                id="id-plant-order"
                                type="range"
                                min="2"
                                max="8"
                                step="1"
                                prop:value=move || plant_order.get().to_string()
                                on:input=move |ev| {
                                    if let Ok(value) = event_target_value(&ev).parse::<usize>() {
                                        set_plant_order.set(value.clamp(2, 8));
                                    }
                                }
                            />
                        </div>

                        <div class="control-row">
                            <label for="id-noise-level">"Output noise"</label>
                            <output>{move || format!("{:.3}", noise_level.get())}</output>
                            <input
                                id="id-noise-level"
                                type="range"
                                min="0.0"
                                max="0.15"
                                step="0.005"
                                prop:value=move || noise_level.get().to_string()
                                on:input=move |ev| {
                                    if let Ok(value) = event_target_value(&ev).parse::<f64>() {
                                        set_noise_level.set(value.max(0.0));
                                    }
                                }
                            />
                        </div>

                        <div class="control-row">
                            <label for="id-observer-order">"Observer order"</label>
                            <output>{move || observer_order.get().to_string()}</output>
                            <input
                                id="id-observer-order"
                                type="range"
                                min="3"
                                max="12"
                                step="1"
                                prop:value=move || observer_order.get().to_string()
                                on:input=move |ev| {
                                    if let Ok(value) = event_target_value(&ev).parse::<usize>() {
                                        set_observer_order.set(value.max(3));
                                    }
                                }
                            />
                        </div>

                        <div class="control-row">
                            <label for="id-retained-order">"ERA order"</label>
                            <output>{move || retained_order.get().to_string()}</output>
                            <input
                                id="id-retained-order"
                                type="range"
                                min="1"
                                max="5"
                                step="1"
                                prop:value=move || retained_order.get().to_string()
                                on:input=move |ev| {
                                    if let Ok(value) = event_target_value(&ev).parse::<usize>() {
                                        set_retained_order.set(value.max(1));
                                    }
                                }
                            />
                        </div>
                    </section>

                    <section>
                        <h2>"Interpretation"</h2>
                        <p class="section-copy">
                            "The left plot checks whether OKID recovered the planted Markov sequence. The right plot"
                            " checks whether the ERA realization built from those Markov parameters reproduces the"
                            " planted step and frequency response."
                        </p>
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
                                <h2>"Identification traces"</h2>
                                <p>"Markov recovery, realized step response, and model mismatch for the same OKID -> ERA run."</p>
                            </div>
                        </div>
                        <div class="plot-subsection">
                            <div class="plot-header">
                                <div>
                                    <h2>"Markov parameters"</h2>
                                    <p>"Planted versus identified impulse-response blocks."</p>
                                </div>
                            </div>
                            <div id="identification-markov-plot" class="plot-surface"></div>
                        </div>

                        <div class="plot-subsection">
                            <div class="plot-header">
                                <div>
                                    <h2>"Step response"</h2>
                                    <p>"Planted model versus the ERA realization built from the identified Markov sequence."</p>
                                </div>
                            </div>
                            <div id="identification-step-plot" class="plot-surface"></div>
                        </div>

                        <div class="plot-subsection">
                            <div class="plot-header">
                                <div>
                                    <h2>"Plant vs. model error"</h2>
                                    <p>"Signed step-response mismatch between the planted system and the ERA realization."</p>
                                </div>
                            </div>
                            <div id="identification-error-plot" class="plot-surface"></div>
                        </div>

                        <div class="plot-subsection">
                            <div class="plot-header">
                                <div>
                                    <h2>"Bode magnitude"</h2>
                                    <p>"Frequency-response magnitude for the planted system and the ERA realization."</p>
                                </div>
                            </div>
                            <div id="identification-bode-mag-plot" class="plot-surface"></div>
                        </div>

                        <div class="plot-subsection">
                            <div class="plot-header">
                                <div>
                                    <h2>"Bode phase"</h2>
                                    <p>"Frequency-response phase for the same planted-versus-identified comparison."</p>
                                </div>
                            </div>
                            <div id="identification-bode-phase-plot" class="plot-surface"></div>
                        </div>
                    </article>
                </div>
            </div>
        </div>
    }
}

#[derive(Clone, Copy)]
enum IdentificationPlot {
    Markov,
    StepResponse,
    StepError,
    BodeMagnitude,
    BodePhase,
}

#[derive(Clone, PartialEq)]
struct IdentificationDemo {
    lags: Vec<f64>,
    planted_markov: Vec<f64>,
    identified_markov: Vec<f64>,
    steps: Vec<f64>,
    planted_step: Vec<f64>,
    identified_step: Vec<f64>,
    step_error: Vec<f64>,
    bode_frequency_over_fs: Vec<f64>,
    planted_bode_magnitude_db: Vec<f64>,
    identified_bode_magnitude_db: Vec<f64>,
    planted_bode_phase_deg: Vec<f64>,
    identified_bode_phase_deg: Vec<f64>,
    plant_order: usize,
    requested_order: usize,
    retained_order: usize,
    rms_step_error: f64,
}

fn build_identification_plot(
    result: Result<IdentificationDemo, String>,
    which: IdentificationPlot,
) -> Plot {
    match result {
        Ok(demo) => match which {
            IdentificationPlot::Markov => build_line_plot(
                "Markov-parameter recovery",
                "lag index",
                "H[k]",
                false,
                vec![
                    LineSeries::lines_markers("planted", demo.lags.clone(), demo.planted_markov),
                    LineSeries::lines_markers("identified", demo.lags, demo.identified_markov),
                ],
            ),
            IdentificationPlot::StepResponse => build_line_plot(
                "Step-response comparison",
                "step index",
                "output",
                false,
                vec![
                    LineSeries::lines("planted", demo.steps.clone(), demo.planted_step),
                    LineSeries::lines("identified", demo.steps, demo.identified_step),
                ],
            ),
            IdentificationPlot::StepError => build_line_plot(
                "Step-response error",
                "step index",
                "identified - planted",
                false,
                vec![LineSeries::lines("error", demo.steps, demo.step_error)],
            ),
            IdentificationPlot::BodeMagnitude => build_line_plot(
                "Frequency-response magnitude",
                "frequency / fs",
                "magnitude (dB)",
                true,
                vec![
                    LineSeries::lines(
                        "planted",
                        demo.bode_frequency_over_fs.clone(),
                        demo.planted_bode_magnitude_db,
                    ),
                    LineSeries::lines(
                        "identified",
                        demo.bode_frequency_over_fs,
                        demo.identified_bode_magnitude_db,
                    ),
                ],
            ),
            IdentificationPlot::BodePhase => build_line_plot(
                "Frequency-response phase",
                "frequency / fs",
                "phase (deg)",
                true,
                vec![
                    LineSeries::lines(
                        "planted",
                        demo.bode_frequency_over_fs.clone(),
                        demo.planted_bode_phase_deg,
                    ),
                    LineSeries::lines(
                        "identified",
                        demo.bode_frequency_over_fs,
                        demo.identified_bode_phase_deg,
                    ),
                ],
            ),
        },
        Err(message) => build_line_plot(&message, "", "", false, Vec::new()),
    }
}

fn identification_summary(result: Result<IdentificationDemo, String>) -> String {
    match result {
        Ok(demo) => {
            let order_note = if demo.requested_order == demo.retained_order {
                format!("ERA retained order {}", demo.retained_order)
            } else {
                format!(
                    "ERA retained order {} (requested {}, capped by available Hankel spectrum)",
                    demo.retained_order, demo.requested_order
                )
            };
            format!(
                "Planted order {} with {} produced step-response RMS error {:.4}. Larger observer horizons usually improve the recovered Markov sequence until noise starts to dominate the regression.",
                demo.plant_order, order_note, demo.rms_step_error,
            )
        }
        Err(err) => format!("Identification failed: {err}"),
    }
}

fn run_identification_demo(
    plant_order: usize,
    noise_level: f64,
    observer_order: usize,
    retained_order: usize,
) -> Result<IdentificationDemo, String> {
    let dt = 0.2;
    let effective_plant_order = plant_order.clamp(2, 8);
    let system = planted_identification_system(effective_plant_order, dt)?;

    let n_samples = 140;
    let inputs = Mat::from_fn(1, n_samples, |_, col| excitation_signal(col));
    let x0 = vec![0.0; effective_plant_order];
    let sim = system
        .simulate(&x0, inputs.as_ref())
        .map_err(|err| err.to_string())?;
    let noisy_outputs = Mat::from_fn(1, n_samples, |_, col| {
        sim.outputs[(0, col)] + noise_level * measurement_noise_signal(col)
    });

    let okid_result = okid(
        noisy_outputs.as_ref(),
        inputs.as_ref(),
        &OkidParams::new(24, observer_order),
    )
    .map_err(|err| err.to_string())?;
    let requested_order = retained_order.max(1);
    let era_params = EraParams::new(system.sample_time()).with_order(requested_order);
    let era = match era_from_markov(&okid_result.markov, 6, 6, &era_params) {
        Ok(result) => result,
        Err(EraError::InvalidOrder { available, .. }) => era_from_markov(
            &okid_result.markov,
            6,
            6,
            &EraParams::new(system.sample_time()).with_order(available.max(1)),
        )
        .map_err(|err| err.to_string())?,
        Err(err) => return Err(err.to_string()),
    };

    let lags = (0..12).map(|idx| idx as f64).collect::<Vec<_>>();
    let planted_markov = (0..12)
        .map(|idx| system.markov_parameters(12).block(idx)[(0, 0)])
        .collect::<Vec<_>>();
    let identified_markov = (0..12)
        .map(|idx| okid_result.markov.block(idx)[(0, 0)])
        .collect::<Vec<_>>();

    let planted_step_response = system.step_response(30);
    let identified_step_response = era.realized.step_response(30);
    let steps = (0..30).map(|idx| idx as f64).collect::<Vec<_>>();
    let planted_step = planted_step_response
        .values
        .iter()
        .map(|block| block[(0, 0)])
        .collect::<Vec<_>>();
    let identified_step = identified_step_response
        .values
        .iter()
        .map(|block| block[(0, 0)])
        .collect::<Vec<_>>();
    let sample_rate = 1.0 / dt;
    let response_frequencies = logspace(
        (1.0e-6 * sample_rate).log10(),
        (sample_rate * core::f64::consts::PI * 0.98).log10(),
        260,
    );
    let planted_bode = system
        .bode_data(&response_frequencies)
        .map_err(|err| err.to_string())?;
    let identified_bode = era
        .realized
        .bode_data(&response_frequencies)
        .map_err(|err| err.to_string())?;
    let step_error = identified_step
        .iter()
        .zip(&planted_step)
        .map(|(identified, planted)| identified - planted)
        .collect::<Vec<_>>();
    let rms_step_error = (planted_step
        .iter()
        .zip(&identified_step)
        .map(|(lhs, rhs)| {
            let err = lhs - rhs;
            err * err
        })
        .sum::<f64>()
        / (planted_step.len() as f64))
        .sqrt();

    Ok(IdentificationDemo {
        lags,
        planted_markov,
        identified_markov,
        steps,
        planted_step,
        identified_step,
        step_error,
        bode_frequency_over_fs: planted_bode
            .angular_frequencies
            .iter()
            .map(|omega| *omega / sample_rate)
            .collect(),
        planted_bode_magnitude_db: planted_bode.magnitude_db,
        identified_bode_magnitude_db: identified_bode.magnitude_db,
        planted_bode_phase_deg: planted_bode.phase_deg,
        identified_bode_phase_deg: identified_bode.phase_deg,
        plant_order: effective_plant_order,
        requested_order,
        retained_order: era.retained_order,
        rms_step_error,
    })
}

fn planted_identification_system(
    plant_order: usize,
    dt: f64,
) -> Result<DiscreteStateSpace<f64>, String> {
    let order = plant_order.clamp(2, 8);
    let real_modes = [0.87, 0.72, -0.58, 0.49, -0.34, 0.28];
    let b_weights = [0.18, 0.10, 0.24, -0.21, 0.16, -0.13, 0.11, -0.09];
    let c_weights = [1.0, -0.62, 0.88, 0.56, -0.47, 0.39, -0.31, 0.25];

    DiscreteStateSpace::new(
        Mat::from_fn(order, order, |row, col| {
            if row == 0 && col == 0 {
                0.92
            } else if row == 0 && col == 1 {
                0.24
            } else if row == 1 && col == 0 {
                -0.24
            } else if row == 1 && col == 1 {
                0.92
            } else if row >= 2 && row == col {
                real_modes[row - 2]
            } else if col >= 2 && row < col {
                let distance = (col - row) as f64;
                let sign = if (row + col) % 2 == 0 { 1.0 } else { -1.0 };
                sign * (0.18 - 0.025 * distance).max(0.03)
            } else {
                0.0
            }
        }),
        Mat::from_fn(order, 1, |row, _| b_weights[row]),
        Mat::from_fn(1, order, |_, col| c_weights[col]),
        Mat::zeros(1, 1),
        dt,
    )
    .map_err(|err| err.to_string())
}

fn excitation_signal(step: usize) -> f64 {
    let k = step as f64;
    let square = if (step / 10).is_multiple_of(2) {
        1.0
    } else {
        -0.65
    };
    square + 0.35 * (0.19 * k).sin() + 0.18 * (0.07 * k + 0.4).cos()
}

fn measurement_noise_signal(step: usize) -> f64 {
    let k = step as f64;
    0.7 * (0.37 * k + 0.2).sin() + 0.25 * (0.11 * k + 0.8).cos()
}

#[cfg(test)]
mod tests {
    use super::run_identification_demo;

    #[test]
    fn identification_demo_includes_frequency_response_comparison() {
        let demo = run_identification_demo(4, 0.03, 6, 2).expect("identification demo should run");

        assert_eq!(
            demo.bode_frequency_over_fs.len(),
            demo.planted_bode_magnitude_db.len()
        );
        assert_eq!(
            demo.bode_frequency_over_fs.len(),
            demo.identified_bode_magnitude_db.len()
        );
        assert_eq!(
            demo.bode_frequency_over_fs.len(),
            demo.planted_bode_phase_deg.len()
        );
        assert_eq!(
            demo.bode_frequency_over_fs.len(),
            demo.identified_bode_phase_deg.len()
        );
        assert!(
            !demo.bode_frequency_over_fs.is_empty(),
            "expected bode samples in identification demo"
        );
    }
}
