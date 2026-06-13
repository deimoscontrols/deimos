use crate::catalog::{EXAMPLE_GROUPS, ExampleEntry, ExampleId};
use crate::examples::{
    DenseSpectralDecompositionPage, EquilibrationPage, EstimationPage, FilterDesignPage,
    GramianHsvdPage, HomePage, IdentificationPage, LinearSolverComparisonPage, LtiPlotGalleryPage,
    LtiProcessModelsPage, NonlinearEstimationPage, PidDesignPage, ProcessModelFitPage,
    ReductionPage, SavGolDesignPage, SparseCholeskyPage, SynthesisPage,
};
use leptos::prelude::*;

/// Top-level application shell for the interactive example browser.
#[component]
pub fn App() -> impl IntoView {
    let (selected, set_selected) = signal(ExampleId::Home);
    let (light_mode, set_light_mode) = signal(false);

    let current_entry = move || selected.get().entry();
    let current_page = move || match selected.get() {
        ExampleId::Home => view! { <HomePage set_selected /> }.into_any(),
        ExampleId::LinearSolverComparison => view! { <LinearSolverComparisonPage /> }.into_any(),
        ExampleId::SparseCholesky => view! { <SparseCholeskyPage /> }.into_any(),
        ExampleId::Equilibration => view! { <EquilibrationPage /> }.into_any(),
        ExampleId::DenseSpectralDecomposition => {
            view! { <DenseSpectralDecompositionPage /> }.into_any()
        }
        ExampleId::GramianHsvd => view! { <GramianHsvdPage /> }.into_any(),
        ExampleId::LtiProcessModels => view! { <LtiProcessModelsPage /> }.into_any(),
        ExampleId::LtiPlotGallery => view! { <LtiPlotGalleryPage /> }.into_any(),
        ExampleId::FilterDesign => view! { <FilterDesignPage /> }.into_any(),
        ExampleId::SavGolDesign => view! { <SavGolDesignPage /> }.into_any(),
        ExampleId::Estimation => view! { <EstimationPage /> }.into_any(),
        ExampleId::NonlinearEstimation => view! { <NonlinearEstimationPage /> }.into_any(),
        ExampleId::Identification => view! { <IdentificationPage /> }.into_any(),
        ExampleId::ProcessModelFit => view! { <ProcessModelFitPage /> }.into_any(),
        ExampleId::Reduction => view! { <ReductionPage /> }.into_any(),
        ExampleId::PidDesign => view! { <PidDesignPage /> }.into_any(),
        ExampleId::Synthesis => view! { <SynthesisPage /> }.into_any(),
    };

    view! {
        <div class=move || {
            if light_mode.get() {
                "app-shell light-mode"
            } else {
                "app-shell"
            }
        }>
            <aside class="sidebar">
                <h1 class="sidebar-title">"deimos_numerics"</h1>
                <p class="sidebar-copy">
                    "A browser-hosted workbench for interactive demos built directly on top of the Rust library."
                </p>

                <button
                    class=move || {
                        if light_mode.get() {
                            "theme-toggle active"
                        } else {
                            "theme-toggle"
                        }
                    }
                    attr:aria-pressed=move || light_mode.get().to_string()
                    on:click=move |_| set_light_mode.update(|enabled| *enabled = !*enabled)
                >
                    <span class="theme-toggle-icon" aria-hidden="true"></span>
                    <span>"Light mode"</span>
                </button>

                <button
                    class=move || nav_button_class(current_entry(), ExampleId::Home)
                    on:click=move |_| set_selected.set(ExampleId::Home)
                >
                    <span class="nav-button-title">
                        <span>"Overview"</span>
                        <StatusBadge entry=ExampleId::Home.entry() />
                    </span>
                </button>

                {EXAMPLE_GROUPS
                    .iter()
                    .copied()
                    .map(|group| {
                        view! {
                            <section class="nav-group">
                                <h2>{group.title}</h2>
                                <div class="nav-list">
                                    {group
                                        .entries
                                        .iter()
                                        .copied()
                                        .map(|entry| {
                                            view! {
                                                <button
                                                    class=move || nav_button_class(current_entry(), entry.id)
                                                    on:click=move |_| set_selected.set(entry.id)
                                                >
                                                    <span class="nav-button-title">
                                                        <span>{entry.title}</span>
                                                        <StatusBadge entry />
                                                    </span>
                                                </button>
                                            }
                                        })
                                        .collect_view()}
                                </div>
                            </section>
                        }
                    })
                    .collect_view()}
            </aside>

            <main class="content">{current_page}</main>
        </div>
    }
}

fn nav_button_class(current: ExampleEntry, selected_id: ExampleId) -> &'static str {
    if current.id == selected_id {
        "nav-button active"
    } else {
        "nav-button"
    }
}

#[component]
fn StatusBadge(entry: ExampleEntry) -> impl IntoView {
    view! {
        <span class=format!("status-badge {}", entry.status.class_name())>
            {entry.status.label()}
        </span>
    }
}
