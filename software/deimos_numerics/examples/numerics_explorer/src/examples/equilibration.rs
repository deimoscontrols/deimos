use crate::demo_signal::gaussianish_signal;
use crate::plot_helpers::{LineSeries, build_line_plot, build_sparse_pattern_plot};
use crate::plotly_support::use_plotly_chart;
use crate::timing::measure;
use faer::sparse::{SparseColMat, Triplet};
use leptos::prelude::*;
use numerical::sparse::{
    BiCGSTAB, DiagonalPrecond, Equilibration, EquilibrationParams, Precond, SparseMatVec,
};
use plotly::Plot;

/// Interactive sparse equilibration page comparing unscaled and equilibrated Krylov solves.
#[component]
pub fn EquilibrationPage() -> impl IntoView {
    let (dimension, set_dimension) = signal(180_usize);
    let (matrix_structure, set_matrix_structure) = signal(MatrixStructure::Coupled);
    let (scale_spread_decades, set_scale_spread_decades) = signal(6.0_f64);
    let (coupling, set_coupling) = signal(0.22_f64);
    let (random_sparsity_percent, set_random_sparsity_percent) = signal(0.40_f64);
    let (equilibration_iterations, set_equilibration_iterations) = signal(8_usize);
    let (tol_log10, set_tol_log10) = signal(-6.0_f64);
    let (max_iterations, set_max_iterations) = signal(50_usize);
    let (preconditioner, set_preconditioner) = signal(BicgPreconditioner::None);

    let inputs = move || EquilibrationInputs {
        dimension: dimension.get(),
        matrix_structure: matrix_structure.get(),
        scale_spread_decades: scale_spread_decades.get(),
        coupling: coupling.get(),
        random_sparsity_percent: random_sparsity_percent.get(),
        equilibration_iterations: equilibration_iterations.get(),
        tolerance: 10.0_f64.powf(tol_log10.get()),
        max_iterations: max_iterations.get(),
        preconditioner: preconditioner.get(),
    };
    let demo = Memo::new(move |_| run_equilibration_demo(inputs()));

    use_plotly_chart("equilibration-residual-plot", move || {
        build_equilibration_plot(demo.get(), EquilibrationPlot::ResidualHistory)
    });
    use_plotly_chart("equilibration-scales-plot", move || {
        build_equilibration_plot(demo.get(), EquilibrationPlot::ScaleFactors)
    });
    use_plotly_chart("equilibration-matrix-plot", move || {
        build_equilibration_plot(demo.get(), EquilibrationPlot::MatrixPattern)
    });

    let summary = move || equilibration_summary(demo.get());

    view! {
        <div class="page">
            <header class="page-header">
                <p class="eyebrow">"Linear Algebra"</p>
                <h1>"Equilibration"</h1>
                <p>
                    "This page applies the crate's two-sided Ruiz-style sparse equilibration to a deliberately"
                    " badly scaled system, then compares BiCGSTAB before and after scaling."
                </p>
            </header>

            <div class="control-layout">
                <aside class="control-card">
                    <section>
                        <h2>"System scaling"</h2>
                        <p class="section-copy">
                            "Choose a matrix family, then apply independent deterministic row and column scaling"
                            " profiles to make the raw linear system numerically awkward before equilibration is"
                            " applied."
                        </p>

                        <div class="control-row">
                            <label for="equilibration-structure">"Matrix structure"</label>
                            <select
                                id="equilibration-structure"
                                on:change=move |ev| {
                                    set_matrix_structure
                                        .set(MatrixStructure::from_form_value(&event_target_value(&ev)));
                                }
                            >
                                <option
                                    value="coupled"
                                    selected=move || matrix_structure.get() == MatrixStructure::Coupled
                                >
                                    "Coupled nonsymmetric"
                                </option>
                                <option
                                    value="tridiagonal"
                                    selected=move || matrix_structure.get() == MatrixStructure::Tridiagonal
                                >
                                    "Tridiagonal"
                                </option>
                                <option
                                    value="random_sparse"
                                    selected=move || matrix_structure.get() == MatrixStructure::RandomSparse
                                >
                                    "Random sparse"
                                </option>
                            </select>
                        </div>

                        <div class="control-row">
                            <label for="equilibration-dimension">"Dimension"</label>
                            <output>{move || dimension.get().to_string()}</output>
                            <input
                                id="equilibration-dimension"
                                type="range"
                                min="20"
                                max="1200"
                                step="20"
                                prop:value=move || dimension.get().to_string()
                                on:input=move |ev| {
                                    if let Ok(value) = event_target_value(&ev).parse::<usize>() {
                                        set_dimension.set(value.clamp(20, 1200));
                                    }
                                }
                            />
                        </div>

                        <div class="control-row">
                            <label for="equilibration-spread">"Scale spread"</label>
                            <output>{move || format!("{:.1} decades", scale_spread_decades.get())}</output>
                            <input
                                id="equilibration-spread"
                                type="range"
                                min="0.0"
                                max="14.0"
                                step="0.25"
                                prop:value=move || scale_spread_decades.get().to_string()
                                on:input=move |ev| {
                                    if let Ok(value) = event_target_value(&ev).parse::<f64>() {
                                        set_scale_spread_decades.set(value.clamp(0.0, 14.0));
                                    }
                                }
                            />
                        </div>

                        <div class="control-row">
                            <label for="equilibration-coupling">"Off-diagonal coupling"</label>
                            <output>{move || format!("{:.2}", coupling.get())}</output>
                            <input
                                id="equilibration-coupling"
                                type="range"
                                min="0.05"
                                max="0.45"
                                step="0.01"
                                prop:value=move || coupling.get().to_string()
                                prop:disabled=move || matrix_structure.get() != MatrixStructure::Coupled
                                on:input=move |ev| {
                                    if let Ok(value) = event_target_value(&ev).parse::<f64>() {
                                        set_coupling.set(value.clamp(0.05, 0.45));
                                    }
                                }
                            />
                        </div>

                        <div class="control-row">
                            <label for="equilibration-random-sparsity">"Random sparsity"</label>
                            <output>{move || format!("{:.2}%", random_sparsity_percent.get())}</output>
                            <input
                                id="equilibration-random-sparsity"
                                type="range"
                                min="0.05"
                                max="1.50"
                                step="0.05"
                                prop:value=move || random_sparsity_percent.get().to_string()
                                prop:disabled=move || matrix_structure.get() != MatrixStructure::RandomSparse
                                on:input=move |ev| {
                                    if let Ok(value) = event_target_value(&ev).parse::<f64>() {
                                        set_random_sparsity_percent.set(value.clamp(0.05, 1.50));
                                    }
                                }
                            />
                        </div>
                    </section>

                    <section>
                        <h2>"Equilibration"</h2>
                        <p class="section-copy">
                            "This controls only the preprocessing pass that builds the two-sided scaling factors,"
                            " not the Krylov iteration limit."
                        </p>

                        <div class="control-row">
                            <label for="equilibration-iters">"Max equilibration iterations"</label>
                            <output>{move || equilibration_iterations.get().to_string()}</output>
                            <input
                                id="equilibration-iters"
                                type="range"
                                min="1"
                                max="20"
                                step="1"
                                prop:value=move || equilibration_iterations.get().to_string()
                                on:input=move |ev| {
                                    if let Ok(value) = event_target_value(&ev).parse::<usize>() {
                                        set_equilibration_iterations.set(value.clamp(1, 20));
                                    }
                                }
                            />
                        </div>
                    </section>

                    <section>
                        <h2>"BiCGSTAB"</h2>
                        <p class="section-copy">
                            "Both solves use the same zero initial guess and the same stopping threshold measured"
                            " against the original `A x = b` system. Only the internal matrix, right-hand side,"
                            " and unknown coordinates differ after equilibration. The same preconditioner choice"
                            " is applied to both runs."
                        </p>

                        <div class="control-row">
                            <label for="equilibration-tolerance">"Tolerance"</label>
                            <output>{move || format!("{:.1e}", 10.0_f64.powf(tol_log10.get()))}</output>
                            <input
                                id="equilibration-tolerance"
                                type="range"
                                min="-10.0"
                                max="-3.0"
                                step="0.25"
                                prop:value=move || tol_log10.get().to_string()
                                on:input=move |ev| {
                                    if let Ok(value) = event_target_value(&ev).parse::<f64>() {
                                        set_tol_log10.set(value.clamp(-10.0, -3.0));
                                    }
                                }
                            />
                        </div>

                        <div class="control-row">
                            <label for="equilibration-max-iters">"Max iterations"</label>
                            <output>{move || max_iterations.get().to_string()}</output>
                            <input
                                id="equilibration-max-iters"
                                type="range"
                                min="20"
                                max="400"
                                step="10"
                                prop:value=move || max_iterations.get().to_string()
                                on:input=move |ev| {
                                    if let Ok(value) = event_target_value(&ev).parse::<usize>() {
                                        set_max_iterations.set(value.clamp(20, 400));
                                    }
                                }
                            />
                        </div>

                        <div class="control-row">
                            <label for="equilibration-preconditioner">"Preconditioner"</label>
                            <select
                                id="equilibration-preconditioner"
                                on:change=move |ev| {
                                    set_preconditioner
                                        .set(BicgPreconditioner::from_form_value(&event_target_value(&ev)));
                                }
                            >
                                <option
                                    value="none"
                                    selected=move || preconditioner.get() == BicgPreconditioner::None
                                >
                                    "None"
                                </option>
                                <option
                                    value="diagonal"
                                    selected=move || preconditioner.get() == BicgPreconditioner::Diagonal
                                >
                                    "Diagonal (Jacobi)"
                                </option>
                            </select>
                        </div>
                    </section>

                    <section>
                        <h2>"Interpretation"</h2>
                        <p class="section-copy">
                            "Equilibration does not change the true solution of `A x = b`. It changes the scaling of"
                            " the equations and unknown coordinates so the Krylov solve sees a better balanced operator."
                            " The row and column scale curves are the explicit `D_r` and `D_c` factors used internally,"
                            " and in this demo they are intentionally chosen from independent deterministic profiles"
                            " rather than reciprocal ramps."
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
                                <h2>"Equilibration traces"</h2>
                                <p>"Residual convergence and explicit row/column scaling for the same sparse system."</p>
                            </div>
                        </div>
                        <div class="plot-subsection">
                            <div class="plot-header">
                                <div>
                                    <h2>"Residual history"</h2>
                                    <p>"True residual norms in the original coordinates before and after applying the two-sided scaling."</p>
                                </div>
                            </div>
                            <div id="equilibration-residual-plot" class="plot-surface"></div>
                        </div>

                        <div class="plot-subsection">
                            <div class="plot-header">
                                <div>
                                    <h2>"Scale factors"</h2>
                                    <p>"Log10 row/column equilibration scales alongside Jacobi inverse-diagonal magnitudes."</p>
                                </div>
                            </div>
                            <div id="equilibration-scales-plot" class="plot-surface"></div>
                        </div>
                    </article>

                    <article class="plot-card">
                        <div class="plot-header">
                            <div>
                                <h2>"Matrix sparsity pattern"</h2>
                                <p>"The same badly scaled sparse operator that feeds both Krylov solves."</p>
                            </div>
                        </div>
                        <div id="equilibration-matrix-plot" class="plot-surface"></div>
                    </article>
                </div>
            </div>
        </div>
    }
}

#[derive(Clone, Copy)]
struct EquilibrationInputs {
    dimension: usize,
    matrix_structure: MatrixStructure,
    scale_spread_decades: f64,
    coupling: f64,
    random_sparsity_percent: f64,
    equilibration_iterations: usize,
    tolerance: f64,
    max_iterations: usize,
    preconditioner: BicgPreconditioner,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BicgPreconditioner {
    None,
    Diagonal,
}

impl BicgPreconditioner {
    fn from_form_value(value: &str) -> Self {
        match value {
            "diagonal" => Self::Diagonal,
            _ => Self::None,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Diagonal => "diagonal",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MatrixStructure {
    Coupled,
    Tridiagonal,
    RandomSparse,
}

impl MatrixStructure {
    fn from_form_value(value: &str) -> Self {
        match value {
            "tridiagonal" => Self::Tridiagonal,
            "random_sparse" => Self::RandomSparse,
            _ => Self::Coupled,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Coupled => "coupled nonsymmetric",
            Self::Tridiagonal => "tridiagonal",
            Self::RandomSparse => "random sparse",
        }
    }
}

#[derive(Clone, PartialEq)]
struct EquilibrationDemo {
    unscaled_iter_index: Vec<f64>,
    unscaled_log10_residual: Vec<f64>,
    scaled_iter_index: Vec<f64>,
    scaled_log10_residual: Vec<f64>,
    scale_index: Vec<f64>,
    row_scale_log10: Vec<f64>,
    col_scale_log10: Vec<f64>,
    unscaled_diag_precond_log10: Vec<f64>,
    scaled_diag_precond_log10: Vec<f64>,
    unscaled_iterations: usize,
    scaled_iterations: usize,
    unscaled_converged: bool,
    scaled_converged: bool,
    unscaled_relative_error: f64,
    scaled_relative_error: f64,
    unscaled_ms: f64,
    equilibration_ms: f64,
    scaled_ms: f64,
    matrix_structure: MatrixStructure,
    preconditioner: BicgPreconditioner,
    equilibration_iterations: usize,
    row_spread_before: f64,
    col_spread_before: f64,
    row_spread_after: f64,
    col_spread_after: f64,
    matrix_columns: Vec<f64>,
    matrix_rows: Vec<f64>,
    nrows: usize,
    ncols: usize,
}

#[derive(Clone, Copy)]
enum EquilibrationPlot {
    ResidualHistory,
    ScaleFactors,
    MatrixPattern,
}

fn build_equilibration_plot(
    result: Result<EquilibrationDemo, String>,
    which: EquilibrationPlot,
) -> Plot {
    match result {
        Ok(demo) => match which {
            EquilibrationPlot::ResidualHistory => build_line_plot(
                "True residual history",
                "iteration",
                "log10 ||b - A x||",
                false,
                vec![
                    LineSeries::lines(
                        "unscaled",
                        demo.unscaled_iter_index,
                        demo.unscaled_log10_residual,
                    ),
                    LineSeries::lines(
                        "equilibrated",
                        demo.scaled_iter_index,
                        demo.scaled_log10_residual,
                    ),
                ],
            ),
            EquilibrationPlot::ScaleFactors => build_line_plot(
                "Equilibration scales",
                "index",
                "log10 scale",
                false,
                vec![
                    LineSeries::lines("row scale", demo.scale_index.clone(), demo.row_scale_log10),
                    LineSeries::lines(
                        "column scale",
                        demo.scale_index.clone(),
                        demo.col_scale_log10,
                    ),
                    LineSeries::lines(
                        "|Jacobi inv diag(A)|",
                        demo.scale_index.clone(),
                        demo.unscaled_diag_precond_log10,
                    ),
                    LineSeries::lines(
                        "|Jacobi inv diag(A_eq)|",
                        demo.scale_index,
                        demo.scaled_diag_precond_log10,
                    ),
                ],
            ),
            EquilibrationPlot::MatrixPattern => build_sparse_pattern_plot(
                &format!(
                    "Badly scaled {} matrix pattern",
                    demo.matrix_structure.label()
                ),
                demo.nrows,
                demo.ncols,
                demo.matrix_columns,
                demo.matrix_rows,
            ),
        },
        Err(message) => build_line_plot(&message, "", "", false, Vec::new()),
    }
}

fn equilibration_summary(result: Result<EquilibrationDemo, String>) -> String {
    match result {
        Ok(demo) => format!(
            "On the {} matrix with {} preconditioning, row spread {:.2e} -> {:.2e}; column spread {:.2e} -> {:.2e}. Equilibration used up to {} preprocessing iterations. Unscaled BiCGSTAB {} after {} iterations with relative error {:.2e} in {:.3} ms. Equilibrated BiCGSTAB {} after {} iterations with relative error {:.2e} in {:.3} ms. Both iteration counts are based on the true residual in the original coordinates. Scaling analysis itself took {:.3} ms.",
            demo.matrix_structure.label(),
            demo.preconditioner.label(),
            demo.row_spread_before,
            demo.row_spread_after,
            demo.col_spread_before,
            demo.col_spread_after,
            demo.equilibration_iterations,
            if demo.unscaled_converged {
                "converged"
            } else {
                "stopped"
            },
            demo.unscaled_iterations,
            demo.unscaled_relative_error,
            demo.unscaled_ms,
            if demo.scaled_converged {
                "converged"
            } else {
                "stopped"
            },
            demo.scaled_iterations,
            demo.scaled_relative_error,
            demo.scaled_ms,
            demo.equilibration_ms,
        ),
        Err(err) => format!("Equilibration demo failed: {err}"),
    }
}

fn run_equilibration_demo(inputs: EquilibrationInputs) -> Result<EquilibrationDemo, String> {
    let matrix = build_badly_scaled_matrix(inputs)?;
    let n = matrix.nrows();
    let x_true = (0..n)
        .map(|idx| (0.13 * idx as f64).sin() + if idx % 2 == 0 { 1.0 } else { -0.8 })
        .collect::<Vec<_>>();
    let b = apply_matrix(&matrix, &x_true);

    let (row_spread_before, col_spread_before) = row_col_spread_csc(&matrix)?;
    let (matrix_columns, matrix_rows) = sparse_pattern_points(&matrix);
    let unscaled_diag_precond_log10 = diagonal_precond_log10(&matrix)?;

    let x0 = vec![0.0; n];
    let (unscaled_run, unscaled_ms) = measure(|| {
        run_bicgstab_history(
            &matrix,
            &x0,
            &b,
            &matrix,
            &b,
            None,
            inputs.tolerance,
            inputs.max_iterations,
            inputs.preconditioner,
        )
    });
    let unscaled_run = unscaled_run?;

    let equilibration_iterations = inputs.equilibration_iterations.clamp(1, 20);
    let (eq_result, equilibration_ms) = measure(|| {
        Equilibration::<f64>::compute_from_csc(
            matrix.as_ref(),
            EquilibrationParams {
                max_iters: equilibration_iterations,
                ..EquilibrationParams::default()
            },
        )
    });
    let eq = eq_result.map_err(|err| format!("{err:?}"))?;

    let mut scaled_matrix = matrix.clone();
    eq.scale_csc_matrix_in_place(&mut scaled_matrix);
    let (row_spread_after, col_spread_after) = row_col_spread_csc(&scaled_matrix)?;
    let scaled_diag_precond_log10 = diagonal_precond_log10(&scaled_matrix)?;

    let mut scaled_b = b.clone();
    eq.scale_rhs_in_place(&mut scaled_b);
    let mut scaled_x0 = x0.clone();
    eq.scale_initial_guess_in_place(&mut scaled_x0);

    let (scaled_run_result, scaled_ms) = measure(|| {
        run_bicgstab_history(
            &scaled_matrix,
            &scaled_x0,
            &scaled_b,
            &matrix,
            &b,
            Some(eq.col_scale()),
            inputs.tolerance,
            inputs.max_iterations,
            inputs.preconditioner,
        )
    });
    let scaled_run = scaled_run_result?;

    let scale_index = (0..n).map(|idx| idx as f64).collect::<Vec<_>>();
    Ok(EquilibrationDemo {
        unscaled_iter_index: unscaled_run.iter_index,
        unscaled_log10_residual: unscaled_run.log10_residual,
        scaled_iter_index: scaled_run.iter_index,
        scaled_log10_residual: scaled_run.log10_residual,
        scale_index,
        row_scale_log10: eq.row_scale().iter().copied().map(safe_log10).collect(),
        col_scale_log10: eq.col_scale().iter().copied().map(safe_log10).collect(),
        unscaled_diag_precond_log10,
        scaled_diag_precond_log10,
        unscaled_iterations: unscaled_run.iterations,
        scaled_iterations: scaled_run.iterations,
        unscaled_converged: unscaled_run.converged,
        scaled_converged: scaled_run.converged,
        unscaled_relative_error: relative_error(&unscaled_run.x, &x_true),
        scaled_relative_error: relative_error(&scaled_run.x, &x_true),
        unscaled_ms,
        equilibration_ms,
        scaled_ms,
        matrix_structure: inputs.matrix_structure,
        preconditioner: inputs.preconditioner,
        equilibration_iterations,
        row_spread_before,
        col_spread_before,
        row_spread_after,
        col_spread_after,
        matrix_columns,
        matrix_rows,
        nrows: matrix.nrows(),
        ncols: matrix.ncols(),
    })
}

#[derive(Clone)]
struct BicgRun {
    iter_index: Vec<f64>,
    log10_residual: Vec<f64>,
    iterations: usize,
    converged: bool,
    x: Vec<f64>,
}

fn run_bicgstab_history(
    matrix: &SparseColMat<usize, f64>,
    x0: &[f64],
    b: &[f64],
    original_matrix: &SparseColMat<usize, f64>,
    original_b: &[f64],
    col_unscale: Option<&[f64]>,
    tolerance: f64,
    max_iterations: usize,
    preconditioner: BicgPreconditioner,
) -> Result<BicgRun, String> {
    match preconditioner {
        BicgPreconditioner::None => {
            let solver = BiCGSTAB::new(matrix.as_ref(), x0, b).map_err(|err| err.to_string())?;
            finish_bicgstab_history(
                solver,
                original_matrix,
                original_b,
                col_unscale,
                tolerance,
                max_iterations,
            )
        }
        BicgPreconditioner::Diagonal => {
            let diagonal =
                DiagonalPrecond::try_from(matrix.as_ref()).map_err(|err| format!("{err:?}"))?;
            let solver = BiCGSTAB::new_with_precond(matrix.as_ref(), diagonal, x0, b)
                .map_err(|err| err.to_string())?;
            finish_bicgstab_history(
                solver,
                original_matrix,
                original_b,
                col_unscale,
                tolerance,
                max_iterations,
            )
        }
    }
}

fn finish_bicgstab_history<P>(
    mut solver: BiCGSTAB<f64, faer::sparse::SparseColMatRef<'_, usize, f64>, P>,
    original_matrix: &SparseColMat<usize, f64>,
    original_b: &[f64],
    col_unscale: Option<&[f64]>,
    tolerance: f64,
    max_iterations: usize,
) -> Result<BicgRun, String>
where
    P: Precond<f64>,
{
    let mut current_x = solver_x_in_original_coordinates(solver.x(), col_unscale);
    let mut true_residual = residual_norm(original_matrix, &current_x, original_b);
    let mut log10_residual = vec![safe_log10(true_residual)];

    if true_residual < tolerance {
        return Ok(BicgRun {
            iter_index: vec![0.0],
            log10_residual,
            iterations: 0,
            converged: true,
            x: current_x,
        });
    }

    for _ in 0..max_iterations {
        solver.step();
        current_x = solver_x_in_original_coordinates(solver.x(), col_unscale);
        true_residual = residual_norm(original_matrix, &current_x, original_b);
        log10_residual.push(safe_log10(true_residual));
        if true_residual < tolerance {
            break;
        }
    }

    let iterations = solver.iteration_count();
    Ok(BicgRun {
        iter_index: (0..log10_residual.len()).map(|idx| idx as f64).collect(),
        log10_residual,
        iterations,
        converged: true_residual < tolerance,
        x: current_x,
    })
}

fn solver_x_in_original_coordinates(x: &faer::Col<f64>, col_unscale: Option<&[f64]>) -> Vec<f64> {
    match col_unscale {
        Some(scales) => x
            .iter()
            .zip(scales.iter())
            .map(|(&value, &scale)| value * scale)
            .collect(),
        None => x.iter().copied().collect(),
    }
}

fn build_badly_scaled_matrix(
    inputs: EquilibrationInputs,
) -> Result<SparseColMat<usize, f64>, String> {
    let n = inputs.dimension.clamp(20, 1200);
    let spread = inputs.scale_spread_decades.clamp(0.0, 14.0);
    let coupling = inputs.coupling.clamp(0.05, 0.45);
    let random_sparsity_percent = inputs.random_sparsity_percent.clamp(0.05, 1.50);

    let mut triplets = match inputs.matrix_structure {
        MatrixStructure::Coupled => coupled_matrix_triplets(n, coupling),
        MatrixStructure::Tridiagonal => shifted_laplacian_triplets(n, 0.25),
        MatrixStructure::RandomSparse => random_sparse_triplets(n, 0.35, random_sparsity_percent),
    };
    apply_scale_spread_to_triplets(&mut triplets, n, n, spread);

    SparseColMat::<usize, f64>::try_new_from_triplets(n, n, &triplets)
        .map_err(|err| err.to_string())
}

fn coupled_matrix_triplets(n: usize, coupling: f64) -> Vec<Triplet<usize, usize, f64>> {
    let mut triplets = Vec::with_capacity(5 * n);
    for row in 0..n {
        for (col, base) in [
            (row.saturating_sub(1), if row > 0 { -coupling } else { 0.0 }),
            (row, 2.4 + 0.2 * gaussianish_signal(row, 0)),
            (row + 1, if row + 1 < n { -0.8 * coupling } else { 0.0 }),
        ] {
            if base == 0.0 || col >= n {
                continue;
            }
            triplets.push(Triplet::new(row, col, base));
        }

        let far_col = (row + n / 3 + 1) % n;
        let far_base = 0.12 * coupling * gaussianish_signal(row, 17);
        if far_col != row && far_base.abs() > 1.0e-12 {
            triplets.push(Triplet::new(row, far_col, far_base));
        }
    }
    triplets
}

fn shifted_laplacian_triplets(n: usize, shift: f64) -> Vec<Triplet<usize, usize, f64>> {
    let mut triplets = Vec::with_capacity(3 * n.saturating_sub(2) + 2);
    for row in 0..n {
        triplets.push(Triplet::new(row, row, 2.0 + shift));
        if row > 0 {
            triplets.push(Triplet::new(row, row - 1, -1.0));
        }
        if row + 1 < n {
            triplets.push(Triplet::new(row, row + 1, -1.0));
        }
    }
    triplets
}

fn random_sparse_triplets(
    n: usize,
    shift: f64,
    random_sparsity_percent: f64,
) -> Vec<Triplet<usize, usize, f64>> {
    let target_edges = ((random_sparsity_percent / 100.0) * n as f64).round() as usize;
    let random_edges = (3 + target_edges).clamp(3, 32);
    let mut triplets = Vec::with_capacity(n * (5 + random_edges));
    for row in 0..n {
        let mut cols = Vec::with_capacity(4 + random_edges);
        let mut vals = Vec::with_capacity(4 + random_edges);

        for &(offset, base_weight) in &[(-2_isize, -0.10_f64), (-1, -0.16), (1, -0.14), (2, -0.09)]
        {
            let col = row as isize + offset;
            if !(0..n as isize).contains(&col) {
                continue;
            }
            cols.push(col as usize);
            vals.push(
                base_weight
                    * (1.0 + 0.15 * gaussianish_signal(row + offset.unsigned_abs(), 0x51a2).tanh()),
            );
        }

        let row_random_edges = random_edges.min(n.saturating_sub(1));
        for edge in 0..row_random_edges {
            let mut jump = (gaussianish_signal(row * 17 + edge, 0x8f3d + edge as u64).abs()
                * n as f64
                * 0.37) as usize;
            jump = jump % n.max(1);
            let mut col = (row + 3 + jump + edge * 11) % n.max(1);
            if col == row {
                col = (col + 1) % n.max(1);
            }
            while cols.contains(&col) || col == row {
                col = (col + 1) % n.max(1);
            }
            let magnitude = 0.03
                + 0.05
                    * (gaussianish_signal(row * 23 + edge, 0x1d4f + edge as u64)
                        .abs()
                        .min(3.0)
                        / 3.0);
            let sign = if gaussianish_signal(row * 29 + edge, 0xa761 + edge as u64) >= 0.0 {
                1.0
            } else {
                -1.0
            };
            cols.push(col);
            vals.push(sign * magnitude);
        }

        let row_abs_sum = vals.iter().map(|value| value.abs()).sum::<f64>();
        triplets.push(Triplet::new(row, row, shift + 0.75 + 1.15 * row_abs_sum));
        for (&col, &value) in cols.iter().zip(vals.iter()) {
            triplets.push(Triplet::new(row, col, value));
        }
    }
    triplets
}

fn apply_scale_spread_to_triplets(
    triplets: &mut [Triplet<usize, usize, f64>],
    nrows: usize,
    ncols: usize,
    scale_spread_decades: f64,
) {
    let spread = scale_spread_decades.clamp(0.0, 14.0);
    if spread <= 0.0 {
        return;
    }

    let row_scale = build_scale_profile(nrows, spread, 0x31d2, 0x8a4f);
    let col_scale = build_scale_profile(ncols, spread, 0x5c91, 0xd247);

    for triplet in triplets.iter_mut() {
        triplet.val = row_scale[triplet.row].recip() * triplet.val * col_scale[triplet.col].recip();
    }
}

fn build_scale_profile(n: usize, spread: f64, seed_a: u64, seed_b: u64) -> Vec<f64> {
    if n == 0 {
        return Vec::new();
    }

    let raw = (0..n)
        .map(|idx| {
            let a = gaussianish_signal(idx * 17 + 3, seed_a).tanh();
            let b = gaussianish_signal(idx * 31 + 11, seed_b).tanh();
            0.65 * a + 0.35 * b
        })
        .collect::<Vec<_>>();
    let mean = raw.iter().copied().sum::<f64>() / n as f64;
    let centered = raw.iter().map(|&value| value - mean).collect::<Vec<_>>();
    let max_abs = centered
        .iter()
        .copied()
        .map(f64::abs)
        .fold(0.0_f64, f64::max)
        .max(1.0e-12);

    centered
        .into_iter()
        .map(|value| 10.0_f64.powf(0.5 * spread * (value / max_abs)))
        .collect()
}

#[cfg(test)]
fn correlation(lhs: &[f64], rhs: &[f64]) -> f64 {
    assert_eq!(lhs.len(), rhs.len());
    if lhs.is_empty() {
        return 0.0;
    }

    let lhs_mean = lhs.iter().copied().sum::<f64>() / lhs.len() as f64;
    let rhs_mean = rhs.iter().copied().sum::<f64>() / rhs.len() as f64;
    let mut num = 0.0;
    let mut lhs_den = 0.0;
    let mut rhs_den = 0.0;
    for (&x, &y) in lhs.iter().zip(rhs.iter()) {
        let dx = x - lhs_mean;
        let dy = y - rhs_mean;
        num += dx * dy;
        lhs_den += dx * dx;
        rhs_den += dy * dy;
    }
    num / (lhs_den.sqrt() * rhs_den.sqrt()).max(1.0e-12)
}

fn sparse_pattern_points(matrix: &SparseColMat<usize, f64>) -> (Vec<f64>, Vec<f64>) {
    let matrix = matrix.as_ref().canonical();
    let mut columns = Vec::with_capacity(matrix.row_idx().len());
    let mut rows = Vec::with_capacity(matrix.row_idx().len());
    for col in 0..matrix.ncols() {
        for idx in matrix.col_ptr()[col]..matrix.col_ptr()[col + 1] {
            columns.push((col + 1) as f64);
            rows.push((matrix.row_idx()[idx] + 1) as f64);
        }
    }
    (columns, rows)
}

fn apply_matrix(matrix: &SparseColMat<usize, f64>, rhs: &[f64]) -> Vec<f64> {
    let mut out = vec![0.0; matrix.nrows()];
    matrix.as_ref().apply(&mut out, rhs);
    out
}

fn diagonal_precond_log10(matrix: &SparseColMat<usize, f64>) -> Result<Vec<f64>, String> {
    let diagonal = DiagonalPrecond::try_from(matrix.as_ref()).map_err(|err| format!("{err:?}"))?;
    Ok(diagonal
        .inverse_diagonal()
        .iter()
        .map(|value| safe_log10(value.abs()))
        .collect())
}

fn residual_norm(matrix: &SparseColMat<usize, f64>, x: &[f64], b: &[f64]) -> f64 {
    let residual = apply_matrix(matrix, x)
        .iter()
        .zip(b.iter())
        .map(|(ax, rhs)| ax - rhs)
        .collect::<Vec<_>>();
    euclidean_norm(&residual)
}

fn row_col_spread_csc(matrix: &SparseColMat<usize, f64>) -> Result<(f64, f64), String> {
    let matrix = matrix.as_ref().canonical();
    let mut row_norm = vec![0.0_f64; matrix.nrows()];
    let mut col_norm = vec![0.0_f64; matrix.ncols()];

    for col in 0..matrix.ncols() {
        let mut max_in_col = 0.0_f64;
        for idx in matrix.col_ptr()[col]..matrix.col_ptr()[col + 1] {
            let row = matrix.row_idx()[idx];
            let mag = matrix.val()[idx].abs();
            row_norm[row] = row_norm[row].max(mag);
            max_in_col = max_in_col.max(mag);
        }
        col_norm[col] = max_in_col;
    }

    let row_min = row_norm
        .iter()
        .copied()
        .filter(|value| *value > 0.0)
        .fold(f64::INFINITY, f64::min);
    let row_max = row_norm.iter().copied().fold(0.0_f64, f64::max);
    let col_min = col_norm
        .iter()
        .copied()
        .filter(|value| *value > 0.0)
        .fold(f64::INFINITY, f64::min);
    let col_max = col_norm.iter().copied().fold(0.0_f64, f64::max);

    if !row_min.is_finite() || !col_min.is_finite() {
        return Err("matrix contains an all-zero row or column".to_string());
    }

    Ok((
        row_max / row_min.max(1.0e-16),
        col_max / col_min.max(1.0e-16),
    ))
}

fn relative_error(lhs: &[f64], rhs: &[f64]) -> f64 {
    let diff = lhs
        .iter()
        .zip(rhs.iter())
        .map(|(lhs, rhs)| lhs - rhs)
        .collect::<Vec<_>>();
    euclidean_norm(&diff) / euclidean_norm(rhs).max(1.0e-16)
}

fn euclidean_norm(values: &[f64]) -> f64 {
    values.iter().map(|value| value * value).sum::<f64>().sqrt()
}

fn safe_log10(value: f64) -> f64 {
    value.max(1.0e-16).log10()
}

#[cfg(test)]
mod tests {
    use super::{
        BicgPreconditioner, EquilibrationInputs, MatrixStructure, build_scale_profile, correlation,
        run_equilibration_demo,
    };

    #[test]
    fn equilibration_demo_runs() {
        let demo = run_equilibration_demo(EquilibrationInputs {
            dimension: 80,
            matrix_structure: MatrixStructure::Coupled,
            scale_spread_decades: 5.0,
            coupling: 0.22,
            random_sparsity_percent: 0.40,
            equilibration_iterations: 8,
            tolerance: 1.0e-6,
            max_iterations: 220,
            preconditioner: BicgPreconditioner::None,
        })
        .unwrap();
        assert_eq!(demo.scale_index.len(), 80);
        assert!(demo.row_spread_after < demo.row_spread_before);
        assert!(demo.col_spread_after < demo.col_spread_before);
    }

    #[test]
    fn equilibration_demo_runs_on_tridiagonal_matrix() {
        let demo = run_equilibration_demo(EquilibrationInputs {
            dimension: 80,
            matrix_structure: MatrixStructure::Tridiagonal,
            scale_spread_decades: 6.0,
            coupling: 0.22,
            random_sparsity_percent: 0.40,
            equilibration_iterations: 8,
            tolerance: 1.0e-6,
            max_iterations: 220,
            preconditioner: BicgPreconditioner::None,
        })
        .unwrap();
        assert_eq!(demo.scale_index.len(), 80);
        assert!(demo.row_spread_after < demo.row_spread_before);
        assert!(demo.col_spread_after < demo.col_spread_before);
    }

    #[test]
    fn equilibration_demo_runs_on_random_sparse_matrix() {
        let demo = run_equilibration_demo(EquilibrationInputs {
            dimension: 80,
            matrix_structure: MatrixStructure::RandomSparse,
            scale_spread_decades: 6.0,
            coupling: 0.22,
            random_sparsity_percent: 0.60,
            equilibration_iterations: 8,
            tolerance: 1.0e-6,
            max_iterations: 220,
            preconditioner: BicgPreconditioner::Diagonal,
        })
        .unwrap();
        assert_eq!(demo.scale_index.len(), 80);
        assert!(demo.row_spread_after < demo.row_spread_before);
        assert!(demo.col_spread_after < demo.col_spread_before);
    }

    #[test]
    fn imposed_row_and_column_profiles_are_not_strongly_correlated() {
        let row = build_scale_profile(256, 8.0, 0x31d2, 0x8a4f)
            .into_iter()
            .map(f64::log10)
            .collect::<Vec<_>>();
        let col = build_scale_profile(256, 8.0, 0x5c91, 0xd247)
            .into_iter()
            .map(f64::log10)
            .collect::<Vec<_>>();

        assert!(correlation(&row, &col).abs() < 0.6);
    }
}
