use crate::plot_helpers::{LineSeries, build_line_plot, build_sparse_pattern_plot};
use crate::plotly_support::use_plotly_chart;
use crate::timing::measure;
use faer::linalg::cholesky::ldlt::factor::LdltRegularization;
use faer::linalg::cholesky::llt::factor::LltRegularization;
use faer::sparse::linalg::cholesky::{CholeskySymbolicParams, SymmetricOrdering};
use faer::sparse::{SparseColMat, Triplet};
use faer::{Par, Side, Spec};
use leptos::prelude::*;
use numerical::sparse::{SparseLdlt, SparseLlt, SparseMatVec};
use plotly::Plot;

/// Interactive sparse Cholesky page comparing LLT and LDLT on symmetric systems.
#[component]
pub fn SparseCholeskyPage() -> impl IntoView {
    let (dimension, set_dimension) = signal(180_usize);
    let (diagonal_shift, set_diagonal_shift) = signal(0.45_f64);
    let (matrix_family, set_matrix_family) = signal(CholeskyMatrixFamily::Spd);

    let inputs = move || SparseCholeskyInputs {
        dimension: dimension.get(),
        diagonal_shift: diagonal_shift.get(),
        matrix_family: matrix_family.get(),
    };
    let demo = Memo::new(move |_| run_sparse_cholesky_demo(inputs()));

    use_plotly_chart("sparse-cholesky-error-plot", move || {
        build_sparse_cholesky_plot(demo.get(), SparseCholeskyPlot::StateError)
    });
    use_plotly_chart("sparse-cholesky-matrix-plot", move || {
        build_sparse_cholesky_plot(demo.get(), SparseCholeskyPlot::MatrixPattern)
    });

    let summary = move || sparse_cholesky_summary(demo.get());

    view! {
        <div class="page">
            <header class="page-header">
                <p class="eyebrow">"Linear Algebra"</p>
                <h1>"Sparse Cholesky"</h1>
                <p>
                    "Compare sparse LLT and LDLT factorizations on the same symmetric matrix. The SPD family"
                    " shows the usual positive-definite case; the indefinite family shows when LDLT remains usable"
                    " while LLT correctly refuses the matrix."
                </p>
            </header>

            <div class="control-layout">
                <aside class="control-card">
                    <section>
                        <h2>"System"</h2>
                        <p class="section-copy">
                            "Both families are banded symmetric systems with a deterministic planted solution. The"
                            " SPD family is strictly diagonally dominant; the indefinite family alternates the sign"
                            " of its diagonal while keeping the matrix nonsingular."
                        </p>

                        <div class="control-row">
                            <label for="sparse-cholesky-family">"Matrix family"</label>
                            <select
                                id="sparse-cholesky-family"
                                on:change=move |ev| {
                                    set_matrix_family
                                        .set(CholeskyMatrixFamily::from_form_value(&event_target_value(&ev)));
                                }
                            >
                                <option
                                    value="spd"
                                    selected=move || matrix_family.get() == CholeskyMatrixFamily::Spd
                                >
                                    "SPD"
                                </option>
                                <option
                                    value="indefinite"
                                    selected=move || matrix_family.get() == CholeskyMatrixFamily::Indefinite
                                >
                                    "Indefinite"
                                </option>
                            </select>
                        </div>

                        <div class="control-row">
                            <label for="sparse-cholesky-dimension">"Dimension"</label>
                            <output>{move || dimension.get().to_string()}</output>
                            <input
                                id="sparse-cholesky-dimension"
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
                            <label for="sparse-cholesky-shift">"Diagonal shift"</label>
                            <output>{move || format!("{:.2}", diagonal_shift.get())}</output>
                            <input
                                id="sparse-cholesky-shift"
                                type="range"
                                min="0.05"
                                max="1.50"
                                step="0.05"
                                prop:value=move || diagonal_shift.get().to_string()
                                on:input=move |ev| {
                                    if let Ok(value) = event_target_value(&ev).parse::<f64>() {
                                        set_diagonal_shift.set(value.clamp(0.05, 1.50));
                                    }
                                }
                            />
                        </div>
                    </section>

                    <section>
                        <h2>"Interpretation"</h2>
                        <p class="section-copy">
                            "LLT assumes positive definiteness, so it is the fastest specialized option when that"
                            " structure is really present. LDLT is more flexible because it can factor symmetric"
                            " indefinite systems, but it does more bookkeeping and is not a drop-in replacement for"
                            " positive-definite structure checks."
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
                                <h2>"Factorization comparison"</h2>
                                <p>"Pointwise log10 solution error for whichever Cholesky variants were valid on this matrix."</p>
                            </div>
                        </div>
                        <div id="sparse-cholesky-error-plot" class="plot-surface"></div>
                    </article>

                    <article class="plot-card">
                        <div class="plot-header">
                            <div>
                                <h2>"Matrix sparsity pattern"</h2>
                                <p>"Spy-style view of the symmetric system matrix given to both Cholesky factorizations."</p>
                            </div>
                        </div>
                        <div id="sparse-cholesky-matrix-plot" class="plot-surface"></div>
                    </article>
                </div>
            </div>
        </div>
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CholeskyMatrixFamily {
    Spd,
    Indefinite,
}

impl CholeskyMatrixFamily {
    fn from_form_value(value: &str) -> Self {
        match value {
            "indefinite" => Self::Indefinite,
            _ => Self::Spd,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Spd => "SPD",
            Self::Indefinite => "indefinite",
        }
    }
}

#[derive(Clone, Copy)]
struct SparseCholeskyInputs {
    dimension: usize,
    diagonal_shift: f64,
    matrix_family: CholeskyMatrixFamily,
}

#[derive(Clone, PartialEq)]
struct SparseCholeskyDemo {
    state_index: Vec<f64>,
    llt_error_log10: Option<Vec<f64>>,
    ldlt_error_log10: Vec<f64>,
    llt_factor_ms: Option<f64>,
    llt_solve_ms: Option<f64>,
    llt_residual: Option<f64>,
    llt_relative_error: Option<f64>,
    llt_failure: Option<String>,
    ldlt_factor_ms: f64,
    ldlt_solve_ms: f64,
    ldlt_residual: f64,
    ldlt_relative_error: f64,
    matrix_columns: Vec<f64>,
    matrix_rows: Vec<f64>,
    matrix_family: CholeskyMatrixFamily,
}

#[derive(Clone, Copy)]
enum SparseCholeskyPlot {
    StateError,
    MatrixPattern,
}

fn build_sparse_cholesky_plot(
    result: Result<SparseCholeskyDemo, String>,
    which: SparseCholeskyPlot,
) -> Plot {
    match result {
        Ok(demo) => match which {
            SparseCholeskyPlot::StateError => {
                let mut series = Vec::new();
                if let Some(llt) = demo.llt_error_log10 {
                    series.push(LineSeries::lines("LLT", demo.state_index.clone(), llt));
                }
                series.push(LineSeries::lines(
                    "LDLT",
                    demo.state_index,
                    demo.ldlt_error_log10,
                ));
                build_line_plot(
                    "Sparse Cholesky solution error",
                    "state index",
                    "log10 |x - x_true|",
                    false,
                    series,
                )
            }
            SparseCholeskyPlot::MatrixPattern => build_sparse_pattern_plot(
                "Symmetric matrix pattern",
                demo.matrix_rows.iter().copied().fold(0.0_f64, f64::max) as usize,
                demo.matrix_columns.iter().copied().fold(0.0_f64, f64::max) as usize,
                demo.matrix_columns,
                demo.matrix_rows,
            ),
        },
        Err(message) => build_line_plot(&message, "", "", false, Vec::new()),
    }
}

fn sparse_cholesky_summary(result: Result<SparseCholeskyDemo, String>) -> String {
    match result {
        Ok(demo) => {
            let llt_note = match (
                demo.llt_factor_ms,
                demo.llt_solve_ms,
                demo.llt_residual,
                demo.llt_relative_error,
                demo.llt_failure.as_ref(),
            ) {
                (Some(factor_ms), Some(solve_ms), Some(residual), Some(error), _) => format!(
                    "LLT factored in {:.3} ms and solved in {:.3} ms with residual {:.2e} and relative error {:.2e}.",
                    factor_ms, solve_ms, residual, error
                ),
                (_, _, _, _, Some(reason)) => format!("LLT rejected the matrix: {reason}."),
                _ => "LLT was not available on this matrix.".to_string(),
            };

            format!(
                "{} system: {} LDLT factored in {:.3} ms and solved in {:.3} ms with residual {:.2e} and relative error {:.2e}.",
                demo.matrix_family.label(),
                llt_note,
                demo.ldlt_factor_ms,
                demo.ldlt_solve_ms,
                demo.ldlt_residual,
                demo.ldlt_relative_error,
            )
        }
        Err(err) => format!("Sparse Cholesky demo failed: {err}"),
    }
}

fn run_sparse_cholesky_demo(inputs: SparseCholeskyInputs) -> Result<SparseCholeskyDemo, String> {
    let (upper, full, x_true) = build_symmetric_system(inputs)?;
    let b = apply_matrix(&full, &x_true);
    let state_index = (0..x_true.len()).map(|idx| idx as f64).collect::<Vec<_>>();
    let (matrix_columns, matrix_rows) = sparse_pattern_points(&full);

    let (llt_result, llt_factor_ms) = measure(|| {
        SparseLlt::<usize, f64>::factorize(
            upper.as_ref(),
            Side::Upper,
            SymmetricOrdering::Identity,
            CholeskySymbolicParams::default(),
            LltRegularization::default(),
            Par::Seq,
            Spec::default(),
        )
    });

    let (llt_error_log10, llt_solve_ms, llt_residual, llt_relative_error, llt_failure) =
        match llt_result {
            Ok(llt) => {
                let (solution, solve_ms) = measure(|| llt.solve_rhs(&b, Par::Seq));
                let x = solution.map_err(|err| err.to_string())?;
                let values = x.iter().copied().collect::<Vec<_>>();
                (
                    Some(pointwise_log10_error(&values, &x_true)),
                    Some(solve_ms),
                    Some(residual_norm(&full, &values, &b)),
                    Some(relative_error(&values, &x_true)),
                    None,
                )
            }
            Err(err) => (None, None, None, None, Some(err.to_string())),
        };

    let (ldlt_result, ldlt_factor_ms) = measure(|| {
        SparseLdlt::<usize, f64>::factorize(
            upper.as_ref(),
            Side::Upper,
            SymmetricOrdering::Identity,
            CholeskySymbolicParams::default(),
            LdltRegularization::default(),
            Par::Seq,
            Spec::default(),
        )
    });
    let ldlt = ldlt_result.map_err(|err| err.to_string())?;
    let (ldlt_solution, ldlt_solve_ms) = measure(|| ldlt.solve_rhs(&b, Par::Seq));
    let ldlt_values = ldlt_solution
        .map_err(|err| err.to_string())?
        .iter()
        .copied()
        .collect::<Vec<_>>();

    Ok(SparseCholeskyDemo {
        state_index,
        llt_error_log10,
        ldlt_error_log10: pointwise_log10_error(&ldlt_values, &x_true),
        llt_factor_ms: llt_failure.is_none().then_some(llt_factor_ms),
        llt_solve_ms,
        llt_residual,
        llt_relative_error,
        llt_failure,
        ldlt_factor_ms,
        ldlt_solve_ms,
        ldlt_residual: residual_norm(&full, &ldlt_values, &b),
        ldlt_relative_error: relative_error(&ldlt_values, &x_true),
        matrix_columns,
        matrix_rows,
        matrix_family: inputs.matrix_family,
    })
}

fn build_symmetric_system(
    inputs: SparseCholeskyInputs,
) -> Result<(SparseColMat<usize, f64>, SparseColMat<usize, f64>, Vec<f64>), String> {
    let n = inputs.dimension.clamp(20, 1200);
    let shift = inputs.diagonal_shift.clamp(0.05, 1.50);

    let mut upper_triplets = Vec::with_capacity(3 * n - 3);
    let mut full_triplets = Vec::with_capacity(5 * n - 6);
    for row in 0..n {
        let diag = match inputs.matrix_family {
            CholeskyMatrixFamily::Spd => 1.9 + shift + 0.1 * ((row % 7) as f64),
            CholeskyMatrixFamily::Indefinite => {
                let mag = 1.4 + 0.35 * shift + 0.08 * ((row % 5) as f64);
                if row % 2 == 0 { mag } else { -mag }
            }
        };
        upper_triplets.push(Triplet::new(row, row, diag));
        full_triplets.push(Triplet::new(row, row, diag));

        if row + 1 < n {
            let off = match inputs.matrix_family {
                CholeskyMatrixFamily::Spd => -0.28 - 0.03 * ((row % 3) as f64),
                CholeskyMatrixFamily::Indefinite => 0.17 * if row % 2 == 0 { 1.0 } else { -1.0 },
            };
            upper_triplets.push(Triplet::new(row, row + 1, off));
            full_triplets.push(Triplet::new(row, row + 1, off));
            full_triplets.push(Triplet::new(row + 1, row, off));
        }

        if row + 2 < n {
            let off = match inputs.matrix_family {
                CholeskyMatrixFamily::Spd => 0.05,
                CholeskyMatrixFamily::Indefinite => -0.04,
            };
            upper_triplets.push(Triplet::new(row, row + 2, off));
            full_triplets.push(Triplet::new(row, row + 2, off));
            full_triplets.push(Triplet::new(row + 2, row, off));
        }
    }

    let upper = SparseColMat::<usize, f64>::try_new_from_triplets(n, n, &upper_triplets)
        .map_err(|err| err.to_string())?;
    let full = SparseColMat::<usize, f64>::try_new_from_triplets(n, n, &full_triplets)
        .map_err(|err| err.to_string())?;
    let x_true = (0..n)
        .map(|idx| (0.17 * idx as f64).sin() + if idx % 2 == 0 { 0.75 } else { -0.45 })
        .collect::<Vec<_>>();
    Ok((upper, full, x_true))
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
    use super::{CholeskyMatrixFamily, SparseCholeskyInputs, run_sparse_cholesky_demo};

    #[test]
    fn sparse_cholesky_demo_runs_on_spd_system() {
        let demo = run_sparse_cholesky_demo(SparseCholeskyInputs {
            dimension: 80,
            diagonal_shift: 0.45,
            matrix_family: CholeskyMatrixFamily::Spd,
        })
        .unwrap();
        assert_eq!(demo.state_index.len(), 80);
        assert!(demo.llt_failure.is_none());
        assert!(demo.ldlt_residual < 1.0e-8);
    }

    #[test]
    fn sparse_cholesky_demo_runs_on_indefinite_system() {
        let demo = run_sparse_cholesky_demo(SparseCholeskyInputs {
            dimension: 80,
            diagonal_shift: 0.45,
            matrix_family: CholeskyMatrixFamily::Indefinite,
        })
        .unwrap();
        assert_eq!(demo.state_index.len(), 80);
        assert!(demo.ldlt_residual < 1.0e-8);
    }
}
