use crate::demo_signal::gaussianish_signal;
use crate::plot_helpers::{
    LineSeries, build_complex_plane_plot, build_line_plot, build_matrix_heatmap_plot,
    matrix_grid_from_fn,
};
use crate::plotly_support::use_plotly_chart;
use crate::timing::measure;
use faer::Mat;
use leptos::prelude::*;
use numerical::decomp::{DenseDecompParams, dense_eigen, dense_self_adjoint_eigen, dense_svd};
use plotly::Plot;

/// Interactive dense spectral page comparing eigen and singular structure.
#[component]
pub fn DenseSpectralDecompositionPage() -> impl IntoView {
    let (matrix_family, set_matrix_family) = signal(DenseSpectralFamily::SelfAdjoint);
    let (dimension, set_dimension) = signal(8_usize);
    let (coupling, set_coupling) = signal(0.35_f64);
    let (departure_from_normality, set_departure_from_normality) = signal(0.65_f64);
    let (use_partial, set_use_partial) = signal(true);
    let (components, set_components) = signal(4_usize);
    let (tol_log10, set_tol_log10) = signal(-8.0_f64);

    let inputs = move || DenseSpectralInputs {
        matrix_family: matrix_family.get(),
        dimension: dimension.get(),
        coupling: coupling.get(),
        departure_from_normality: departure_from_normality.get(),
        use_partial: use_partial.get(),
        components: components.get(),
        tolerance: 10.0_f64.powf(tol_log10.get()),
    };
    let demo = Memo::new(move |_| run_dense_spectral_demo(inputs()));

    use_plotly_chart("dense-spectral-spectrum-plot", move || {
        build_dense_spectral_plot(demo.get(), DenseSpectralPlot::Spectrum)
    });
    use_plotly_chart("dense-spectral-eigen-map-plot", move || {
        build_dense_spectral_plot(demo.get(), DenseSpectralPlot::EigenMap)
    });
    use_plotly_chart("dense-spectral-matrix-plot", move || {
        build_dense_spectral_plot(demo.get(), DenseSpectralPlot::Matrix)
    });

    let summary = move || dense_spectral_summary(demo.get());

    view! {
        <div class="page">
            <header class="page-header">
                <p class="eyebrow">"Linear Algebra"</p>
                <h1>"Dense Eigen + SVD"</h1>
                <p>
                    "Compare the dense eigendecomposition and dense SVD on self-adjoint and non-normal matrices."
                    " The self-adjoint family gives the baseline where singular values and eigenvalue magnitudes line"
                    " up; the non-normal family shows how quickly those views diverge."
                </p>
            </header>

            <div class="control-layout">
                <aside class="control-card">
                    <section>
                        <h2>"Matrix family"</h2>
                        <p class="section-copy">
                            "The self-adjoint matrix uses symmetric couplings only. The non-normal matrix adds an"
                            " antisymmetric and upper-biased component, which can move the eigenvalues away from the"
                            " singular-value picture while keeping the same general scale."
                        </p>

                        <div class="control-row">
                            <label for="dense-spectral-family">"Matrix family"</label>
                            <select
                                id="dense-spectral-family"
                                on:change=move |ev| {
                                    set_matrix_family
                                        .set(DenseSpectralFamily::from_form_value(&event_target_value(&ev)));
                                }
                            >
                                <option
                                    value="self_adjoint"
                                    selected=move || matrix_family.get() == DenseSpectralFamily::SelfAdjoint
                                >
                                    "Self-adjoint"
                                </option>
                                <option
                                    value="non_normal"
                                    selected=move || matrix_family.get() == DenseSpectralFamily::NonNormal
                                >
                                    "Non-normal"
                                </option>
                            </select>
                        </div>

                        <div class="control-row">
                            <label for="dense-spectral-dimension">"Dimension"</label>
                            <output>{move || dimension.get().to_string()}</output>
                            <input
                                id="dense-spectral-dimension"
                                type="range"
                                min="3"
                                max="18"
                                step="1"
                                prop:value=move || dimension.get().to_string()
                                on:input=move |ev| {
                                    if let Ok(value) = event_target_value(&ev).parse::<usize>() {
                                        set_dimension.set(value.clamp(3, 18));
                                    }
                                }
                            />
                        </div>

                        <div class="control-row">
                            <label for="dense-spectral-coupling">"Coupling"</label>
                            <output>{move || format!("{:.2}", coupling.get())}</output>
                            <input
                                id="dense-spectral-coupling"
                                type="range"
                                min="0.05"
                                max="0.80"
                                step="0.02"
                                prop:value=move || coupling.get().to_string()
                                on:input=move |ev| {
                                    if let Ok(value) = event_target_value(&ev).parse::<f64>() {
                                        set_coupling.set(value.clamp(0.05, 0.80));
                                    }
                                }
                            />
                        </div>

                        <div class="control-row">
                            <label for="dense-spectral-departure">"Departure from normality"</label>
                            <output>{move || format!("{:.2}", departure_from_normality.get())}</output>
                            <input
                                id="dense-spectral-departure"
                                type="range"
                                min="0.0"
                                max="1.2"
                                step="0.05"
                                prop:value=move || departure_from_normality.get().to_string()
                                prop:disabled=move || matrix_family.get() == DenseSpectralFamily::SelfAdjoint
                                on:input=move |ev| {
                                    if let Ok(value) = event_target_value(&ev).parse::<f64>() {
                                        set_departure_from_normality.set(value.clamp(0.0, 1.2));
                                    }
                                }
                            />
                        </div>
                    </section>

                    <section>
                        <h2>"Decomposition mode"</h2>
                        <p class="section-copy">
                            "Dense decomposition can either run in full mode or request only the leading dominant"
                            " components through the partial backend. The same setting is applied to both the eigen"
                            " and SVD paths so the timing and convergence diagnostics stay comparable."
                        </p>

                        <div class="control-row checkbox-row">
                            <label for="dense-spectral-partial">"Use partial decomposition"</label>
                            <input
                                id="dense-spectral-partial"
                                type="checkbox"
                                prop:checked=move || use_partial.get()
                                on:change=move |ev| set_use_partial.set(event_target_checked(&ev))
                            />
                        </div>

                        <div class="control-row">
                            <label for="dense-spectral-components">"Leading components"</label>
                            <output>{move || components.get().to_string()}</output>
                            <input
                                id="dense-spectral-components"
                                type="range"
                                min="1"
                                max="12"
                                step="1"
                                prop:value=move || components.get().to_string()
                                prop:disabled=move || !use_partial.get()
                                on:input=move |ev| {
                                    if let Ok(value) = event_target_value(&ev).parse::<usize>() {
                                        set_components.set(value.clamp(1, 12));
                                    }
                                }
                            />
                        </div>

                        <div class="control-row">
                            <label for="dense-spectral-tolerance">"Tolerance"</label>
                            <output>{move || format!("{:.1e}", 10.0_f64.powf(tol_log10.get()))}</output>
                            <input
                                id="dense-spectral-tolerance"
                                type="range"
                                min="-12.0"
                                max="-3.0"
                                step="0.25"
                                prop:value=move || tol_log10.get().to_string()
                                prop:disabled=move || !use_partial.get()
                                on:input=move |ev| {
                                    if let Ok(value) = event_target_value(&ev).parse::<f64>() {
                                        set_tol_log10.set(value.clamp(-12.0, -3.0));
                                    }
                                }
                            />
                        </div>
                    </section>

                    <section>
                        <h2>"Interpretation"</h2>
                        <p class="section-copy">
                            "Singular values measure operator amplification; eigenvalues describe invariant directions."
                            " On self-adjoint matrices those stories line up cleanly. On non-normal matrices they can"
                            " drift apart even when the matrix entries still look modest in the heatmap."
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
                                <h2>"Spectral traces"</h2>
                                <p>"Magnitude and location views of the same dense decomposition run."</p>
                            </div>
                        </div>
                        <div class="plot-subsection">
                            <div class="plot-header">
                                <div>
                                    <h2>"Spectrum magnitude"</h2>
                                    <p>"Log10 singular values compared with log10 eigenvalue magnitudes."</p>
                                </div>
                            </div>
                            <div id="dense-spectral-spectrum-plot" class="plot-surface"></div>
                        </div>

                        <div class="plot-subsection">
                            <div class="plot-header">
                                <div>
                                    <h2>"Eigenvalue map"</h2>
                                    <p>"Eigenvalues plotted in the complex plane for the same matrix."</p>
                                </div>
                            </div>
                            <div id="dense-spectral-eigen-map-plot" class="plot-surface"></div>
                        </div>
                    </article>

                    <article class="plot-card">
                        <div class="plot-header">
                            <div>
                                <h2>"Matrix heatmap"</h2>
                                <p>"The dense matrix whose eigendecomposition and SVD are being compared."</p>
                            </div>
                        </div>
                        <div id="dense-spectral-matrix-plot" class="plot-surface"></div>
                    </article>
                </div>
            </div>
        </div>
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DenseSpectralFamily {
    SelfAdjoint,
    NonNormal,
}

impl DenseSpectralFamily {
    fn from_form_value(value: &str) -> Self {
        match value {
            "non_normal" => Self::NonNormal,
            _ => Self::SelfAdjoint,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::SelfAdjoint => "self-adjoint",
            Self::NonNormal => "non-normal",
        }
    }
}

#[derive(Clone, Copy)]
struct DenseSpectralInputs {
    matrix_family: DenseSpectralFamily,
    dimension: usize,
    coupling: f64,
    departure_from_normality: f64,
    use_partial: bool,
    components: usize,
    tolerance: f64,
}

#[derive(Clone, PartialEq)]
struct DenseSpectralDemo {
    spectrum_index: Vec<f64>,
    singular_log10: Vec<f64>,
    eigen_abs_log10: Vec<f64>,
    eigen_re: Vec<f64>,
    eigen_im: Vec<f64>,
    matrix_values: Vec<Vec<f64>>,
    matrix_family: DenseSpectralFamily,
    eigen_ms: f64,
    svd_ms: f64,
    eigen_requested: usize,
    eigen_converged: usize,
    eigen_residual: f64,
    eigen_orthogonality: f64,
    svd_requested: usize,
    svd_converged: usize,
    svd_residual: f64,
    svd_orthogonality: f64,
    partial_mode: bool,
}

#[derive(Clone, Copy)]
enum DenseSpectralPlot {
    Spectrum,
    EigenMap,
    Matrix,
}

fn build_dense_spectral_plot(
    result: Result<DenseSpectralDemo, String>,
    which: DenseSpectralPlot,
) -> Plot {
    match result {
        Ok(demo) => match which {
            DenseSpectralPlot::Spectrum => build_line_plot(
                "Dense spectrum comparison",
                "component index",
                "log10 magnitude",
                false,
                vec![
                    LineSeries::lines_markers(
                        "singular values",
                        demo.spectrum_index.clone(),
                        demo.singular_log10,
                    ),
                    LineSeries::lines_markers(
                        "|eigenvalues|",
                        demo.spectrum_index,
                        demo.eigen_abs_log10,
                    ),
                ],
            ),
            DenseSpectralPlot::EigenMap => build_complex_plane_plot(
                "Dense eigenvalue map",
                vec![LineSeries::markers(
                    "eigenvalues",
                    demo.eigen_re,
                    demo.eigen_im,
                )],
            ),
            DenseSpectralPlot::Matrix => {
                build_matrix_heatmap_plot("Dense matrix", demo.matrix_values, true)
            }
        },
        Err(message) => build_line_plot(&message, "", "", false, Vec::new()),
    }
}

fn dense_spectral_summary(result: Result<DenseSpectralDemo, String>) -> String {
    match result {
        Ok(demo) => format!(
            "{} matrix, {} mode. Eigen: converged {}/{} in {:.3} ms with residual {:.2e} and orthogonality error {:.2e}. SVD: converged {}/{} in {:.3} ms with residual {:.2e} and orthogonality error {:.2e}.",
            demo.matrix_family.label(),
            if demo.partial_mode { "partial" } else { "full" },
            demo.eigen_converged,
            demo.eigen_requested,
            demo.eigen_ms,
            demo.eigen_residual,
            demo.eigen_orthogonality,
            demo.svd_converged,
            demo.svd_requested,
            demo.svd_ms,
            demo.svd_residual,
            demo.svd_orthogonality,
        ),
        Err(err) => format!("Dense spectral demo failed: {err}"),
    }
}

fn run_dense_spectral_demo(inputs: DenseSpectralInputs) -> Result<DenseSpectralDemo, String> {
    let n = inputs.dimension.clamp(3, 18);
    let matrix = build_dense_matrix(inputs, n);
    let matrix_values = matrix_grid_from_fn(n, n, |row, col| matrix[(row, col)]);
    let params = dense_params(inputs, n);

    let (eigen_payload, eigen_ms) = match inputs.matrix_family {
        DenseSpectralFamily::SelfAdjoint => {
            let (eig, elapsed) = measure(|| {
                dense_self_adjoint_eigen(matrix.as_ref(), &params).map_err(|err| err.to_string())
            });
            let eig = eig?;
            (
                EigenPayload {
                    abs_log10: eig
                        .values
                        .iter()
                        .map(|value| safe_log10(value.abs()))
                        .collect(),
                    re: eig.values.iter().copied().collect(),
                    im: vec![0.0; eig.values.nrows()],
                    requested: eig.info.n_requested,
                    converged: eig.info.n_converged,
                    residual: eig.info.max_residual_norm,
                    orthogonality: eig.info.max_orthogonality_error,
                },
                elapsed,
            )
        }
        DenseSpectralFamily::NonNormal => {
            let (eig, elapsed) =
                measure(|| dense_eigen(matrix.as_ref(), &params).map_err(|err| err.to_string()));
            let eig = eig?;
            (
                EigenPayload {
                    abs_log10: eig
                        .values
                        .iter()
                        .map(|value| safe_log10(value.re.hypot(value.im)))
                        .collect(),
                    re: eig.values.iter().map(|value| value.re).collect(),
                    im: eig.values.iter().map(|value| value.im).collect(),
                    requested: eig.info.n_requested,
                    converged: eig.info.n_converged,
                    residual: eig.info.max_residual_norm,
                    orthogonality: eig.info.max_orthogonality_error,
                },
                elapsed,
            )
        }
    };

    let (svd_result, svd_ms) =
        measure(|| dense_svd(matrix.as_ref(), &params).map_err(|err| err.to_string()));
    let svd = svd_result?;
    let spectrum_len = eigen_payload.abs_log10.len().max(svd.s.nrows());

    Ok(DenseSpectralDemo {
        spectrum_index: (0..spectrum_len).map(|idx| (idx + 1) as f64).collect(),
        singular_log10: svd.s.iter().map(|value| safe_log10(value.abs())).collect(),
        eigen_abs_log10: eigen_payload.abs_log10,
        eigen_re: eigen_payload.re,
        eigen_im: eigen_payload.im,
        matrix_values,
        matrix_family: inputs.matrix_family,
        eigen_ms,
        svd_ms,
        eigen_requested: eigen_payload.requested,
        eigen_converged: eigen_payload.converged,
        eigen_residual: eigen_payload.residual,
        eigen_orthogonality: eigen_payload.orthogonality,
        svd_requested: svd.info.n_requested,
        svd_converged: svd.info.n_converged,
        svd_residual: svd.info.max_residual_norm,
        svd_orthogonality: svd.info.max_orthogonality_error,
        partial_mode: inputs.use_partial,
    })
}

struct EigenPayload {
    abs_log10: Vec<f64>,
    re: Vec<f64>,
    im: Vec<f64>,
    requested: usize,
    converged: usize,
    residual: f64,
    orthogonality: f64,
}

fn dense_params(inputs: DenseSpectralInputs, n: usize) -> DenseDecompParams<f64> {
    let mut params = DenseDecompParams::<f64>::new().with_tol(inputs.tolerance);
    if inputs.use_partial {
        params = params.with_n_components(Some(inputs.components.clamp(1, n)));
    }
    params
}

fn build_dense_matrix(inputs: DenseSpectralInputs, n: usize) -> Mat<f64> {
    let coupling = inputs.coupling.clamp(0.05, 0.80);
    let departure = inputs.departure_from_normality.clamp(0.0, 1.2);
    Mat::from_fn(n, n, |row, col| {
        let symmetric = if row == col {
            2.4 - 0.18 * row as f64 + 0.15 * gaussianish_signal(row, 11)
        } else {
            let distance = row.abs_diff(col) as f64;
            let envelope = coupling / (distance + 0.8);
            let texture = 0.65 + 0.12 * gaussianish_signal(row + col, 23);
            envelope * texture
        };

        match inputs.matrix_family {
            DenseSpectralFamily::SelfAdjoint => symmetric,
            DenseSpectralFamily::NonNormal => {
                let antisymmetric = if row < col {
                    departure * coupling * (0.35 + 0.05 * gaussianish_signal(row * n + col, 41))
                } else if row > col {
                    -departure * coupling * (0.20 + 0.05 * gaussianish_signal(col * n + row, 43))
                } else {
                    0.0
                };
                let upper_bias = if col == row + 2 {
                    0.18 * departure
                } else if row == col + 2 {
                    -0.04 * departure
                } else {
                    0.0
                };
                symmetric + antisymmetric + upper_bias
            }
        }
    })
}

fn safe_log10(value: f64) -> f64 {
    value.max(1.0e-16).log10()
}

#[cfg(test)]
mod tests {
    use super::{DenseSpectralFamily, DenseSpectralInputs, run_dense_spectral_demo};

    #[test]
    fn dense_spectral_demo_runs_for_both_families() {
        for matrix_family in [
            DenseSpectralFamily::SelfAdjoint,
            DenseSpectralFamily::NonNormal,
        ] {
            let demo = run_dense_spectral_demo(DenseSpectralInputs {
                matrix_family,
                dimension: 8,
                coupling: 0.35,
                departure_from_normality: 0.65,
                use_partial: true,
                components: 4,
                tolerance: 1.0e-8,
            })
            .unwrap();
            assert!(!demo.singular_log10.is_empty());
            assert_eq!(demo.matrix_values.len(), 8);
        }
    }
}
