/// High-level example area selection.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ExampleId {
    /// Landing page and catalog overview.
    Home,
    /// Sparse direct versus iterative solver comparison.
    LinearSolverComparison,
    /// Sparse Cholesky factorization comparison.
    SparseCholesky,
    /// Two-sided matrix equilibration for Krylov solves.
    Equilibration,
    /// Dense eigen and singular-value analysis.
    DenseSpectralDecomposition,
    /// Dense Gramian and HSVD exploration.
    GramianHsvd,
    /// Continuous-time process-model exploration.
    LtiProcessModels,
    /// Gallery of LTI plotting-data surfaces.
    LtiPlotGallery,
    /// Digital IIR filter design exploration.
    FilterDesign,
    /// Savitzky-Golay FIR smoothing versus sliding-mean comparison.
    SavGolDesign,
    /// Linear estimator comparison.
    Estimation,
    /// Nonlinear estimator comparison on a shared tracking problem.
    NonlinearEstimation,
    /// OKID plus ERA identification flow.
    Identification,
    /// FOPDT and SOPDT fitting from sampled step-response data.
    ProcessModelFit,
    /// Balanced-truncation comparison.
    Reduction,
    /// PID tuning across low-order process models and a general linear plant.
    PidDesign,
    /// DLQR controller-design comparison.
    Synthesis,
}

/// Availability status for an example page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExampleStatus {
    /// Backed by a working interactive example page.
    Ready,
}

impl ExampleStatus {
    /// Human-readable status label for badges in the UI shell.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Ready => "ready",
        }
    }

    /// CSS class for the corresponding badge.
    #[must_use]
    pub const fn class_name(self) -> &'static str {
        match self {
            Self::Ready => "ready",
        }
    }
}

/// Metadata for one selectable example page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExampleEntry {
    /// Stable identifier used by the app shell.
    pub id: ExampleId,
    /// Display title.
    pub title: &'static str,
    /// Short summary shown in navigation and cards.
    pub summary: &'static str,
    /// Implementation status badge.
    pub status: ExampleStatus,
}

/// Metadata for a major feature area in the interactive example browser.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExampleGroup {
    /// Display title for the feature area.
    pub title: &'static str,
    /// Group-level summary for the home page.
    pub summary: &'static str,
    /// Example pages that fall under this feature area.
    pub entries: &'static [ExampleEntry],
}

/// Example groups exposed by the current app shell.
pub const EXAMPLE_GROUPS: &[ExampleGroup] = &[
    ExampleGroup {
        title: "Linear Algebra",
        summary: "Interactive comparisons for sparse solves, Cholesky factors, equilibration, dense spectra, and Gramian-based state ranking.",
        entries: &[
            ExampleEntry {
                id: ExampleId::LinearSolverComparison,
                title: "Sparse Solvers",
                summary: "Compare sparse LU and BiCGSTAB on the same shifted tridiagonal system with adjustable tolerance.",
                status: ExampleStatus::Ready,
            },
            ExampleEntry {
                id: ExampleId::SparseCholesky,
                title: "Sparse Cholesky",
                summary: "Compare sparse LLT and LDLT factorizations on symmetric positive-definite and indefinite systems.",
                status: ExampleStatus::Ready,
            },
            ExampleEntry {
                id: ExampleId::Equilibration,
                title: "Equilibration",
                summary: "See how two-sided row and column scaling changes BiCGSTAB convergence on a badly scaled sparse system.",
                status: ExampleStatus::Ready,
            },
            ExampleEntry {
                id: ExampleId::DenseSpectralDecomposition,
                title: "Dense Eigen + SVD",
                summary: "Compare eigenvalue structure and singular spectra on self-adjoint and non-normal dense matrices.",
                status: ExampleStatus::Ready,
            },
            ExampleEntry {
                id: ExampleId::GramianHsvd,
                title: "Gramians + HSVD",
                summary: "Inspect controllability and observability Gramians, then compare Hankel singular values against a plain SVD of A.",
                status: ExampleStatus::Ready,
            },
        ],
    },
    ExampleGroup {
        title: "LTI Analysis",
        summary: "Interactive sampling and plotting for process models, time responses, and frequency views.",
        entries: &[
            ExampleEntry {
                id: ExampleId::LtiProcessModels,
                title: "Process Models",
                summary: "Explore delayed first-order process models with step and Bode views driven by `numerical`.",
                status: ExampleStatus::Ready,
            },
            ExampleEntry {
                id: ExampleId::LtiPlotGallery,
                title: "Plot Gallery",
                summary: "Inspect Bode, Nyquist, Nichols, pole-zero, and root-locus data from one loop transfer.",
                status: ExampleStatus::Ready,
            },
        ],
    },
    ExampleGroup {
        title: "Filter Design",
        summary: "Digital filter design examples with immediately inspectable frequency-domain behavior.",
        entries: &[
            ExampleEntry {
                id: ExampleId::FilterDesign,
                title: "Low-pass",
                summary: "Switch between Butterworth and Chebyshev Type I designs, then inspect frequency response and state-space sweeps.",
                status: ExampleStatus::Ready,
            },
            ExampleEntry {
                id: ExampleId::SavGolDesign,
                title: "Savitzky-Golay",
                summary: "Compare Savitzky-Golay smoothing against a same-window sliding mean in Bode and tap views.",
                status: ExampleStatus::Ready,
            },
        ],
    },
    ExampleGroup {
        title: "Estimation",
        summary: "Kalman, EKF, and UKF examples that can show covariance evolution, residuals, and convergence.",
        entries: &[
            ExampleEntry {
                id: ExampleId::Estimation,
                title: "Kalman Workbench",
                summary: "Compare recursive and steady-state discrete Kalman filters on the same noisy constant-velocity signal.",
                status: ExampleStatus::Ready,
            },
            ExampleEntry {
                id: ExampleId::NonlinearEstimation,
                title: "Nonlinear Tracking",
                summary: "Compare a fixed-linearization KF, an EKF, and a UKF on the same nonlinear range-tracking problem.",
                status: ExampleStatus::Ready,
            },
        ],
    },
    ExampleGroup {
        title: "Identification",
        summary: "Data-driven realization and Markov-parameter recovery examples for ERA and OKID workflows.",
        entries: &[
            ExampleEntry {
                id: ExampleId::Identification,
                title: "OKID + ERA",
                summary: "Recover Markov parameters from sampled I/O data and realize a reduced discrete model with ERA.",
                status: ExampleStatus::Ready,
            },
            ExampleEntry {
                id: ExampleId::ProcessModelFit,
                title: "Process-Model Fitting",
                summary: "Fit FOPDT and SOPDT surrogates to matched and unmatched step-response data.",
                status: ExampleStatus::Ready,
            },
        ],
    },
    ExampleGroup {
        title: "Reduction",
        summary: "Interactive views of HSVs, retained order, and reduced-model fidelity under truncation choices.",
        entries: &[ExampleEntry {
            id: ExampleId::Reduction,
            title: "Balanced Truncation",
            summary: "Tune retained order and compare full versus reduced step responses alongside the HSV spectrum.",
            status: ExampleStatus::Ready,
        }],
    },
    ExampleGroup {
        title: "Synthesis",
        summary: "Controller-design examples spanning PID, LQR, LQG, and pole placement.",
        entries: &[
            ExampleEntry {
                id: ExampleId::PidDesign,
                title: "PID Design",
                summary: "Switch between FOPDT, SOPDT, and a general linear plant and compare tuned closed-loop responses.",
                status: ExampleStatus::Ready,
            },
            ExampleEntry {
                id: ExampleId::Synthesis,
                title: "Discrete LQR",
                summary: "Tune quadratic weights and compare open-loop and closed-loop trajectories of a sampled unstable plant.",
                status: ExampleStatus::Ready,
            },
        ],
    },
];

impl ExampleId {
    /// Returns the catalog metadata for this example identifier.
    #[must_use]
    pub const fn entry(self) -> ExampleEntry {
        match self {
            Self::Home => ExampleEntry {
                id: Self::Home,
                title: "Overview",
                summary: "Start here to browse the major feature areas and implemented demos.",
                status: ExampleStatus::Ready,
            },
            Self::LinearSolverComparison => EXAMPLE_GROUPS[0].entries[0],
            Self::SparseCholesky => EXAMPLE_GROUPS[0].entries[1],
            Self::Equilibration => EXAMPLE_GROUPS[0].entries[2],
            Self::DenseSpectralDecomposition => EXAMPLE_GROUPS[0].entries[3],
            Self::GramianHsvd => EXAMPLE_GROUPS[0].entries[4],
            Self::LtiProcessModels => EXAMPLE_GROUPS[1].entries[0],
            Self::LtiPlotGallery => EXAMPLE_GROUPS[1].entries[1],
            Self::FilterDesign => EXAMPLE_GROUPS[2].entries[0],
            Self::SavGolDesign => EXAMPLE_GROUPS[2].entries[1],
            Self::Estimation => EXAMPLE_GROUPS[3].entries[0],
            Self::NonlinearEstimation => EXAMPLE_GROUPS[3].entries[1],
            Self::Identification => EXAMPLE_GROUPS[4].entries[0],
            Self::ProcessModelFit => EXAMPLE_GROUPS[4].entries[1],
            Self::Reduction => EXAMPLE_GROUPS[5].entries[0],
            Self::PidDesign => EXAMPLE_GROUPS[6].entries[0],
            Self::Synthesis => EXAMPLE_GROUPS[6].entries[1],
        }
    }
}
