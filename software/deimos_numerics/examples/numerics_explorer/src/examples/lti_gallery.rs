use crate::plot_helpers::{
    LineSeries, build_complex_plane_plot, build_line_plot, linspace, logspace,
};
use crate::plotly_support::use_plotly_chart;
use leptos::prelude::*;
use numerical::control::lti::ContinuousTransferFunction;
use plotly::Plot;
use plotly::common::{DashType, MarkerSymbol};

/// Interactive gallery page covering the LTI plot-data helpers.
#[component]
pub fn LtiPlotGalleryPage() -> impl IntoView {
    let (loop_gain, set_loop_gain) = signal(3.5_f64);
    let (zero_location, set_zero_location) = signal(0.8_f64);

    use_plotly_chart("lti-gallery-bode-mag", move || {
        build_gallery_plot(
            loop_gain.get(),
            zero_location.get(),
            GalleryPlot::BodeMagnitude,
        )
    });
    use_plotly_chart("lti-gallery-bode-phase", move || {
        build_gallery_plot(loop_gain.get(), zero_location.get(), GalleryPlot::BodePhase)
    });
    use_plotly_chart("lti-gallery-nyquist", move || {
        build_gallery_plot(loop_gain.get(), zero_location.get(), GalleryPlot::Nyquist)
    });
    use_plotly_chart("lti-gallery-nichols", move || {
        build_gallery_plot(loop_gain.get(), zero_location.get(), GalleryPlot::Nichols)
    });
    use_plotly_chart("lti-gallery-pole-zero", move || {
        build_gallery_plot(loop_gain.get(), zero_location.get(), GalleryPlot::PoleZero)
    });
    use_plotly_chart("lti-gallery-root-locus", move || {
        build_gallery_plot(loop_gain.get(), zero_location.get(), GalleryPlot::RootLocus)
    });

    let summary = move || gallery_summary(loop_gain.get(), zero_location.get());

    view! {
        <div class="page">
            <header class="page-header">
                <p class="eyebrow">"LTI Analysis"</p>
                <h1>"LTI Plot Gallery"</h1>
                <p>
                    "This page exercises the plotting-oriented data helpers on one continuous SISO loop transfer."
                    " Changing the loop gain updates the frequency-domain views and the current closed-loop pole"
                    " marker on the root locus. Changing the zero location updates every supported plot surface."
                </p>
            </header>

            <div class="control-layout">
                <aside class="control-card">
                    <section>
                        <h2>"Loop definition"</h2>
                        <p class="section-copy">
                            "The open-loop transfer has the form `L(s) = k (s + z) / ((s + 0.2)(s + 0.9)(s + 1.6))`."
                            " That gives a simple, well-behaved loop whose pole/zero structure is still rich enough"
                            " to make each plotting surface informative."
                        </p>

                        <div class="control-row">
                            <label for="lti-gallery-gain">"Loop gain"</label>
                            <output>{move || format!("{:.2}", loop_gain.get())}</output>
                            <input
                                id="lti-gallery-gain"
                                type="range"
                                min="0.2"
                                max="12.0"
                                step="0.1"
                                prop:value=move || loop_gain.get().to_string()
                                on:input=move |ev| {
                                    if let Ok(value) = event_target_value(&ev).parse::<f64>() {
                                        set_loop_gain.set(value.max(0.2));
                                    }
                                }
                            />
                        </div>

                        <div class="control-row">
                            <label for="lti-gallery-zero">"Zero location"</label>
                            <output>{move || format!("{:.2}", zero_location.get())}</output>
                            <input
                                id="lti-gallery-zero"
                                type="range"
                                min="0.2"
                                max="2.0"
                                step="0.05"
                                prop:value=move || zero_location.get().to_string()
                                on:input=move |ev| {
                                    if let Ok(value) = event_target_value(&ev).parse::<f64>() {
                                        set_zero_location.set(value.max(0.2));
                                    }
                                }
                            />
                        </div>
                    </section>

                    <section>
                        <h2>"Implemented plot types"</h2>
                        <p class="section-copy">
                            "This gallery covers the SISO plotting-data helpers exposed by the LTI layer:"
                            " Bode, Nyquist, Nichols, pole-zero, and root locus."
                        </p>
                    </section>

                    <section>
                        <h2>"Current readout"</h2>
                        <p class="section-copy">{summary}</p>
                    </section>
                </aside>

                <div class="plots-grid wide">
                    <article class="plot-card">
                        <div class="plot-header">
                            <div>
                                <h2>"Bode charts"</h2>
                                <p>"Magnitude and phase views of the same open-loop frequency response."</p>
                            </div>
                        </div>
                        <div class="plot-subsection">
                            <div class="plot-header">
                                <div>
                                    <h2>"Bode magnitude"</h2>
                                    <p>"Magnitude in dB on a monotone logarithmic frequency grid."</p>
                                </div>
                            </div>
                            <div id="lti-gallery-bode-mag" class="plot-surface"></div>
                        </div>

                        <div class="plot-subsection">
                            <div class="plot-header">
                                <div>
                                    <h2>"Bode phase"</h2>
                                    <p>"Unwrapped phase returned directly by `bode_data`."</p>
                                </div>
                            </div>
                            <div id="lti-gallery-bode-phase" class="plot-surface"></div>
                        </div>
                    </article>

                    <article class="plot-card">
                        <div class="plot-header">
                            <div>
                                <h2>"Pole-zero map"</h2>
                                <p>"Open-loop poles and zeros for the current loop transfer."</p>
                            </div>
                        </div>
                        <div id="lti-gallery-pole-zero" class="plot-surface"></div>
                    </article>

                    <article class="plot-card">
                        <div class="plot-header">
                            <div>
                                <h2>"Root locus"</h2>
                                <p>"Sampled closed-loop pole branches with the current gain highlighted."</p>
                            </div>
                        </div>
                        <div id="lti-gallery-root-locus" class="plot-surface"></div>
                    </article>

                    <div class="plots-grid compact">
                        <article class="plot-card">
                            <div class="plot-header">
                                <div>
                                    <h2>"Nyquist"</h2>
                                    <p>"Positive-frequency branch in the complex plane."</p>
                                </div>
                            </div>
                            <div id="lti-gallery-nyquist" class="plot-surface"></div>
                        </article>

                        <article class="plot-card">
                            <div class="plot-header">
                                <div>
                                    <h2>"Nichols"</h2>
                                    <p>"Unwrapped phase in degrees versus magnitude in dB."</p>
                                </div>
                            </div>
                            <div id="lti-gallery-nichols" class="plot-surface"></div>
                        </article>
                    </div>
                </div>
            </div>
        </div>
    }
}

#[derive(Clone, Copy)]
enum GalleryPlot {
    BodeMagnitude,
    BodePhase,
    Nyquist,
    Nichols,
    PoleZero,
    RootLocus,
}

struct GalleryDemo {
    bode_frequencies: Vec<f64>,
    bode_magnitude_db: Vec<f64>,
    bode_phase_deg: Vec<f64>,
    nyquist_re: Vec<f64>,
    nyquist_im: Vec<f64>,
    nichols_phase_deg: Vec<f64>,
    nichols_magnitude_db: Vec<f64>,
    open_loop_poles_re: Vec<f64>,
    open_loop_poles_im: Vec<f64>,
    open_loop_zeros_re: Vec<f64>,
    open_loop_zeros_im: Vec<f64>,
    root_locus_branches: Vec<(Vec<f64>, Vec<f64>)>,
    current_closed_loop_re: Vec<f64>,
    current_closed_loop_im: Vec<f64>,
    current_gain_margin_db: Option<f64>,
    current_phase_margin_deg: Option<f64>,
}

fn build_gallery_plot(loop_gain: f64, zero_location: f64, which: GalleryPlot) -> Plot {
    match run_gallery_demo(loop_gain, zero_location) {
        Ok(demo) => match which {
            GalleryPlot::BodeMagnitude => build_line_plot(
                "Bode magnitude",
                "angular frequency (rad/s)",
                "magnitude (dB)",
                true,
                vec![LineSeries::lines(
                    "magnitude",
                    demo.bode_frequencies,
                    demo.bode_magnitude_db,
                )],
            ),
            GalleryPlot::BodePhase => build_line_plot(
                "Bode phase",
                "angular frequency (rad/s)",
                "phase (deg)",
                true,
                vec![LineSeries::lines(
                    "phase",
                    demo.bode_frequencies,
                    demo.bode_phase_deg,
                )],
            ),
            GalleryPlot::Nyquist => build_complex_plane_plot(
                "Nyquist",
                vec![LineSeries::lines(
                    "positive branch",
                    demo.nyquist_re,
                    demo.nyquist_im,
                )],
            ),
            GalleryPlot::Nichols => build_line_plot(
                "Nichols",
                "phase (deg)",
                "magnitude (dB)",
                false,
                vec![LineSeries::lines(
                    "nichols",
                    demo.nichols_phase_deg,
                    demo.nichols_magnitude_db,
                )],
            ),
            GalleryPlot::PoleZero => build_complex_plane_plot(
                "Pole-zero map",
                vec![
                    LineSeries::markers("zeros", demo.open_loop_zeros_re, demo.open_loop_zeros_im)
                        .with_marker_symbol(MarkerSymbol::CircleOpen)
                        .with_line_width(1.0),
                    LineSeries::markers("poles", demo.open_loop_poles_re, demo.open_loop_poles_im)
                        .with_marker_symbol(MarkerSymbol::X)
                        .with_dash(DashType::Dash)
                        .with_line_width(1.0),
                ],
            ),
            GalleryPlot::RootLocus => {
                let mut series = demo
                    .root_locus_branches
                    .into_iter()
                    .enumerate()
                    .map(|(index, (re, im))| {
                        LineSeries::lines(format!("branch {}", index + 1), re, im)
                    })
                    .collect::<Vec<_>>();
                series.push(
                    LineSeries::markers(
                        "current closed-loop poles",
                        demo.current_closed_loop_re,
                        demo.current_closed_loop_im,
                    )
                    .with_marker_symbol(MarkerSymbol::X)
                    .with_line_width(1.0),
                );
                build_complex_plane_plot("Root locus", series)
            }
        },
        Err(message) => build_line_plot(&message, "", "", false, Vec::new()),
    }
}

fn gallery_summary(loop_gain: f64, zero_location: f64) -> String {
    match run_gallery_demo(loop_gain, zero_location) {
        Ok(demo) => {
            let gain_margin = demo
                .current_gain_margin_db
                .map(|value| format!("{value:.2} dB"))
                .unwrap_or_else(|| "none on sampled grid".to_string());
            let phase_margin = demo
                .current_phase_margin_deg
                .map(|value| format!("{value:.2} deg"))
                .unwrap_or_else(|| "none on sampled grid".to_string());
            format!(
                "Current loop gain {:.2}, zero at -{:.2}. Sampled margins: gain {}, phase {}.",
                loop_gain, zero_location, gain_margin, phase_margin,
            )
        }
        Err(err) => format!("LTI gallery failed: {err}"),
    }
}

fn run_gallery_demo(loop_gain: f64, zero_location: f64) -> Result<GalleryDemo, String> {
    let open_loop = loop_transfer(loop_gain, zero_location)?;
    let base_loop = loop_transfer(1.0, zero_location)?;

    let angular_frequencies = logspace(-2.0, 1.7, 260);
    let bode = open_loop
        .bode_data(&angular_frequencies)
        .map_err(|err| err.to_string())?;
    let nyquist = open_loop
        .nyquist_data(&angular_frequencies)
        .map_err(|err| err.to_string())?;
    let nichols = open_loop
        .nichols_data(&angular_frequencies)
        .map_err(|err| err.to_string())?;
    let margins = open_loop
        .loop_margins(&angular_frequencies)
        .map_err(|err| err.to_string())?;
    let pole_zero = open_loop.pole_zero_data().map_err(|err| err.to_string())?;

    let gains = linspace(0.0, 12.0, 220);
    let root_locus = base_loop
        .root_locus_data(&gains)
        .map_err(|err| err.to_string())?;
    let current_closed_loop = open_loop
        .unity_feedback()
        .map_err(|err| err.to_string())?
        .pole_zero_data()
        .map_err(|err| err.to_string())?;

    let root_locus_branches = root_locus
        .branches
        .iter()
        .map(|branch| {
            let finite = branch
                .poles
                .iter()
                .filter_map(|pole| pole.as_ref().map(|value| (value.re, value.im)))
                .collect::<Vec<_>>();
            let re = finite.iter().map(|value| value.0).collect::<Vec<_>>();
            let im = finite.iter().map(|value| value.1).collect::<Vec<_>>();
            (re, im)
        })
        .collect::<Vec<_>>();

    Ok(GalleryDemo {
        bode_frequencies: bode.angular_frequencies.clone(),
        bode_magnitude_db: bode.magnitude_db,
        bode_phase_deg: bode.phase_deg,
        nyquist_re: nyquist.values.iter().map(|value| value.re).collect(),
        nyquist_im: nyquist.values.iter().map(|value| value.im).collect(),
        nichols_phase_deg: nichols.phase_deg,
        nichols_magnitude_db: nichols.magnitude_db,
        open_loop_poles_re: pole_zero.poles.iter().map(|value| value.re).collect(),
        open_loop_poles_im: pole_zero.poles.iter().map(|value| value.im).collect(),
        open_loop_zeros_re: pole_zero.zeros.iter().map(|value| value.re).collect(),
        open_loop_zeros_im: pole_zero.zeros.iter().map(|value| value.im).collect(),
        root_locus_branches,
        current_closed_loop_re: current_closed_loop
            .poles
            .iter()
            .map(|value| value.re)
            .collect(),
        current_closed_loop_im: current_closed_loop
            .poles
            .iter()
            .map(|value| value.im)
            .collect(),
        current_gain_margin_db: margins.gain_margin_db,
        current_phase_margin_deg: margins.phase_margin_deg,
    })
}

fn loop_transfer(
    loop_gain: f64,
    zero_location: f64,
) -> Result<ContinuousTransferFunction<f64>, String> {
    ContinuousTransferFunction::continuous(
        vec![loop_gain, loop_gain * zero_location],
        vec![1.0, 2.7, 2.06, 0.288],
    )
    .map_err(|err| err.to_string())
}
