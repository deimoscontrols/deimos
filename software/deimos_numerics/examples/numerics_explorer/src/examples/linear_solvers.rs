use crate::demo_signal::gaussianish_signal;
use crate::plot_helpers::{LineSeries, build_line_plot, build_sparse_pattern_plot};
use crate::plotly_support::use_plotly_chart;
use crate::timing::{measure, measure_average_until};
use faer::sparse::linalg::lu::LuSymbolicParams;
use faer::sparse::{SparseColMat, Triplet};
use faer::{Par, Spec};
use leptos::prelude::*;
use numerical::sparse::{
    BiCGSTAB, DiagonalPrecond, Equilibration, EquilibrationParams, SparseLu, SparseMatVec,
};
use plotly::Plot;

/// Interactive sparse direct-versus-iterative solver comparison.
#[component]
pub fn LinearSolverComparisonPage() -> impl IntoView {
    let (dimension, set_dimension) = signal(120_usize);
    let (diagonal_shift, set_diagonal_shift) = signal(0.25_f64);
    let (scale_spread_decades, set_scale_spread_decades) = signal(0.0_f64);
    let (tol_log10, set_tol_log10) = signal(-6.0_f64);
    let (max_iterations, set_max_iterations) = signal(50_usize);
    let (warm_start, set_warm_start) = signal(false);
    let (equilibrate, set_equilibrate) = signal(false);
    let (preconditioner, set_preconditioner) = signal(BicgPreconditioner::None);
    let (matrix_structure, set_matrix_structure) = signal(MatrixStructure::Tridiagonal);
    let (random_sparsity_percent, set_random_sparsity_percent) = signal(0.40_f64);

    let inputs = move || SparseSolverInputs {
        dimension: dimension.get(),
        diagonal_shift: diagonal_shift.get(),
        scale_spread_decades: scale_spread_decades.get(),
        tolerance: 10.0_f64.powf(tol_log10.get()),
        max_iterations: max_iterations.get(),
        warm_start: warm_start.get(),
        equilibrate: equilibrate.get(),
        preconditioner: preconditioner.get(),
        matrix_structure: matrix_structure.get(),
        random_sparsity_percent: random_sparsity_percent.get(),
    };
    let demo = Memo::new(move |_| run_sparse_solver_demo(inputs()));

    use_plotly_chart("linear-solvers-error-plot", move || {
        build_solver_plot(demo.get(), SolverPlot::StateError)
    });
    use_plotly_chart("linear-solvers-residual-plot", move || {
        build_solver_plot(demo.get(), SolverPlot::ResidualHistory)
    });
    use_plotly_chart("linear-solvers-matrix-plot", move || {
        build_solver_plot(demo.get(), SolverPlot::MatrixPattern)
    });

    let summary = move || sparse_solver_summary(demo.get());

    view! {
        <div class="page">
            <header class="page-header">
                <p class="eyebrow">"Linear Algebra"</p>
                <h1>"Sparse Solver Comparison"</h1>
                <p>
                    "Choose between a structured tridiagonal system and a harder deterministic random"
                    " sparse system, then compare sparse LU against BiCGSTAB under different solver"
                    " settings."
                </p>
            </header>

            <div class="control-layout">
                <aside class="control-card">
                    <section>
                        <h2>"System"</h2>
                        <p class="section-copy">
                            "The tridiagonal option is a one-dimensional Laplacian baseline. The"
                            " random sparse option adds irregular longer-range couplings while keeping"
                            " a strong diagonal so both solvers see a stable, repeatable problem."
                        </p>

                        <div class="control-row">
                            <label for="linear-solvers-structure">"Matrix structure"</label>
                            <select
                                id="linear-solvers-structure"
                                on:change=move |ev| {
                                    set_matrix_structure
                                        .set(MatrixStructure::from_form_value(&event_target_value(&ev)));
                                }
                            >
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
                            <label for="linear-solvers-dimension">"Dimension"</label>
                            <output>{move || dimension.get().to_string()}</output>
                            <input
                                id="linear-solvers-dimension"
                                type="range"
                                min="20"
                                max="3000"
                                step="20"
                                prop:value=move || dimension.get().to_string()
                                on:input=move |ev| {
                                    if let Ok(value) = event_target_value(&ev).parse::<usize>() {
                                        set_dimension.set(value.clamp(20, 3000));
                                    }
                                }
                            />
                        </div>

                        <div class="control-row">
                            <label for="linear-solvers-random-sparsity">"Random sparsity"</label>
                            <output>{move || format!("{:.2}%", random_sparsity_percent.get())}</output>
                            <input
                                id="linear-solvers-random-sparsity"
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

                        <div class="control-row">
                            <label for="linear-solvers-shift">"Diagonal shift"</label>
                            <output>{move || format!("{:.2}", diagonal_shift.get())}</output>
                            <input
                                id="linear-solvers-shift"
                                type="range"
                                min="0.05"
                                max="2.00"
                                step="0.05"
                                prop:value=move || diagonal_shift.get().to_string()
                                on:input=move |ev| {
                                    if let Ok(value) = event_target_value(&ev).parse::<f64>() {
                                        set_diagonal_shift.set(value.clamp(0.05, 2.0));
                                    }
                                }
                            />
                        </div>

                        <div class="control-row">
                            <label for="linear-solvers-scale-spread">"Scale spread"</label>
                            <output>{move || format!("{:.1} decades", scale_spread_decades.get())}</output>
                            <input
                                id="linear-solvers-scale-spread"
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
                            <label for="linear-solvers-tolerance">"BiCGSTAB tolerance"</label>
                            <output>{move || format!("{:.1e}", 10.0_f64.powf(tol_log10.get()))}</output>
                            <input
                                id="linear-solvers-tolerance"
                                type="range"
                                min="-10"
                                max="-3"
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
                            <label for="linear-solvers-max-iters">"Max iterations"</label>
                            <output>{move || max_iterations.get().to_string()}</output>
                            <input
                                id="linear-solvers-max-iters"
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

                        <div class="control-row checkbox-row">
                            <label for="linear-solvers-warm-start">"Warm-start BiCGSTAB"</label>
                            <input
                                id="linear-solvers-warm-start"
                                type="checkbox"
                                prop:checked=move || warm_start.get()
                                on:change=move |ev| set_warm_start.set(event_target_checked(&ev))
                            />
                        </div>

                        <div class="control-row checkbox-row">
                            <label for="linear-solvers-equilibrate">"Apply equilibration"</label>
                            <input
                                id="linear-solvers-equilibrate"
                                type="checkbox"
                                prop:checked=move || equilibrate.get()
                                on:change=move |ev| set_equilibrate.set(event_target_checked(&ev))
                            />
                        </div>

                        <div class="control-row">
                            <label for="linear-solvers-preconditioner">"Preconditioner"</label>
                            <select
                                id="linear-solvers-preconditioner"
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
                            "Sparse LU is a direct solve, so its accuracy is mostly independent of the"
                            " iterative tolerance slider. BiCGSTAB exposes the classic Krylov tradeoff:"
                            " tighter tolerance means more iterations and lower residual, especially on"
                            " the more weakly shifted, more badly scaled, or less structured systems. The warm-start toggle"
                            " seeds the Krylov solve with a diagonal-inverse guess instead of the zero"
                            " vector, while the preconditioner menu controls the right-preconditioning"
                            " strategy. The scale-spread slider applies explicit row and column scaling to"
                            " the generated matrix itself. The equilibration toggle applies the crate's two-sided row/column"
                            " scaling before both solves and then maps the solution back to the original"
                            " coordinates."
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
                                <h2>"Solver comparison traces"</h2>
                                <p>"Error and residual views for the same direct-versus-iterative solve run."</p>
                            </div>
                        </div>
                        <div class="plot-subsection">
                            <div class="plot-header">
                                <div>
                                    <h2>"Statewise solution error"</h2>
                                    <p>"Pointwise log10 absolute error against a planted dense truth vector."</p>
                                </div>
                            </div>
                            <div id="linear-solvers-error-plot" class="plot-surface"></div>
                        </div>

                        <div class="plot-subsection">
                            <div class="plot-header">
                                <div>
                                    <h2>"Residual history"</h2>
                                    <p>"BiCGSTAB residual norm per iteration, alongside the target tolerance and LU residual."</p>
                                </div>
                            </div>
                            <div id="linear-solvers-residual-plot" class="plot-surface"></div>
                        </div>
                    </article>

                    <article class="plot-card">
                        <div class="plot-header">
                            <div>
                                <h2>"Matrix sparsity pattern"</h2>
                                <p>"Spy-style nonzero pattern for the system matrix used by both solvers."</p>
                            </div>
                        </div>
                        <div id="linear-solvers-matrix-plot" class="plot-surface"></div>
                    </article>
                </div>
            </div>
        </div>
    }
}

#[derive(Clone, Copy)]
enum SolverPlot {
    StateError,
    ResidualHistory,
    MatrixPattern,
}

#[derive(Clone, Copy)]
struct SparseSolverInputs {
    dimension: usize,
    diagonal_shift: f64,
    scale_spread_decades: f64,
    tolerance: f64,
    max_iterations: usize,
    warm_start: bool,
    equilibrate: bool,
    preconditioner: BicgPreconditioner,
    matrix_structure: MatrixStructure,
    random_sparsity_percent: f64,
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
    Tridiagonal,
    RandomSparse,
}

impl MatrixStructure {
    fn from_form_value(value: &str) -> Self {
        match value {
            "random_sparse" => Self::RandomSparse,
            _ => Self::Tridiagonal,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Tridiagonal => "tridiagonal",
            Self::RandomSparse => "random sparse",
        }
    }
}

#[derive(Clone, PartialEq)]
struct SparseSolverDemo {
    state_index: Vec<f64>,
    lu_log10_error: Vec<f64>,
    bicg_log10_error: Vec<f64>,
    iteration_index: Vec<f64>,
    bicg_log10_residual_history: Vec<f64>,
    tolerance_log10_line: Vec<f64>,
    lu_residual_log10_line: Vec<f64>,
    lu_factor_ms: f64,
    lu_solve_ms: f64,
    bicg_ms: f64,
    lu_relative_error: f64,
    bicg_relative_error: f64,
    lu_residual_norm: f64,
    bicg_residual_norm: f64,
    bicg_iterations: usize,
    bicg_converged: bool,
    bicg_warm_started: bool,
    equilibrated: bool,
    bicg_preconditioner: BicgPreconditioner,
    matrix_structure: MatrixStructure,
    matrix_pattern_columns: Vec<f64>,
    matrix_pattern_rows: Vec<f64>,
    matrix_pattern_displayed: usize,
    matrix_pattern_total: usize,
    matrix_nrows: usize,
    matrix_ncols: usize,
    scale_spread_decades: f64,
    lu_factor_repetitions: usize,
    lu_solve_repetitions: usize,
    bicg_repetitions: usize,
    equilibration_ms: f64,
}

struct SparseMatrixBuild {
    matrix: SparseColMat<usize, f64>,
    pattern_columns: Vec<f64>,
    pattern_rows: Vec<f64>,
    displayed_nonzeros: usize,
    total_nonzeros: usize,
}

fn build_solver_plot(result: Result<SparseSolverDemo, String>, which: SolverPlot) -> Plot {
    match result {
        Ok(demo) => match which {
            SolverPlot::StateError => build_line_plot(
                "Sparse solve error",
                "state index",
                "log10 |x_hat - x_true|",
                false,
                vec![
                    LineSeries::lines("LU", demo.state_index.clone(), demo.lu_log10_error),
                    LineSeries::lines("BiCGSTAB", demo.state_index, demo.bicg_log10_error),
                ],
            ),
            SolverPlot::ResidualHistory => build_line_plot(
                "BiCGSTAB residual history",
                "iteration",
                "log10 residual norm",
                false,
                vec![
                    LineSeries::lines(
                        "BiCGSTAB residual",
                        demo.iteration_index.clone(),
                        demo.bicg_log10_residual_history,
                    ),
                    LineSeries::lines(
                        "target tolerance",
                        demo.iteration_index.clone(),
                        demo.tolerance_log10_line,
                    ),
                    LineSeries::lines(
                        "LU residual",
                        demo.iteration_index,
                        demo.lu_residual_log10_line,
                    ),
                ],
            ),
            SolverPlot::MatrixPattern => build_sparse_pattern_plot(
                &format!(
                    "Matrix sparsity pattern (showing {} of {} nonzeros)",
                    demo.matrix_pattern_displayed, demo.matrix_pattern_total
                ),
                demo.matrix_nrows,
                demo.matrix_ncols,
                demo.matrix_pattern_columns,
                demo.matrix_pattern_rows,
            ),
        },
        Err(message) => build_line_plot(&message, "", "", false, Vec::new()),
    }
}

fn sparse_solver_summary(result: Result<SparseSolverDemo, String>) -> String {
    match result {
        Ok(demo) => format!(
            "On the {} system{}{}{}, sparse LU factorization averaged {:.3} ms over {} runs and LU solve averaged {:.3} ms over {} runs, with relative solution error {:.2e} and residual {:.2e}. BiCGSTAB averaged {:.3} ms over {} solves after {} iterations{}{} using {} preconditioning with relative solution error {:.2e} and residual {:.2e}.",
            demo.matrix_structure.label(),
            if demo.scale_spread_decades > 0.0 {
                format!(" with {:.1}-decade scaling", demo.scale_spread_decades)
            } else {
                String::new()
            },
            if demo.equilibrated {
                " with equilibration"
            } else {
                ""
            },
            if demo.equilibrated {
                format!(" (analysis {:.3} ms)", demo.equilibration_ms)
            } else {
                String::new()
            },
            demo.lu_factor_ms,
            demo.lu_factor_repetitions,
            demo.lu_solve_ms,
            demo.lu_solve_repetitions,
            demo.lu_relative_error,
            demo.lu_residual_norm,
            demo.bicg_ms,
            demo.bicg_repetitions,
            demo.bicg_iterations,
            if demo.bicg_converged {
                String::new()
            } else {
                String::from(" (did not hit the requested tolerance)")
            },
            if demo.bicg_warm_started {
                String::from("; warm-started")
            } else {
                String::new()
            },
            demo.bicg_preconditioner.label(),
            demo.bicg_relative_error,
            demo.bicg_residual_norm,
        ),
        Err(message) => format!("Sparse solver comparison failed: {message}"),
    }
}

fn run_sparse_solver_demo(inputs: SparseSolverInputs) -> Result<SparseSolverDemo, String> {
    let n = inputs.dimension.clamp(20, 3000);
    let shift = inputs.diagonal_shift.clamp(0.05, 2.0);
    let scale_spread_decades = inputs.scale_spread_decades.clamp(0.0, 14.0);
    let tolerance = inputs.tolerance.clamp(1.0e-12, 1.0e-3);
    let max_iterations = inputs.max_iterations.clamp(1, 400);
    let random_sparsity_percent = inputs.random_sparsity_percent.clamp(0.05, 1.50);
    let matrix_build = build_demo_matrix(
        inputs.matrix_structure,
        n,
        shift,
        random_sparsity_percent,
        scale_spread_decades,
    )
    .map_err(|err| err.to_string())?;
    let matrix = matrix_build.matrix;
    let x_true = planted_solution(n);
    let b = apply_matrix(&matrix, &x_true);

    let (solve_matrix, solve_b, equilibration_ms, equilibration) = if inputs.equilibrate {
        let mut scaled_matrix = matrix.clone();
        let mut scaled_b = b.clone();
        let (eq, elapsed) = measure(|| {
            Equilibration::<f64>::compute_from_csc(
                scaled_matrix.as_ref(),
                EquilibrationParams::default(),
            )
        });
        let eq = eq.map_err(|err| format!("{err:?}"))?;
        eq.scale_csc_matrix_in_place(&mut scaled_matrix);
        eq.scale_rhs_in_place(&mut scaled_b);
        (scaled_matrix, scaled_b, elapsed, Some(eq))
    } else {
        (matrix.clone(), b.clone(), 0.0, None)
    };
    let bicg_initial_guess = if inputs.warm_start {
        diagonal_inverse_guess(&solve_matrix, &solve_b)
    } else {
        vec![0.0; n]
    };

    let (lu_factors, lu_factor_ms, lu_factor_repetitions) =
        measure_average_until(25.0, || factorize_lu(&solve_matrix));
    let lu_factors = lu_factors?;
    let (lu_solution, lu_solve_ms, lu_solve_repetitions) =
        measure_average_until(25.0, || solve_with_lu_factors(&lu_factors, &solve_b));
    let mut lu_solution_vec = lu_solution?;

    let (bicg_result, bicg_ms, bicg_repetitions) = measure_average_until(25.0, || {
        solve_with_bicg(
            &solve_matrix,
            &bicg_initial_guess,
            &solve_b,
            tolerance,
            max_iterations,
            inputs.preconditioner,
        )
    });
    let bicg_result = bicg_result?;
    let mut bicg_solution_vec = bicg_result.solution;

    if let Some(eq) = equilibration.as_ref() {
        eq.unscale_solution_in_place(&mut lu_solution_vec);
        eq.unscale_solution_in_place(&mut bicg_solution_vec);
    }

    let state_index = (0..n).map(|i| (i + 1) as f64).collect::<Vec<_>>();
    let lu_error = pointwise_log10_error(&lu_solution_vec, &x_true);
    let bicg_error = pointwise_log10_error(&bicg_solution_vec, &x_true);
    let lu_residual = residual_norm(&matrix, &lu_solution_vec, &b);
    let bicg_residual = residual_norm(&matrix, &bicg_solution_vec, &b);
    let lu_relative_error = relative_error(&lu_solution_vec, &x_true);
    let bicg_relative_error = relative_error(&bicg_solution_vec, &x_true);
    let iteration_index = (0..bicg_result.residual_history.len())
        .map(|i| i as f64)
        .collect::<Vec<_>>();
    let bicg_log10_residual_history = bicg_result
        .residual_history
        .iter()
        .map(|&value| safe_log10(value))
        .collect::<Vec<_>>();
    let tolerance_log10_line = vec![safe_log10(tolerance); iteration_index.len()];
    let lu_residual_log10_line = vec![safe_log10(lu_residual); iteration_index.len()];

    Ok(SparseSolverDemo {
        state_index,
        lu_log10_error: lu_error,
        bicg_log10_error: bicg_error,
        iteration_index,
        bicg_log10_residual_history,
        tolerance_log10_line,
        lu_residual_log10_line,
        lu_factor_ms,
        lu_solve_ms,
        bicg_ms,
        lu_relative_error,
        bicg_relative_error,
        lu_residual_norm: lu_residual,
        bicg_residual_norm: bicg_residual,
        bicg_iterations: bicg_result.iterations,
        bicg_converged: bicg_result.converged,
        bicg_warm_started: inputs.warm_start,
        equilibrated: inputs.equilibrate,
        bicg_preconditioner: inputs.preconditioner,
        matrix_structure: inputs.matrix_structure,
        matrix_pattern_columns: matrix_build.pattern_columns,
        matrix_pattern_rows: matrix_build.pattern_rows,
        matrix_pattern_displayed: matrix_build.displayed_nonzeros,
        matrix_pattern_total: matrix_build.total_nonzeros,
        matrix_nrows: matrix.nrows(),
        matrix_ncols: matrix.ncols(),
        scale_spread_decades,
        lu_factor_repetitions,
        lu_solve_repetitions,
        bicg_repetitions,
        equilibration_ms,
    })
}

struct BicgSolveResult {
    solution: Vec<f64>,
    residual_history: Vec<f64>,
    iterations: usize,
    converged: bool,
}

fn factorize_lu(matrix: &SparseColMat<usize, f64>) -> Result<SparseLu<usize, f64>, String> {
    SparseLu::<usize, f64>::factorize(
        matrix.as_ref(),
        Par::Seq,
        LuSymbolicParams::default(),
        Spec::default(),
    )
    .map_err(|err| err.to_string())
}

fn solve_with_lu_factors(lu: &SparseLu<usize, f64>, b: &[f64]) -> Result<Vec<f64>, String> {
    let solution = lu.solve_rhs(b, Par::Seq).map_err(|err| err.to_string())?;
    Ok((0..b.len()).map(|i| solution[i]).collect())
}

fn solve_with_bicg(
    matrix: &SparseColMat<usize, f64>,
    initial_guess: &[f64],
    b: &[f64],
    tolerance: f64,
    max_iterations: usize,
    preconditioner: BicgPreconditioner,
) -> Result<BicgSolveResult, String> {
    match preconditioner {
        BicgPreconditioner::None => {
            let solver =
                BiCGSTAB::new(matrix.as_ref(), initial_guess, b).map_err(|err| err.to_string())?;
            finish_bicg_solve(solver, b.len(), tolerance, max_iterations)
        }
        BicgPreconditioner::Diagonal => {
            let diagonal =
                DiagonalPrecond::try_from(matrix.as_ref()).map_err(|err| format!("{err:?}"))?;
            let solver = BiCGSTAB::new_with_precond(matrix.as_ref(), diagonal, initial_guess, b)
                .map_err(|err| err.to_string())?;
            finish_bicg_solve(solver, b.len(), tolerance, max_iterations)
        }
    }
}

fn finish_bicg_solve<P>(
    mut solver: BiCGSTAB<f64, faer::sparse::SparseColMatRef<'_, usize, f64>, P>,
    n: usize,
    tolerance: f64,
    max_iterations: usize,
) -> Result<BicgSolveResult, String>
where
    P: numerical::sparse::Precond<f64>,
{
    let mut residual_history = vec![solver.err()];
    let mut converged = solver.err() < tolerance;
    while solver.iteration_count() < max_iterations && !converged {
        solver.step();
        residual_history.push(solver.err());
        if solver.err() < tolerance {
            converged = true;
        }
    }
    Ok(BicgSolveResult {
        solution: (0..n).map(|i| solver.x()[i]).collect(),
        residual_history,
        iterations: solver.iteration_count(),
        converged,
    })
}

fn build_demo_matrix(
    structure: MatrixStructure,
    n: usize,
    shift: f64,
    random_sparsity_percent: f64,
    scale_spread_decades: f64,
) -> Result<SparseMatrixBuild, faer::sparse::CreationError> {
    match structure {
        MatrixStructure::Tridiagonal => shifted_laplacian_matrix(n, shift, scale_spread_decades),
        MatrixStructure::RandomSparse => {
            random_sparse_matrix(n, shift, random_sparsity_percent, scale_spread_decades)
        }
    }
}

fn shifted_laplacian_matrix(
    n: usize,
    shift: f64,
    scale_spread_decades: f64,
) -> Result<SparseMatrixBuild, faer::sparse::CreationError> {
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
    apply_scale_spread_to_triplets(&mut triplets, n, n, scale_spread_decades);
    sparse_matrix_build_from_triplets(n, n, triplets)
}

fn random_sparse_matrix(
    n: usize,
    shift: f64,
    random_sparsity_percent: f64,
    scale_spread_decades: f64,
) -> Result<SparseMatrixBuild, faer::sparse::CreationError> {
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
    apply_scale_spread_to_triplets(&mut triplets, n, n, scale_spread_decades);
    sparse_matrix_build_from_triplets(n, n, triplets)
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

    let row_denom = (nrows - 1).max(1) as f64;
    let col_denom = (ncols - 1).max(1) as f64;
    let row_scale = (0..nrows)
        .map(|idx| 10.0_f64.powf(spread * (idx as f64 / row_denom - 0.5)))
        .collect::<Vec<_>>();
    let col_scale = (0..ncols)
        .map(|idx| 10.0_f64.powf(spread * (0.5 - idx as f64 / col_denom)))
        .collect::<Vec<_>>();

    for triplet in triplets.iter_mut() {
        triplet.val = row_scale[triplet.row].recip() * triplet.val * col_scale[triplet.col].recip();
    }
}

fn sparse_matrix_build_from_triplets(
    nrows: usize,
    ncols: usize,
    triplets: Vec<Triplet<usize, usize, f64>>,
) -> Result<SparseMatrixBuild, faer::sparse::CreationError> {
    let (pattern_columns, pattern_rows, displayed_nonzeros, total_nonzeros) =
        sample_sparse_pattern(&triplets, 16_000);
    let matrix = SparseColMat::try_new_from_triplets(nrows, ncols, &triplets)?;
    Ok(SparseMatrixBuild {
        matrix,
        pattern_columns,
        pattern_rows,
        displayed_nonzeros,
        total_nonzeros,
    })
}

fn sample_sparse_pattern(
    triplets: &[Triplet<usize, usize, f64>],
    max_points: usize,
) -> (Vec<f64>, Vec<f64>, usize, usize) {
    let total_nonzeros = triplets.len();
    let stride = (total_nonzeros / max_points.max(1)).max(1);
    let mut columns = Vec::with_capacity(total_nonzeros.min(max_points));
    let mut rows = Vec::with_capacity(total_nonzeros.min(max_points));

    for triplet in triplets.iter().step_by(stride) {
        columns.push((triplet.col + 1) as f64);
        rows.push((triplet.row + 1) as f64);
        if columns.len() == max_points {
            break;
        }
    }

    let displayed_nonzeros = columns.len();
    (columns, rows, displayed_nonzeros, total_nonzeros)
}

fn planted_solution(n: usize) -> Vec<f64> {
    (0..n)
        .map(|i| {
            let xi = i as f64 / ((n - 1).max(1) as f64);
            (std::f64::consts::PI * xi).sin() + 0.35 * (2.0 * std::f64::consts::PI * xi).cos()
        })
        .collect()
}

fn apply_matrix(matrix: &SparseColMat<usize, f64>, x: &[f64]) -> Vec<f64> {
    let mut out = vec![0.0; matrix.nrows()];
    matrix.as_ref().apply(&mut out, x);
    out
}

fn diagonal_inverse_guess(matrix: &SparseColMat<usize, f64>, b: &[f64]) -> Vec<f64> {
    let diagonal = DiagonalPrecond::try_from(matrix.as_ref())
        .expect("demo matrices always include a nonzero diagonal");
    let inv_diag = diagonal.inverse_diagonal();
    (0..b.len()).map(|i| b[i] * inv_diag[i]).collect()
}

fn residual_norm(matrix: &SparseColMat<usize, f64>, x: &[f64], b: &[f64]) -> f64 {
    let residual = apply_matrix(matrix, x)
        .iter()
        .zip(b.iter())
        .map(|(ax, rhs)| ax - rhs)
        .collect::<Vec<_>>();
    euclidean_norm(&residual)
}

fn relative_error(lhs: &[f64], rhs: &[f64]) -> f64 {
    let diff = lhs
        .iter()
        .zip(rhs.iter())
        .map(|(lhs, rhs)| lhs - rhs)
        .collect::<Vec<_>>();
    euclidean_norm(&diff) / euclidean_norm(rhs).max(1.0e-16)
}

fn pointwise_log10_error(lhs: &[f64], rhs: &[f64]) -> Vec<f64> {
    lhs.iter()
        .zip(rhs.iter())
        .map(|(lhs, rhs)| safe_log10((lhs - rhs).abs()))
        .collect()
}

fn euclidean_norm(values: &[f64]) -> f64 {
    values.iter().map(|value| value * value).sum::<f64>().sqrt()
}

fn safe_log10(value: f64) -> f64 {
    value.max(1.0e-16).log10()
}

#[cfg(test)]
mod tests {
    use super::{BicgPreconditioner, MatrixStructure, SparseSolverInputs, run_sparse_solver_demo};

    #[test]
    fn sparse_solver_demo_runs() {
        let demo = run_sparse_solver_demo(SparseSolverInputs {
            dimension: 80,
            diagonal_shift: 0.25,
            scale_spread_decades: 0.0,
            tolerance: 1.0e-6,
            max_iterations: 180,
            warm_start: false,
            equilibrate: false,
            preconditioner: BicgPreconditioner::None,
            matrix_structure: MatrixStructure::Tridiagonal,
            random_sparsity_percent: 0.40,
        })
        .unwrap();
        assert_eq!(demo.state_index.len(), 80);
        assert!(!demo.bicg_log10_residual_history.is_empty());
    }

    #[test]
    fn sparse_solver_demo_runs_on_random_sparse_matrix() {
        let demo = run_sparse_solver_demo(SparseSolverInputs {
            dimension: 80,
            diagonal_shift: 0.35,
            scale_spread_decades: 0.0,
            tolerance: 1.0e-6,
            max_iterations: 220,
            warm_start: true,
            equilibrate: false,
            preconditioner: BicgPreconditioner::Diagonal,
            matrix_structure: MatrixStructure::RandomSparse,
            random_sparsity_percent: 0.60,
        })
        .unwrap();
        assert_eq!(demo.state_index.len(), 80);
        assert!(!demo.bicg_log10_residual_history.is_empty());
    }

    #[test]
    fn sparse_solver_demo_runs_with_equilibration() {
        let demo = run_sparse_solver_demo(SparseSolverInputs {
            dimension: 80,
            diagonal_shift: 0.35,
            scale_spread_decades: 5.0,
            tolerance: 1.0e-6,
            max_iterations: 220,
            warm_start: false,
            equilibrate: true,
            preconditioner: BicgPreconditioner::None,
            matrix_structure: MatrixStructure::RandomSparse,
            random_sparsity_percent: 0.60,
        })
        .unwrap();
        assert_eq!(demo.state_index.len(), 80);
        assert!(demo.equilibration_ms >= 0.0);
    }
}
