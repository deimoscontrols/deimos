use crate::plot_helpers::{
    LineSeries, build_line_plot, build_matrix_heatmap_plot, matrix_grid_from_fn,
};
use crate::plotly_support::use_plotly_chart;
use crate::timing::measure;
use faer::Mat;
use leptos::prelude::*;
use numerical::control::lti::ContinuousStateSpace;
use numerical::control::reduction::{HsvdParams, hsvd_from_dense_gramians};
use numerical::decomp::{
    DenseDecompParams, dense_eigenvalues, dense_self_adjoint_eigen, dense_svd,
};
use plotly::Plot;

/// Interactive Gramian and HSVD exploration page.
#[component]
pub fn GramianHsvdPage() -> impl IntoView {
    let (plant_order, set_plant_order) = signal(6_usize);
    let (input_skew, set_input_skew) = signal(1.6_f64);
    let (output_skew, set_output_skew) = signal(1.3_f64);
    let (coupling, set_coupling) = signal(0.35_f64);
    let (auto_sigma_tol, set_auto_sigma_tol) = signal(true);
    let (sigma_tol_log10, set_sigma_tol_log10) = signal(-10.0_f64);

    let inputs = move || GramianInputs {
        plant_order: plant_order.get(),
        input_skew: input_skew.get(),
        output_skew: output_skew.get(),
        coupling: coupling.get(),
        auto_sigma_tol: auto_sigma_tol.get(),
        sigma_tol: 10.0_f64.powf(sigma_tol_log10.get()),
    };
    let demo = Memo::new(move |_| run_gramian_hsvd_demo(inputs()));

    use_plotly_chart("gramian-spectrum-plot", move || {
        build_gramian_plot(demo.get(), GramianPlot::GramianSpectra)
    });
    use_plotly_chart("hsvd-spectrum-plot", move || {
        build_gramian_plot(demo.get(), GramianPlot::HsvdVsSvd)
    });
    use_plotly_chart("gramian-a-matrix-plot", move || {
        build_gramian_plot(demo.get(), GramianPlot::StateMatrix)
    });
    use_plotly_chart("gramian-wc-matrix-plot", move || {
        build_gramian_plot(demo.get(), GramianPlot::ControllabilityMatrix)
    });
    use_plotly_chart("gramian-wo-matrix-plot", move || {
        build_gramian_plot(demo.get(), GramianPlot::ObservabilityMatrix)
    });

    let summary = move || gramian_hsvd_summary(demo.get());

    view! {
        <div class="page">
            <header class="page-header">
                <p class="eyebrow">"Linear Algebra"</p>
                <h1>"Gramians and HSVD"</h1>
                <p>
                    "A configurable stable continuous-time system is analyzed through its controllability"
                    " and observability Gramians. The Hankel singular values from HSVD are then compared"
                    " against the singular values and eigenvalues of the state matrix A."
                </p>
            </header>

            <div class="control-layout">
                <aside class="control-card">
                    <section>
                        <h2>"Plant shaping"</h2>
                        <p class="section-copy">
                            "Input and output skew change how energy enters and leaves the state basis."
                            " Coupling controls how strongly neighboring states interact inside A."
                        </p>

                        <div class="control-row">
                            <label for="gramian-order">"Plant states"</label>
                            <output>{move || plant_order.get().to_string()}</output>
                            <input
                                id="gramian-order"
                                type="range"
                                min="3"
                                max="10"
                                step="1"
                                prop:value=move || plant_order.get().to_string()
                                on:input=move |ev| {
                                    if let Ok(value) = event_target_value(&ev).parse::<usize>() {
                                        set_plant_order.set(value.clamp(3, 10));
                                    }
                                }
                            />
                        </div>

                        <div class="control-row">
                            <label for="gramian-input-skew">"Input skew"</label>
                            <output>{move || format!("{:.2}", input_skew.get())}</output>
                            <input
                                id="gramian-input-skew"
                                type="range"
                                min="0.6"
                                max="2.8"
                                step="0.05"
                                prop:value=move || input_skew.get().to_string()
                                on:input=move |ev| {
                                    if let Ok(value) = event_target_value(&ev).parse::<f64>() {
                                        set_input_skew.set(value.clamp(0.6, 2.8));
                                    }
                                }
                            />
                        </div>

                        <div class="control-row">
                            <label for="gramian-output-skew">"Output skew"</label>
                            <output>{move || format!("{:.2}", output_skew.get())}</output>
                            <input
                                id="gramian-output-skew"
                                type="range"
                                min="0.6"
                                max="2.8"
                                step="0.05"
                                prop:value=move || output_skew.get().to_string()
                                on:input=move |ev| {
                                    if let Ok(value) = event_target_value(&ev).parse::<f64>() {
                                        set_output_skew.set(value.clamp(0.6, 2.8));
                                    }
                                }
                            />
                        </div>

                        <div class="control-row">
                            <label for="gramian-coupling">"State coupling"</label>
                            <output>{move || format!("{:.2}", coupling.get())}</output>
                            <input
                                id="gramian-coupling"
                                type="range"
                                min="0.10"
                                max="0.80"
                                step="0.02"
                                prop:value=move || coupling.get().to_string()
                                on:input=move |ev| {
                                    if let Ok(value) = event_target_value(&ev).parse::<f64>() {
                                        set_coupling.set(value.clamp(0.10, 0.80));
                                    }
                                }
                            />
                        </div>

                        <div class="control-row checkbox-row">
                            <label for="gramian-auto-sigma-tol">"Auto HSVD sigma tol"</label>
                            <input
                                id="gramian-auto-sigma-tol"
                                type="checkbox"
                                prop:checked=move || auto_sigma_tol.get()
                                on:change=move |ev| set_auto_sigma_tol.set(event_target_checked(&ev))
                            />
                        </div>

                        <div class="control-row">
                            <label for="gramian-sigma-tol">"HSVD sigma tol"</label>
                            <output>
                                {move || {
                                    if auto_sigma_tol.get() {
                                        "auto".to_string()
                                    } else {
                                        format!("{:.1e}", 10.0_f64.powf(sigma_tol_log10.get()))
                                    }
                                }}
                            </output>
                            <input
                                id="gramian-sigma-tol"
                                type="range"
                                min="-14"
                                max="-2"
                                step="0.25"
                                prop:value=move || sigma_tol_log10.get().to_string()
                                prop:disabled=move || auto_sigma_tol.get()
                                on:input=move |ev| {
                                    if let Ok(value) = event_target_value(&ev).parse::<f64>() {
                                        set_sigma_tol_log10.set(value.clamp(-14.0, -2.0));
                                    }
                                }
                            />
                        </div>
                    </section>

                    <section>
                        <h2>"Interpretation"</h2>
                        <p class="section-copy">
                            "The Gramian spectra show how controllability and observability energy are distributed."
                            " The HSVD spectrum combines both views. Comparing it to the singular values of A"
                            " makes the point that balanced importance is not the same thing as a naive matrix SVD."
                            " The sigma tolerance can also truncate numerically tiny HSVs before they are reported."
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
                                <h2>"Gramian and HSVD spectra"</h2>
                                <p>"Energy-distribution and balanced-importance views for the same planted system."</p>
                            </div>
                        </div>
                        <div class="plot-subsection">
                            <div class="plot-header">
                                <div>
                                    <h2>"Gramian spectra"</h2>
                                    <p>"Log10 eigenvalue spectra of the controllability and observability Gramians."</p>
                                </div>
                            </div>
                            <div id="gramian-spectrum-plot" class="plot-surface"></div>
                        </div>

                        <div class="plot-subsection">
                            <div class="plot-header">
                                <div>
                                    <h2>"HSVD versus plain SVD"</h2>
                                    <p>"Log10 Hankel singular values compared with the singular values and eigenvalue magnitudes of A."</p>
                                </div>
                            </div>
                            <div id="hsvd-spectrum-plot" class="plot-surface"></div>
                        </div>
                    </article>

                    <div class="plots-grid compact">
                        <article class="plot-card">
                            <div class="plot-header">
                                <div>
                                    <h2>"State matrix A"</h2>
                                    <p>"The planted stable dynamics matrix whose naive SVD is compared against HSVD."</p>
                                </div>
                            </div>
                            <div id="gramian-a-matrix-plot" class="plot-surface"></div>
                        </article>

                        <article class="plot-card">
                            <div class="plot-header">
                                <div>
                                    <h2>"Controllability Gramian"</h2>
                                    <p>"Dense solution of `A Wc + Wc A^T + B B^T = 0`."</p>
                                </div>
                            </div>
                            <div id="gramian-wc-matrix-plot" class="plot-surface"></div>
                        </article>

                        <article class="plot-card">
                            <div class="plot-header">
                                <div>
                                    <h2>"Observability Gramian"</h2>
                                    <p>"Dense solution of `A^T Wo + Wo A + C^T C = 0`."</p>
                                </div>
                            </div>
                            <div id="gramian-wo-matrix-plot" class="plot-surface"></div>
                        </article>
                    </div>
                </div>
            </div>
        </div>
    }
}

#[derive(Clone, Copy)]
enum GramianPlot {
    GramianSpectra,
    HsvdVsSvd,
    StateMatrix,
    ControllabilityMatrix,
    ObservabilityMatrix,
}

#[derive(Clone, Copy)]
struct GramianInputs {
    plant_order: usize,
    input_skew: f64,
    output_skew: f64,
    coupling: f64,
    auto_sigma_tol: bool,
    sigma_tol: f64,
}

#[derive(Clone, PartialEq)]
struct GramianHsvdDemo {
    state_index: Vec<f64>,
    controllability_log10: Vec<f64>,
    observability_log10: Vec<f64>,
    hsv_log10: Vec<f64>,
    a_svd_log10: Vec<f64>,
    a_eig_log10: Vec<f64>,
    controllability_residual_norm: f64,
    observability_residual_norm: f64,
    controllability_ms: f64,
    observability_ms: f64,
    hsvd_ms: f64,
    svd_ms: f64,
    eig_ms: f64,
    sigma_tol: Option<f64>,
    a_matrix: Vec<Vec<f64>>,
    wc_matrix: Vec<Vec<f64>>,
    wo_matrix: Vec<Vec<f64>>,
}

fn build_gramian_plot(result: Result<GramianHsvdDemo, String>, which: GramianPlot) -> Plot {
    match result {
        Ok(demo) => match which {
            GramianPlot::GramianSpectra => build_line_plot(
                "Gramian spectra",
                "state index",
                "log10 spectrum",
                false,
                vec![
                    LineSeries::lines_markers(
                        "controllability",
                        demo.state_index.clone(),
                        demo.controllability_log10,
                    ),
                    LineSeries::lines_markers(
                        "observability",
                        demo.state_index.clone(),
                        demo.observability_log10,
                    ),
                ],
            ),
            GramianPlot::HsvdVsSvd => build_line_plot(
                "HSVD versus SVD(A) and |eig(A)|",
                "state index",
                "log10 spectrum",
                false,
                vec![
                    LineSeries::lines_markers("HSV", demo.state_index.clone(), demo.hsv_log10),
                    LineSeries::lines_markers(
                        "sigma(A)",
                        demo.state_index.clone(),
                        demo.a_svd_log10,
                    ),
                    LineSeries::lines_markers("|lambda(A)|", demo.state_index, demo.a_eig_log10),
                ],
            ),
            GramianPlot::StateMatrix => {
                build_matrix_heatmap_plot("State matrix A", demo.a_matrix, true)
            }
            GramianPlot::ControllabilityMatrix => {
                build_matrix_heatmap_plot("Controllability Gramian", demo.wc_matrix, true)
            }
            GramianPlot::ObservabilityMatrix => {
                build_matrix_heatmap_plot("Observability Gramian", demo.wo_matrix, true)
            }
        },
        Err(message) => build_line_plot(&message, "", "", false, Vec::new()),
    }
}

fn gramian_hsvd_summary(result: Result<GramianHsvdDemo, String>) -> String {
    match result {
        Ok(demo) => format!(
            "Controllability Gramian solved in {:.2} ms with residual {:.2e}; observability Gramian solved in {:.2} ms with residual {:.2e}. HSVD took {:.2} ms{}; SVD(A) took {:.2} ms; |eig(A)| took {:.2} ms.",
            demo.controllability_ms,
            demo.controllability_residual_norm,
            demo.observability_ms,
            demo.observability_residual_norm,
            demo.hsvd_ms,
            match demo.sigma_tol {
                Some(sigma_tol) => format!(" with sigma tol {:.1e}", sigma_tol),
                None => String::from(" with automatic sigma tolerance"),
            },
            demo.svd_ms,
            demo.eig_ms,
        ),
        Err(message) => format!("Gramian / HSVD analysis failed: {message}"),
    }
}

fn run_gramian_hsvd_demo(inputs: GramianInputs) -> Result<GramianHsvdDemo, String> {
    let system = planted_gramian_system(
        inputs.plant_order.clamp(3, 10),
        inputs.input_skew.clamp(0.6, 2.8),
        inputs.output_skew.clamp(0.6, 2.8),
        inputs.coupling.clamp(0.10, 0.80),
    )?;
    let decomp_params = DenseDecompParams::<f64>::new();

    let hsvd_params = if inputs.auto_sigma_tol {
        HsvdParams::new()
    } else {
        HsvdParams::new().with_sigma_tol(inputs.sigma_tol.clamp(1.0e-14, 1.0e-2))
    };

    let (wc, controllability_ms) = measure(|| {
        system
            .controllability_gramian()
            .map_err(|err| err.to_string())
    });
    let wc = wc?;

    let (wo, observability_ms) = measure(|| {
        system
            .observability_gramian()
            .map_err(|err| err.to_string())
    });
    let wo = wo?;

    let (hsvd, hsvd_ms) = measure(|| {
        hsvd_from_dense_gramians(wc.solution.as_ref(), wo.solution.as_ref(), &hsvd_params)
            .map_err(|err| err.to_string())
    });
    let hsvd = hsvd?;

    let (a_svd, svd_ms) =
        measure(|| dense_svd(system.a(), &decomp_params).map_err(|err| err.to_string()));
    let a_svd = a_svd?;
    let (a_eig, eig_ms) = measure(|| dense_eigenvalues(system.a()).map_err(|err| err.to_string()));
    let a_eig = a_eig?;

    let wc_eig = dense_self_adjoint_eigen(wc.solution.as_ref(), &decomp_params)
        .map_err(|err| err.to_string())?;
    let wo_eig = dense_self_adjoint_eigen(wo.solution.as_ref(), &decomp_params)
        .map_err(|err| err.to_string())?;
    let n = hsvd
        .hankel_singular_values
        .nrows()
        .max(a_svd.s.nrows())
        .max(a_eig.nrows());
    let state_index = (0..n).map(|i| (i + 1) as f64).collect::<Vec<_>>();

    Ok(GramianHsvdDemo {
        state_index,
        controllability_log10: spectral_log10_values(
            (0..wc_eig.values.nrows()).map(|i| wc_eig.values[i].abs()),
        ),
        observability_log10: spectral_log10_values(
            (0..wo_eig.values.nrows()).map(|i| wo_eig.values[i].abs()),
        ),
        hsv_log10: spectral_log10_values(
            (0..hsvd.hankel_singular_values.nrows()).map(|i| hsvd.hankel_singular_values[i].abs()),
        ),
        a_svd_log10: spectral_log10_values((0..a_svd.s.nrows()).map(|i| a_svd.s[i].abs())),
        a_eig_log10: spectral_log10_values(
            (0..a_eig.nrows()).map(|i| a_eig[i].re.hypot(a_eig[i].im)),
        ),
        controllability_residual_norm: wc.residual_norm,
        observability_residual_norm: wo.residual_norm,
        controllability_ms,
        observability_ms,
        hsvd_ms,
        svd_ms,
        eig_ms,
        sigma_tol: (!inputs.auto_sigma_tol).then_some(inputs.sigma_tol.clamp(1.0e-14, 1.0e-2)),
        a_matrix: matrix_grid_from_fn(system.nstates(), system.nstates(), |row, col| {
            system.a()[(row, col)]
        }),
        wc_matrix: matrix_grid_from_fn(wc.solution.nrows(), wc.solution.ncols(), |row, col| {
            wc.solution[(row, col)]
        }),
        wo_matrix: matrix_grid_from_fn(wo.solution.nrows(), wo.solution.ncols(), |row, col| {
            wo.solution[(row, col)]
        }),
    })
}

fn planted_gramian_system(
    order: usize,
    input_skew: f64,
    output_skew: f64,
    coupling: f64,
) -> Result<ContinuousStateSpace<f64>, String> {
    let a = Mat::from_fn(order, order, |row, col| {
        if row == col {
            -(0.8 + 0.35 * row as f64)
        } else if row + 1 == col {
            coupling * (0.8 + 0.08 * row as f64)
        } else if col + 1 == row {
            -0.45 * coupling * (0.6 + 0.05 * col as f64)
        } else if row + 2 == col {
            0.18 * coupling
        } else {
            0.0
        }
    });
    let denom = ((order - 1).max(1)) as f64;
    let b = Mat::from_fn(order, 2, |row, col| {
        let xi = row as f64 / denom;
        match col {
            0 => input_skew.powf(-xi),
            _ => 0.35 + 0.65 * xi,
        }
    });
    let c = Mat::from_fn(2, order, |row, col| {
        let xi = col as f64 / denom;
        match row {
            0 => output_skew.powf(-(1.0 - xi)),
            _ => 0.2 + 0.5 * xi + if col % 2 == 0 { 0.12 } else { -0.08 },
        }
    });
    let d = Mat::<f64>::zeros(2, 2);
    ContinuousStateSpace::new(a, b, c, d).map_err(|err| err.to_string())
}

fn spectral_log10_values(values: impl Iterator<Item = f64>) -> Vec<f64> {
    values.map(safe_log10).collect()
}

fn safe_log10(value: f64) -> f64 {
    value.max(1.0e-16).log10()
}

#[cfg(test)]
mod tests {
    use super::{GramianInputs, run_gramian_hsvd_demo};

    #[test]
    fn gramian_hsvd_demo_runs() {
        let demo = run_gramian_hsvd_demo(GramianInputs {
            plant_order: 6,
            input_skew: 1.6,
            output_skew: 1.3,
            coupling: 0.35,
            auto_sigma_tol: true,
            sigma_tol: 1.0e-10,
        })
        .unwrap();
        assert!(!demo.controllability_log10.is_empty());
        assert_eq!(
            demo.controllability_log10.len(),
            demo.observability_log10.len()
        );
        assert!(!demo.a_eig_log10.is_empty());
    }
}
