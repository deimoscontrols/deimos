#![cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]

mod app;
mod catalog;
mod demo_signal;
mod examples;
mod plot_helpers;
mod plotly_support;
mod timing;

#[cfg(target_arch = "wasm32")]
fn main() {
    use leptos::{mount::mount_to_body, prelude::*};

    console_error_panic_hook::set_once();
    mount_to_body(|| view! { <app::App /> });
}

#[cfg(not(target_arch = "wasm32"))]
fn main() {
    eprintln!(
        "This example app is intended for the wasm target. Run `trunk serve` in examples/leptos-plotly/."
    );
}
