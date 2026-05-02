use crate::catalog::{EXAMPLE_GROUPS, ExampleId};
use leptos::prelude::*;

/// Landing page that exposes the major feature areas and implemented examples.
#[component]
pub fn HomePage(set_selected: WriteSignal<ExampleId>) -> impl IntoView {
    view! {
        <div class="page">
            <header class="page-header">
                <p class="eyebrow">"Interactive Example Browser"</p>
                <h1>"numerical + Leptos + Plotly"</h1>
                <p>
                    "This app is a browser-hosted workbench for interactive examples across the major control and"
                    " numerics feature areas. Each page keeps controls in Leptos signals, builds figures in Rust,"
                    " and calls directly into `numerical` for the plotted data."
                </p>
            </header>

            <div class="home-grid home-masonry">
                {EXAMPLE_GROUPS
                    .iter()
                    .copied()
                    .map(|group| {
                        view! {
                            <article class="home-card">
                                <h2>{group.title}</h2>
                                <p>{group.summary}</p>
                                <div class="nav-list">
                                    {group
                                        .entries
                                        .iter()
                                        .copied()
                                        .map(|entry| {
                                            view! {
                                                <button on:click=move |_| set_selected.set(entry.id)>
                                                    <span class="nav-button-title">
                                                        <span>{entry.title}</span>
                                                        <span
                                                            class=format!(
                                                                "status-badge {}",
                                                                entry.status.class_name()
                                                            )
                                                        >
                                                            {entry.status.label()}
                                                        </span>
                                                    </span>
                                                    <p>{entry.summary}</p>
                                                </button>
                                            }
                                        })
                                        .collect_view()}
                                </div>
                            </article>
                        }
                    })
                    .collect_view()}
            </div>
        </div>
    }
}
