use crate::plot_helpers::{
    LineSeries, build_line_plot, build_matrix_heatmap_plot, linspace, logspace, matrix_grid_from_fn,
};
use crate::plotly_support::use_plotly_chart;
use faer::Mat;
use leptos::prelude::*;
use numerical::control::lti::ContinuousStateSpace;
use numerical::control::reduction::BalancedParams;
use plotly::Plot;

/// Interactive balanced-truncation page comparing full and reduced models.
#[component]
pub fn ReductionPage() -> impl IntoView {
    let (plant_order, set_plant_order) = signal(6_usize);
    let (retained_order, set_retained_order) = signal(3_usize);
    let demo = Memo::new(move |_| run_reduction_demo(plant_order.get(), retained_order.get()));

    use_plotly_chart("reduction-step-plot", move || {
        build_reduction_plot(demo.get(), ReductionPlot::StepResponse)
    });
    use_plotly_chart("reduction-bode-mag-plot", move || {
        build_reduction_plot(demo.get(), ReductionPlot::BodeMagnitude)
    });
    use_plotly_chart("reduction-bode-phase-plot", move || {
        build_reduction_plot(demo.get(), ReductionPlot::BodePhase)
    });
    use_plotly_chart("reduction-hsv-plot", move || {
        build_reduction_plot(demo.get(), ReductionPlot::Hsv)
    });
    use_plotly_chart("reduction-full-a-plot", move || {
        build_reduction_plot(demo.get(), ReductionPlot::FullStateMatrix)
    });
    use_plotly_chart("reduction-reduced-a-plot", move || {
        build_reduction_plot(demo.get(), ReductionPlot::ReducedStateMatrix)
    });

    let summary = move || reduction_summary(demo.get());

    view! {
        <div class="page">
            <header class="page-header">
                <p class="eyebrow">"Reduction"</p>
                <h1>"Balanced Truncation Explorer"</h1>
                <p>
                    "A configurable stable continuous model is reduced with dense balanced truncation. The first plot"
                    " overlays the full and reduced step responses, and the second shows the Hankel singular value"
                    " spectrum being truncated as the planted order and retained order change."
                </p>
            </header>

            <div class="control-layout">
                <aside class="control-card">
                    <section>
                        <h2>"Model order"</h2>
                        <p class="section-copy">
                            "Plant order controls how many dynamic modes are present in the planted continuous-time"
                            " system. Retained order controls how many balanced directions survive truncation."
                        </p>

                        <div class="control-row">
                            <label for="reduction-plant-order">"Plant states"</label>
                            <output>{move || plant_order.get().to_string()}</output>
                            <input
                                id="reduction-plant-order"
                                type="range"
                                min="3"
                                max="8"
                                step="1"
                                prop:value=move || plant_order.get().to_string()
                                on:input=move |ev| {
                                    if let Ok(value) = event_target_value(&ev).parse::<usize>() {
                                        set_plant_order.set(value.clamp(3, 8));
                                    }
                                }
                            />
                        </div>

                        <p class="section-copy">
                            "Balanced truncation ranks states by joint controllability and observability energy."
                            " Moving the retained-order slider changes how many of those balanced directions are kept."
                        </p>

                        <div class="control-row">
                            <label for="reduction-order">"Retained states"</label>
                            <output>{move || retained_order.get().to_string()}</output>
                            <input
                                id="reduction-order"
                                type="range"
                                min="1"
                                max="8"
                                step="1"
                                prop:value=move || retained_order.get().to_string()
                                on:input=move |ev| {
                                    if let Ok(value) = event_target_value(&ev).parse::<usize>() {
                                        set_retained_order.set(value.clamp(1, 8));
                                    }
                                }
                            />
                        </div>
                    </section>

                    <section>
                        <h2>"Interpretation"</h2>
                        <p class="section-copy">
                            "The HSV spectrum tells you how much balanced energy each state direction contributes."
                            " When the tail decays quickly, low-order truncations preserve the dominant step-response"
                            " shape with little visible distortion."
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
                                <h2>"Balanced truncation traces"</h2>
                                <p>"Response and HSV views for the same full-versus-reduced model comparison."</p>
                            </div>
                        </div>
                        <div class="plot-subsection">
                            <div class="plot-header">
                                <div>
                                    <h2>"Step response"</h2>
                                    <p>"Full model versus balanced-truncated model."</p>
                                </div>
                            </div>
                            <div id="reduction-step-plot" class="plot-surface"></div>
                        </div>

                        <div class="plot-subsection">
                            <div class="plot-header">
                                <div>
                                    <h2>"Hankel singular values"</h2>
                                    <p>"Balanced energy spectrum for the same model."</p>
                                </div>
                            </div>
                            <div id="reduction-hsv-plot" class="plot-surface"></div>
                        </div>

                        <div class="plot-subsection">
                            <div class="plot-header">
                                <div>
                                    <h2>"Bode magnitude"</h2>
                                    <p>"First-channel magnitude comparison between the full and reduced models."</p>
                                </div>
                            </div>
                            <div id="reduction-bode-mag-plot" class="plot-surface"></div>
                        </div>

                        <div class="plot-subsection">
                            <div class="plot-header">
                                <div>
                                    <h2>"Bode phase"</h2>
                                    <p>"First-channel phase comparison for the same full-versus-reduced model."</p>
                                </div>
                            </div>
                            <div id="reduction-bode-phase-plot" class="plot-surface"></div>
                        </div>
                    </article>

                    <div class="plots-grid two-up">
                        <article class="plot-card">
                            <div class="plot-header">
                                <div>
                                    <h2>"Full state matrix"</h2>
                                    <p>"The planted continuous-time `A` matrix before reduction."</p>
                                </div>
                            </div>
                            <div id="reduction-full-a-plot" class="plot-surface"></div>
                        </article>

                        <article class="plot-card">
                            <div class="plot-header">
                                <div>
                                    <h2>"Reduced state matrix"</h2>
                                    <p>"The balanced-truncated `A_r` matrix after retaining the selected order."</p>
                                </div>
                            </div>
                            <div id="reduction-reduced-a-plot" class="plot-surface"></div>
                        </article>
                    </div>
                </div>
            </div>
        </div>
    }
}

#[derive(Clone, Copy)]
enum ReductionPlot {
    StepResponse,
    Hsv,
    BodeMagnitude,
    BodePhase,
    FullStateMatrix,
    ReducedStateMatrix,
}

#[derive(Clone, PartialEq)]
struct ReductionDemo {
    times: Vec<f64>,
    full_step: Vec<f64>,
    reduced_step: Vec<f64>,
    hsv_indices: Vec<f64>,
    hsv_values: Vec<f64>,
    bode_frequencies: Vec<f64>,
    full_bode_magnitude_db: Vec<f64>,
    reduced_bode_magnitude_db: Vec<f64>,
    full_bode_phase_deg: Vec<f64>,
    reduced_bode_phase_deg: Vec<f64>,
    plant_order: usize,
    requested_order: usize,
    retained_order: usize,
    error_bound: Option<f64>,
    rms_step_error: f64,
    final_output_error: f64,
    full_a_matrix: Vec<Vec<f64>>,
    reduced_a_matrix: Vec<Vec<f64>>,
}

fn build_reduction_plot(result: Result<ReductionDemo, String>, which: ReductionPlot) -> Plot {
    match result {
        Ok(demo) => match which {
            ReductionPlot::StepResponse => build_line_plot(
                "Balanced truncation step response",
                "time (s)",
                "output",
                false,
                vec![
                    LineSeries::lines("full model", demo.times.clone(), demo.full_step),
                    LineSeries::lines("reduced model", demo.times, demo.reduced_step),
                ],
            ),
            ReductionPlot::Hsv => build_line_plot(
                "Hankel singular values",
                "state index",
                "sigma",
                false,
                vec![LineSeries::lines_markers(
                    format!("retain r = {}", demo.retained_order),
                    demo.hsv_indices,
                    demo.hsv_values,
                )],
            ),
            ReductionPlot::BodeMagnitude => build_line_plot(
                "Balanced truncation magnitude response",
                "angular frequency",
                "magnitude (dB)",
                true,
                vec![
                    LineSeries::lines(
                        "full model",
                        demo.bode_frequencies.clone(),
                        demo.full_bode_magnitude_db,
                    ),
                    LineSeries::lines(
                        "reduced model",
                        demo.bode_frequencies,
                        demo.reduced_bode_magnitude_db,
                    ),
                ],
            ),
            ReductionPlot::BodePhase => build_line_plot(
                "Balanced truncation phase response",
                "angular frequency",
                "phase (deg)",
                true,
                vec![
                    LineSeries::lines(
                        "full model",
                        demo.bode_frequencies.clone(),
                        demo.full_bode_phase_deg,
                    ),
                    LineSeries::lines(
                        "reduced model",
                        demo.bode_frequencies,
                        demo.reduced_bode_phase_deg,
                    ),
                ],
            ),
            ReductionPlot::FullStateMatrix => {
                build_matrix_heatmap_plot("Full state matrix A", demo.full_a_matrix, true)
            }
            ReductionPlot::ReducedStateMatrix => {
                build_matrix_heatmap_plot("Reduced state matrix Ar", demo.reduced_a_matrix, true)
            }
        },
        Err(message) => build_line_plot(&message, "", "", false, Vec::new()),
    }
}

fn reduction_summary(result: Result<ReductionDemo, String>) -> String {
    match result {
        Ok(demo) => match demo.error_bound {
            Some(bound) => format!(
                "Plant order {} with retained order {}{} has balanced-truncation tail bound {:.4}. Sampled step-response RMS error is {:.4}; final sampled error is {:.4}.",
                demo.plant_order,
                demo.retained_order,
                if demo.requested_order == demo.retained_order {
                    String::new()
                } else {
                    format!(
                        " (requested {}, capped at plant order)",
                        demo.requested_order
                    )
                },
                bound,
                demo.rms_step_error,
                demo.final_output_error,
            ),
            None => format!(
                "Plant order {} with retained order {}{} has sampled step-response RMS error {:.4}; final sampled error is {:.4}.",
                demo.plant_order,
                demo.retained_order,
                if demo.requested_order == demo.retained_order {
                    String::new()
                } else {
                    format!(
                        " (requested {}, capped at plant order)",
                        demo.requested_order
                    )
                },
                demo.rms_step_error,
                demo.final_output_error,
            ),
        },
        Err(err) => format!("Reduction failed: {err}"),
    }
}

fn run_reduction_demo(plant_order: usize, retained_order: usize) -> Result<ReductionDemo, String> {
    let effective_plant_order = plant_order.clamp(3, 8);
    let effective_retained_order = retained_order.clamp(1, effective_plant_order);
    let system = planted_reduction_system(effective_plant_order)?;

    let result = system
        .balanced_truncation(&BalancedParams::new().with_order(effective_retained_order))
        .map_err(|err| err.to_string())?;
    let sample_times = linspace(0.0, 28.0, 260);
    let full_step_response = system
        .step_response(&sample_times)
        .map_err(|err| err.to_string())?;
    let reduced_step_response = result
        .reduced
        .step_response(&sample_times)
        .map_err(|err| err.to_string())?;
    let full_bode_system = extract_continuous_siso_channel(&system, 0, 0)?;
    let reduced_bode_system = extract_continuous_siso_channel(&result.reduced, 0, 0)?;
    let bode_frequencies = logspace(-3.0, 1.1, 260);
    let full_bode = full_bode_system
        .bode_data(&bode_frequencies)
        .map_err(|err| err.to_string())?;
    let reduced_bode = reduced_bode_system
        .bode_data(&bode_frequencies)
        .map_err(|err| err.to_string())?;

    let full_step = full_step_response
        .values
        .iter()
        .map(|block| block[(0, 0)])
        .collect::<Vec<_>>();
    let reduced_step = reduced_step_response
        .values
        .iter()
        .map(|block| block[(0, 0)])
        .collect::<Vec<_>>();
    let final_output_error = (full_step.last().copied().unwrap_or(0.0_f64)
        - reduced_step.last().copied().unwrap_or(0.0_f64))
    .abs();
    let rms_step_error = (full_step
        .iter()
        .zip(&reduced_step)
        .map(|(full, reduced)| {
            let err = full - reduced;
            err * err
        })
        .sum::<f64>()
        / (full_step.len() as f64))
        .sqrt();

    let hsv_values = (0..result.hankel_singular_values.nrows())
        .map(|idx| result.hankel_singular_values[idx])
        .collect::<Vec<_>>();
    let hsv_indices = (0..hsv_values.len())
        .map(|idx| (idx + 1) as f64)
        .collect::<Vec<_>>();

    Ok(ReductionDemo {
        times: sample_times,
        full_step,
        reduced_step,
        hsv_indices,
        hsv_values,
        bode_frequencies: full_bode.angular_frequencies,
        full_bode_magnitude_db: full_bode.magnitude_db,
        reduced_bode_magnitude_db: reduced_bode.magnitude_db,
        full_bode_phase_deg: full_bode.phase_deg,
        reduced_bode_phase_deg: reduced_bode.phase_deg,
        plant_order: effective_plant_order,
        requested_order: retained_order,
        retained_order: result.reduced_order,
        error_bound: result.error_bound,
        rms_step_error,
        final_output_error,
        full_a_matrix: matrix_grid_from_fn(system.a().nrows(), system.a().ncols(), |row, col| {
            system.a()[(row, col)]
        }),
        reduced_a_matrix: matrix_grid_from_fn(
            result.reduced.a().nrows(),
            result.reduced.a().ncols(),
            |row, col| result.reduced.a()[(row, col)],
        ),
    })
}

fn extract_continuous_siso_channel(
    system: &ContinuousStateSpace<f64>,
    output: usize,
    input: usize,
) -> Result<ContinuousStateSpace<f64>, String> {
    ContinuousStateSpace::new(
        Mat::from_fn(system.a().nrows(), system.a().ncols(), |row, col| {
            system.a()[(row, col)]
        }),
        Mat::from_fn(system.b().nrows(), 1, |row, _| system.b()[(row, input)]),
        Mat::from_fn(1, system.c().ncols(), |_, col| system.c()[(output, col)]),
        Mat::from_fn(1, 1, |_, _| system.d()[(output, input)]),
    )
    .map_err(|err| err.to_string())
}

fn planted_reduction_system(order: usize) -> Result<ContinuousStateSpace<f64>, String> {
    let effective_order = order.clamp(3, 8);
    let pole_rates = [0.16, 0.22, 0.30, 0.39, 0.50, 0.64, 0.81, 1.01];

    ContinuousStateSpace::new(
        Mat::from_fn(effective_order, effective_order, |row, col| {
            if row == col {
                -pole_rates[row]
            } else if row < col {
                let distance = (col - row) as f64;
                let phase = ((row + 1) * 17 + (col + 1) * 13) as f64;
                0.12 * phase.sin() / distance.sqrt()
            } else {
                0.0
            }
        }),
        Mat::from_fn(effective_order, 3, |row, input| {
            let phase = (row + 1) as f64;
            let base = pole_rates[row].powf(0.85);
            let taper = 0.82 + 0.34 * (row as f64) / ((effective_order - 1) as f64);
            match input {
                0 => {
                    let amplitude = 1.0 + 0.18 * (1.73 * phase).sin() + 0.10 * (0.91 * phase).cos();
                    let sign = if row % 2 == 0 { 1.0 } else { -1.0 };
                    sign * base * taper * amplitude
                }
                1 => {
                    let amplitude =
                        0.85 + 0.16 * (1.11 * phase).cos() - 0.09 * (0.57 * phase).sin();
                    let sign = if row % 3 == 0 { -1.0 } else { 1.0 };
                    sign * base * taper * amplitude
                }
                _ => {
                    let amplitude =
                        0.92 + 0.14 * (0.67 * phase).sin() + 0.11 * (1.29 * phase).cos();
                    let sign = if row % 4 <= 1 { 1.0 } else { -1.0 };
                    sign * base * taper * amplitude
                }
            }
        }),
        Mat::from_fn(3, effective_order, |output, col| {
            let phase = (col + 1) as f64;
            let base = pole_rates[col].powf(0.85);
            let taper = 0.84 + 0.30 * (col as f64) / ((effective_order - 1) as f64);
            match output {
                0 => {
                    let amplitude = 1.0 + 0.16 * (0.63 * phase).cos() - 0.12 * (1.37 * phase).sin();
                    let sign = if col % 3 == 1 { -1.0 } else { 1.0 };
                    sign * base * taper * amplitude
                }
                1 => {
                    let amplitude =
                        0.90 + 0.14 * (0.88 * phase).sin() + 0.08 * (1.41 * phase).cos();
                    let sign = if col % 2 == 0 { -1.0 } else { 1.0 };
                    sign * base * taper * amplitude
                }
                _ => {
                    let amplitude =
                        0.96 + 0.12 * (0.52 * phase).cos() - 0.10 * (1.17 * phase).sin();
                    let sign = if col % 4 <= 1 { 1.0 } else { -1.0 };
                    sign * base * taper * amplitude
                }
            }
        }),
        Mat::zeros(3, 3),
    )
    .map_err(|err| err.to_string())
}

#[cfg(test)]
mod tests {
    use super::{planted_reduction_system, run_reduction_demo};
    use numerical::control::reduction::BalancedParams;

    #[test]
    fn planted_reduction_system_has_richer_hsv_spectrum() {
        let system = planted_reduction_system(8).expect("plant should build");
        let result = system
            .balanced_truncation(&BalancedParams::new())
            .expect("balanced truncation should succeed");
        let sigma0 = result.hankel_singular_values[0];
        let significant = (0..result.hankel_singular_values.nrows())
            .filter(|&idx| result.hankel_singular_values[idx] >= 0.04 * sigma0)
            .count();
        let order_two = run_reduction_demo(8, 2).expect("order-two reduction should run");
        let order_five = run_reduction_demo(8, 5).expect("order-five reduction should run");

        assert!(
            significant >= 5,
            "expected at least five nontrivial Hankel singular values, got {significant} from {:?}",
            (0..result.hankel_singular_values.nrows())
                .map(|idx| result.hankel_singular_values[idx])
                .collect::<Vec<_>>()
        );
        assert!(
            order_five.rms_step_error < 0.7 * order_two.rms_step_error,
            "expected retained order 5 to reduce step RMS error materially: order 2 = {}, order 5 = {}",
            order_two.rms_step_error,
            order_five.rms_step_error,
        );
    }

    #[test]
    fn reduction_demo_includes_frequency_response_comparison() {
        let demo = run_reduction_demo(6, 3).expect("reduction demo should run");

        assert_eq!(
            demo.bode_frequencies.len(),
            demo.full_bode_magnitude_db.len()
        );
        assert_eq!(
            demo.bode_frequencies.len(),
            demo.reduced_bode_magnitude_db.len()
        );
        assert_eq!(demo.bode_frequencies.len(), demo.full_bode_phase_deg.len());
        assert_eq!(
            demo.bode_frequencies.len(),
            demo.reduced_bode_phase_deg.len()
        );
        assert!(!demo.bode_frequencies.is_empty(), "expected bode samples");
    }
}
