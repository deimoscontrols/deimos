use crate::demo_signal::step_then_tone_signal;
use crate::plotly_support::use_plotly_chart;
use faer_traits::RealField;
use leptos::prelude::*;
use num_traits::{Float, ToPrimitive};
use numerical::control::lti::{
    DeltaSection, DigitalFilterFamily, DigitalFilterSpec, DiscreteSos, DiscreteStateSpace,
    FilterShape, SecondOrderSection, design_digital_filter_sos,
};
use numerical::sparse::CompensatedField;
use plotly::{
    Layout, Plot, Scatter,
    common::{DashType, Line, Mode, Title},
    layout::{Axis, AxisType},
};

/// Interactive digital lowpass filter-design page.
#[component]
pub fn FilterDesignPage() -> impl IntoView {
    let (family, set_family) = signal(FilterFamilyChoice::Butterworth);
    let (order, set_order) = signal(4_usize);
    let (cutoff_fraction, set_cutoff_fraction) = signal(0.4_f64);
    let (sample_rate, set_sample_rate) = signal(20.0_f64);
    let (ripple_db, set_ripple_db) = signal(1.0_f64);
    let (sim_method, set_sim_method) = signal(FilterSimMethod::Sos);
    let (design_precision, set_design_precision) = signal(FilterNumericPrecision::F64);
    let (sim_precision, set_sim_precision) = signal(FilterSimPrecision::F64);

    let inputs = move || FilterDesignInputs {
        family: family.get(),
        order: order.get(),
        cutoff_fraction: cutoff_fraction.get(),
        sample_rate: sample_rate.get(),
        ripple_db: ripple_db.get(),
        sim_method: sim_method.get(),
        design_precision: design_precision.get(),
        sim_precision: sim_precision.get(),
    };
    let design = Memo::new(move |_| run_filter_design(inputs()));

    use_plotly_chart("filter-design-mag-plot", move || {
        build_filter_plot(design.get(), FilterPlotKind::Magnitude)
    });
    use_plotly_chart("filter-design-phase-plot", move || {
        build_filter_plot(design.get(), FilterPlotKind::Phase)
    });
    use_plotly_chart("filter-design-a-sweep-plot", move || {
        build_filter_plot(design.get(), FilterPlotKind::Sweep0)
    });
    use_plotly_chart("filter-design-c-sweep-plot", move || {
        build_filter_plot(design.get(), FilterPlotKind::Sweep1)
    });
    use_plotly_chart("filter-design-d-sweep-plot", move || {
        build_filter_plot(design.get(), FilterPlotKind::Sweep2)
    });
    use_plotly_chart("filter-design-time-plot", move || {
        build_filter_plot(design.get(), FilterPlotKind::TimeResponse)
    });

    let design_status = move || filter_summary(design.get());
    let sweep_card_title = move || {
        design.with(|result| match result {
            Ok(data) => data.sweep_card_title.clone(),
            Err(_) => "Representation sweep vs cutoff".to_string(),
        })
    };
    let sweep_card_description = move || {
        design.with(|result| match result {
            Ok(data) => data.sweep_card_description.clone(),
            Err(_) => "The sweep below redesigns the same family over a log-spaced cutoff grid."
                .to_string(),
        })
    };
    let sweep_plot_0_title = move || sweep_plot_title(&design.get(), 0);
    let sweep_plot_0_description = move || sweep_plot_description(&design.get(), 0);
    let sweep_plot_1_title = move || sweep_plot_title(&design.get(), 1);
    let sweep_plot_1_description = move || sweep_plot_description(&design.get(), 1);
    let sweep_plot_2_title = move || sweep_plot_title(&design.get(), 2);
    let sweep_plot_2_description = move || sweep_plot_description(&design.get(), 2);

    view! {
        <div class="page">
            <header class="page-header">
                <p class="eyebrow">"Filter Design"</p>
                <h1>"Digital Lowpass Explorer"</h1>
                <p>
                    "This page runs the digital IIR design path directly in the browser, supports Butterworth and"
                    " Chebyshev Type I lowpass filters, and sweeps the selected execution representation over cutoff."
                </p>
            </header>

            <div class="control-layout">
                <aside class="control-card">
                    <section>
                        <h2>"Design controls"</h2>
                        <p class="section-copy">
                            "The main response plots show one designed digital lowpass filter. The matrix sweep below"
                            " redesigns that same family over a log-spaced cutoff sweep from `1e-6 * fs` to `0.45 * fs`,"
                            " following either SOS coefficients or state-space entries based on the selected simulation kernel."
                        </p>

                        <div class="control-row">
                            <label for="filter-family">"Family"</label>
                            <select
                                id="filter-family"
                                on:change=move |ev| {
                                    set_family
                                        .set(FilterFamilyChoice::from_form_value(&event_target_value(&ev)));
                                }
                            >
                                <option
                                    value="butterworth"
                                    selected=move || family.get() == FilterFamilyChoice::Butterworth
                                >
                                    "Butterworth"
                                </option>
                                <option
                                    value="chebyshev1"
                                    selected=move || family.get() == FilterFamilyChoice::Chebyshev1
                                >
                                    "Chebyshev I"
                                </option>
                            </select>
                        </div>

                        <div class="control-row">
                            <label for="filter-order">"Order"</label>
                            <output>{move || order.get().to_string()}</output>
                            <input
                                id="filter-order"
                                type="range"
                                min="1"
                                max="10"
                                step="1"
                                prop:value=move || order.get().to_string()
                                on:input=move |ev| {
                                    if let Ok(value) = event_target_value(&ev).parse::<usize>() {
                                        set_order.set(value.max(1));
                                    }
                                }
                            />
                        </div>

                        <div class="control-row">
                            <label for="filter-cutoff">"Cutoff / fs"</label>
                            <output>
                                {move || {
                                    let ratio = cutoff_fraction.get();
                                    format!("{ratio:.3e} fs ({:.4})", ratio * sample_rate.get())
                                }}
                            </output>
                            <input
                                id="filter-cutoff"
                                type="range"
                                min="-6.0"
                                max=move || MAX_CUTOFF_FRACTION.log10().to_string()
                                step="0.02"
                                prop:value=move || cutoff_fraction_to_log10(cutoff_fraction.get()).to_string()
                                on:input=move |ev| {
                                    if let Ok(value) = event_target_value(&ev).parse::<f64>() {
                                        set_cutoff_fraction.set(log10_to_cutoff_fraction(value));
                                    }
                                }
                            />
                        </div>

                        <div class="control-row">
                            <label for="filter-sample-rate">"Sample rate"</label>
                            <output>{move || format!("{:.2} samples/s", sample_rate.get())}</output>
                            <input
                                id="filter-sample-rate"
                                type="range"
                                min="4.0"
                                max="40.0"
                                step="0.5"
                                prop:value=move || sample_rate.get().to_string()
                                on:input=move |ev| {
                                    if let Ok(value) = event_target_value(&ev).parse::<f64>() {
                                        set_sample_rate.set(value.clamp(4.0, 40.0));
                                    }
                                }
                            />
                        </div>

                        <div class="control-row">
                            <label for="filter-ripple">"Passband ripple"</label>
                            <output>{move || format!("{:.2} dB", ripple_db.get())}</output>
                            <input
                                id="filter-ripple"
                                type="range"
                                min="0.10"
                                max="3.00"
                                step="0.05"
                                prop:value=move || ripple_db.get().to_string()
                                prop:disabled=move || family.get() != FilterFamilyChoice::Chebyshev1
                                on:input=move |ev| {
                                    if let Ok(value) = event_target_value(&ev).parse::<f64>() {
                                        set_ripple_db.set(value.clamp(0.10, 3.00));
                                    }
                                }
                            />
                        </div>

                        <div class="control-row">
                            <label for="filter-sim-method">"Simulation kernel"</label>
                            <select
                                id="filter-sim-method"
                                prop:value=move || sim_method.get().as_key().to_string()
                                on:change=move |ev| {
                                    set_sim_method.set(FilterSimMethod::from_form_value(&event_target_value(&ev)));
                                }
                            >
                                <option value="sos">"SOS cascade"</option>
                                <option value="delta-sos">"Delta-SOS"</option>
                                <option value="state-space">"State-space"</option>
                            </select>
                        </div>

                        <div class="control-row">
                            <label for="filter-design-f32">"Design filter in f32"</label>
                            <input
                                id="filter-design-f32"
                                type="checkbox"
                                prop:checked=move || design_precision.get() == FilterNumericPrecision::F32
                                on:change=move |ev| {
                                    set_design_precision.set(if event_target_checked(&ev) {
                                        FilterNumericPrecision::F32
                                    } else {
                                        FilterNumericPrecision::F64
                                    });
                                }
                            />
                        </div>

                        <div class="control-row">
                            <label for="filter-sim-f32">"Evaluate time sim in f32"</label>
                            <input
                                id="filter-sim-f32"
                                type="checkbox"
                                prop:checked=move || sim_precision.get() == FilterSimPrecision::F32
                                on:change=move |ev| {
                                    set_sim_precision.set(if event_target_checked(&ev) {
                                        FilterSimPrecision::F32
                                    } else {
                                        FilterSimPrecision::F64
                                    });
                                }
                            />
                        </div>
                    </section>

                    <section>
                        <h2>"Design summary"</h2>
                        <p class="section-copy">{design_status}</p>
                    </section>
                </aside>

                <div class="plots-grid wide">
                    <article class="plot-card">
                        <div class="plot-header">
                            <div>
                                <h2>"Frequency response"</h2>
                                <p>"Magnitude and phase of the currently selected digital lowpass design."</p>
                            </div>
                        </div>
                        <div class="plot-subsection">
                            <div class="plot-header">
                                <div>
                                    <h2>"Magnitude"</h2>
                                    <p>"Sampled on the unit circle from the designed digital SOS filter."</p>
                                </div>
                            </div>
                            <div id="filter-design-mag-plot" class="plot-surface"></div>
                        </div>

                        <div class="plot-subsection">
                            <div class="plot-header">
                                <div>
                                    <h2>"Phase"</h2>
                                    <p>"Unwrapped phase over the same monotone frequency grid."</p>
                                </div>
                            </div>
                            <div id="filter-design-phase-plot" class="plot-surface"></div>
                        </div>

                        <div class="plot-subsection">
                            <div class="plot-header">
                                <div>
                                    <h2>"Time-domain response"</h2>
                                    <p>"A short step followed by a single-tone check at the selected cutoff frequency."</p>
                                </div>
                            </div>
                            <div id="filter-design-time-plot" class="plot-surface"></div>
                        </div>
                    </article>

                    <article class="plot-card">
                        <div class="plot-header">
                            <div>
                                <h2>{sweep_card_title}</h2>
                                <p>{sweep_card_description}</p>
                            </div>
                        </div>

                        <div class="plot-subsection">
                            <div class="plot-header">
                                <div>
                                    <h2>{sweep_plot_0_title}</h2>
                                    <p>{sweep_plot_0_description}</p>
                                </div>
                            </div>
                            <div id="filter-design-a-sweep-plot" class="plot-surface"></div>
                        </div>

                        <div class="plot-subsection">
                            <div class="plot-header">
                                <div>
                                    <h2>{sweep_plot_1_title}</h2>
                                    <p>{sweep_plot_1_description}</p>
                                </div>
                            </div>
                            <div id="filter-design-c-sweep-plot" class="plot-surface"></div>
                        </div>

                        <div class="plot-subsection">
                            <div class="plot-header">
                                <div>
                                    <h2>{sweep_plot_2_title}</h2>
                                    <p>{sweep_plot_2_description}</p>
                                </div>
                            </div>
                            <div id="filter-design-d-sweep-plot" class="plot-surface"></div>
                        </div>
                    </article>
                </div>
            </div>
        </div>
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FilterFamilyChoice {
    Butterworth,
    Chebyshev1,
}

impl FilterFamilyChoice {
    fn from_form_value(value: &str) -> Self {
        match value {
            "chebyshev1" => Self::Chebyshev1,
            _ => Self::Butterworth,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Butterworth => "Butterworth",
            Self::Chebyshev1 => "Chebyshev I",
        }
    }
}

#[derive(Clone, Copy)]
struct FilterDesignInputs {
    family: FilterFamilyChoice,
    order: usize,
    cutoff_fraction: f64,
    sample_rate: f64,
    ripple_db: f64,
    sim_method: FilterSimMethod,
    design_precision: FilterNumericPrecision,
    sim_precision: FilterSimPrecision,
}

#[derive(Clone, PartialEq)]
struct FilterDesignData {
    family: FilterFamilyChoice,
    order: usize,
    cutoff_fraction: f64,
    cutoff: f64,
    sample_rate: f64,
    ripple_db: f64,
    sections: usize,
    state_order: usize,
    dc_gain: f64,
    sim_method: FilterSimMethod,
    design_precision: FilterNumericPrecision,
    sim_precision: FilterSimPrecision,
    response_frequency_over_fs: Vec<f64>,
    magnitude_db: Vec<f64>,
    phase_deg: Vec<f64>,
    simulation_times: Vec<f64>,
    simulation_input: Vec<f64>,
    simulation_output: Vec<f64>,
    sweep_cutoff_over_fs: Vec<f64>,
    sweep_card_title: String,
    sweep_card_description: String,
    sweep_plots: Vec<SweepPlotData>,
}

#[derive(Clone, PartialEq)]
struct SweepPlotData {
    title: String,
    description: String,
    y_label: String,
    log_y: bool,
    absolute_value: bool,
    series: Vec<Vec<f64>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FilterSimMethod {
    Sos,
    DeltaSos,
    StateSpace,
}

impl FilterSimMethod {
    fn as_key(self) -> &'static str {
        match self {
            Self::Sos => "sos",
            Self::DeltaSos => "delta-sos",
            Self::StateSpace => "state-space",
        }
    }

    fn from_form_value(value: &str) -> Self {
        match value {
            "delta-sos" => Self::DeltaSos,
            "state-space" => Self::StateSpace,
            _ => Self::Sos,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Sos => "SOS cascade",
            Self::DeltaSos => "Delta-SOS",
            Self::StateSpace => "state-space",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FilterSimPrecision {
    F64,
    F32,
}

impl FilterSimPrecision {
    fn label(self) -> &'static str {
        match self {
            Self::F64 => "f64",
            Self::F32 => "f32",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FilterNumericPrecision {
    F64,
    F32,
}

impl FilterNumericPrecision {
    fn label(self) -> &'static str {
        match self {
            Self::F64 => "f64",
            Self::F32 => "f32",
        }
    }
}

#[derive(Clone, Copy)]
enum FilterPlotKind {
    Magnitude,
    Phase,
    TimeResponse,
    Sweep0,
    Sweep1,
    Sweep2,
}

fn build_filter_plot(result: Result<FilterDesignData, String>, kind: FilterPlotKind) -> Plot {
    match result {
        Ok(data) => match kind {
            FilterPlotKind::Magnitude => build_multiline_plot(
                &format!("{} magnitude", data.family.label()),
                "frequency / fs",
                "magnitude (dB)",
                true,
                false,
                true,
                vec![(
                    "response".to_string(),
                    data.response_frequency_over_fs,
                    data.magnitude_db,
                )],
            ),
            FilterPlotKind::Phase => build_multiline_plot(
                &format!("{} phase", data.family.label()),
                "frequency / fs",
                "phase (deg)",
                true,
                false,
                true,
                vec![(
                    "response".to_string(),
                    data.response_frequency_over_fs,
                    data.phase_deg,
                )],
            ),
            FilterPlotKind::TimeResponse => build_multiline_plot(
                &format!(
                    "{} time response (design {} / {} / eval {})",
                    data.family.label(),
                    data.design_precision.label(),
                    data.sim_method.label(),
                    data.sim_precision.label(),
                ),
                "time (s)",
                "signal",
                false,
                false,
                true,
                vec![
                    (
                        "input".to_string(),
                        data.simulation_times.clone(),
                        data.simulation_input,
                    ),
                    (
                        "filtered".to_string(),
                        data.simulation_times,
                        data.simulation_output,
                    ),
                ],
            ),
            FilterPlotKind::Sweep0 => {
                build_sweep_plot(data.sweep_plots.first(), &data.sweep_cutoff_over_fs)
            }
            FilterPlotKind::Sweep1 => {
                build_sweep_plot(data.sweep_plots.get(1), &data.sweep_cutoff_over_fs)
            }
            FilterPlotKind::Sweep2 => {
                build_sweep_plot(data.sweep_plots.get(2), &data.sweep_cutoff_over_fs)
            }
        },
        Err(message) => build_empty_plot(&message, "", ""),
    }
}

fn run_filter_design(inputs: FilterDesignInputs) -> Result<FilterDesignData, String> {
    match inputs.design_precision {
        FilterNumericPrecision::F64 => run_filter_design_typed::<f64>(inputs),
        FilterNumericPrecision::F32 => run_filter_design_typed::<f32>(inputs),
    }
}

fn run_filter_design_typed<R>(inputs: FilterDesignInputs) -> Result<FilterDesignData, String>
where
    R: Float + Copy + RealField + ToPrimitive + CompensatedField,
{
    let order = inputs.order.max(1);
    let sample_rate = inputs.sample_rate.clamp(4.0, 40.0);
    let cutoff_fraction = clamp_cutoff_fraction(inputs.cutoff_fraction);
    let cutoff = cutoff_fraction * sample_rate;
    let cutoff_angular = cutoff * core::f64::consts::TAU;
    let ripple_db = inputs.ripple_db.clamp(0.10, 3.00);
    let spec =
        make_filter_spec_typed::<R>(inputs.family, order, cutoff_angular, sample_rate, ripple_db)?;
    let typed_filter = design_digital_filter_sos(&spec).map_err(|err| err.to_string())?;
    let typed_state_space = typed_filter
        .to_state_space()
        .map_err(|err| err.to_string())?;
    let filter = typed_filter
        .try_cast::<f64>()
        .map_err(|err| err.to_string())?;
    let state_space = typed_state_space
        .try_cast::<f64>()
        .map_err(|err| err.to_string())?;

    let response_frequencies = logspace(
        (MIN_CUTOFF_FRACTION * sample_rate * core::f64::consts::TAU).log10(),
        (MAX_CUTOFF_FRACTION * sample_rate * core::f64::consts::TAU).log10(),
        520,
    );
    let typed_response_frequencies = cast_slice_to_precision::<R>(&response_frequencies)?;
    let bode = typed_filter
        .bode_data(&typed_response_frequencies)
        .map_err(|err| err.to_string())?;
    let response_frequency_over_fs = bode
        .angular_frequencies
        .iter()
        .map(|omega| {
            scalar_to_f64(*omega).map(|value| value / (sample_rate * core::f64::consts::TAU))
        })
        .collect::<Result<Vec<_>, _>>()?;

    let sweep_cutoffs = logspace(
        (MIN_CUTOFF_FRACTION * sample_rate * core::f64::consts::TAU).log10(),
        (MAX_SWEEP_CUTOFF_FRACTION * sample_rate * core::f64::consts::TAU).log10(),
        240,
    );
    let (sweep_card_title, sweep_card_description, sweep_plots) = match inputs.sim_method {
        FilterSimMethod::Sos => collect_sos_sweeps_typed::<R>(
            inputs.family,
            order,
            sample_rate,
            ripple_db,
            &sweep_cutoffs,
        )?,
        FilterSimMethod::DeltaSos => collect_delta_sos_sweeps_typed::<R>(
            inputs.family,
            order,
            sample_rate,
            ripple_db,
            &sweep_cutoffs,
        )?,
        FilterSimMethod::StateSpace => collect_state_space_sweeps::<R>(
            inputs.family,
            order,
            sample_rate,
            ripple_db,
            &sweep_cutoffs,
        )?,
    };
    let (simulation_times, simulation_input) = step_then_tone_signal(sample_rate, cutoff_fraction);
    let simulation_output = run_time_response(
        &filter,
        &state_space,
        &simulation_input,
        inputs.sim_method,
        inputs.sim_precision,
    )?;

    Ok(FilterDesignData {
        family: inputs.family,
        order,
        cutoff_fraction,
        cutoff,
        sample_rate,
        ripple_db,
        sections: filter.sections().len(),
        state_order: state_space.nstates(),
        dc_gain: filter.dc_gain().map(|value| value.re).unwrap_or(0.0),
        sim_method: inputs.sim_method,
        design_precision: inputs.design_precision,
        sim_precision: inputs.sim_precision,
        response_frequency_over_fs,
        magnitude_db: bode
            .magnitude_db
            .into_iter()
            .map(scalar_to_f64)
            .collect::<Result<Vec<_>, _>>()?,
        phase_deg: bode
            .phase_deg
            .into_iter()
            .map(scalar_to_f64)
            .collect::<Result<Vec<_>, _>>()?,
        simulation_times,
        simulation_input,
        simulation_output,
        sweep_card_title,
        sweep_card_description,
        sweep_cutoff_over_fs: sweep_cutoffs
            .iter()
            .map(|cutoff| *cutoff / (sample_rate * core::f64::consts::TAU))
            .collect(),
        sweep_plots,
    })
}

/// Runs the low-pass demo's time-domain response with the selected execution
/// representation and numeric precision.
fn run_time_response(
    filter: &DiscreteSos<f64>,
    state_space: &DiscreteStateSpace<f64>,
    input: &[f64],
    sim_method: FilterSimMethod,
    sim_precision: FilterSimPrecision,
) -> Result<Vec<f64>, String> {
    match (sim_method, sim_precision) {
        (FilterSimMethod::Sos, FilterSimPrecision::F64) => filter
            .filter_forward(input)
            .map(|result| result.output)
            .map_err(|err| err.to_string()),
        (FilterSimMethod::DeltaSos, FilterSimPrecision::F64) => filter
            .to_delta_sos()
            .map_err(|err| err.to_string())?
            .filter_forward(input)
            .map(|result| result.output)
            .map_err(|err| err.to_string()),
        (FilterSimMethod::StateSpace, FilterSimPrecision::F64) => state_space
            .filter_forward(input)
            .map(|result| result.output)
            .map_err(|err| err.to_string()),
        (FilterSimMethod::Sos, FilterSimPrecision::F32) => {
            let filter = filter.try_cast::<f32>().map_err(|err| err.to_string())?;
            let input32 = input.iter().map(|&value| value as f32).collect::<Vec<_>>();
            filter
                .filter_forward(&input32)
                .map(|result| result.output.into_iter().map(f64::from).collect())
                .map_err(|err| err.to_string())
        }
        (FilterSimMethod::DeltaSos, FilterSimPrecision::F32) => {
            let filter = filter
                .to_delta_sos()
                .map_err(|err| err.to_string())?
                .try_cast::<f32>()
                .map_err(|err| err.to_string())?;
            let input32 = input.iter().map(|&value| value as f32).collect::<Vec<_>>();
            filter
                .filter_forward(&input32)
                .map(|result| result.output.into_iter().map(f64::from).collect())
                .map_err(|err| err.to_string())
        }
        (FilterSimMethod::StateSpace, FilterSimPrecision::F32) => {
            let state_space = state_space
                .try_cast::<f32>()
                .map_err(|err| err.to_string())?;
            let input32 = input.iter().map(|&value| value as f32).collect::<Vec<_>>();
            state_space
                .filter_forward(&input32)
                .map(|result| result.output.into_iter().map(f64::from).collect())
                .map_err(|err| err.to_string())
        }
    }
}

fn collect_state_space_sweeps<R>(
    family: FilterFamilyChoice,
    order: usize,
    sample_rate: f64,
    ripple_db: f64,
    sweep_cutoffs: &[f64],
) -> Result<(String, String, Vec<SweepPlotData>), String>
where
    R: Float + Copy + RealField + ToPrimitive + CompensatedField,
{
    let first_spec =
        make_filter_spec_typed::<R>(family, order, sweep_cutoffs[0], sample_rate, ripple_db)?;
    let first_filter = design_digital_filter_sos(&first_spec).map_err(|err| err.to_string())?;
    let first_ss = first_filter
        .to_state_space()
        .map_err(|err| err.to_string())?;
    let a = first_ss.a();
    let c = first_ss.c();
    let d = first_ss.d();

    let a_count = a.nrows() * a.ncols();
    let c_count = c.nrows() * c.ncols();
    let d_count = d.nrows() * d.ncols();
    let mut a_series = vec![Vec::with_capacity(sweep_cutoffs.len()); a_count];
    let mut c_series = vec![Vec::with_capacity(sweep_cutoffs.len()); c_count];
    let mut d_series = vec![Vec::with_capacity(sweep_cutoffs.len()); d_count];

    for &cutoff in sweep_cutoffs {
        let spec = make_filter_spec_typed::<R>(family, order, cutoff, sample_rate, ripple_db)?;
        let filter = design_digital_filter_sos(&spec).map_err(|err| err.to_string())?;
        let ss = filter.to_state_space().map_err(|err| err.to_string())?;
        if ss.a().nrows() != a.nrows()
            || ss.a().ncols() != a.ncols()
            || ss.c().nrows() != c.nrows()
            || ss.c().ncols() != c.ncols()
            || ss.d().nrows() != d.nrows()
            || ss.d().ncols() != d.ncols()
        {
            return Err(
                "state-space realization dimension changed across cutoff sweep".to_string(),
            );
        }

        for row in 0..a.nrows() {
            for col in 0..a.ncols() {
                a_series[row * a.ncols() + col].push(scalar_to_f64(ss.a()[(row, col)])?);
            }
        }
        for row in 0..c.nrows() {
            for col in 0..c.ncols() {
                c_series[row * c.ncols() + col].push(scalar_to_f64(ss.c()[(row, col)])?);
            }
        }
        for row in 0..d.nrows() {
            for col in 0..d.ncols() {
                d_series[row * d.ncols() + col].push(scalar_to_f64(ss.d()[(row, col)])?);
            }
        }
    }

    Ok((
        "State-space sweep vs cutoff".to_string(),
        "Nontrivial realized `A`, `C`, and `D` entries over a log-spaced cutoff sweep.".to_string(),
        vec![
            SweepPlotData {
                title: "A entries".to_string(),
                description: "All nontrivial `A[i, j]` traces, shown without entry labels."
                    .to_string(),
                y_label: "|entry|".to_string(),
                log_y: true,
                absolute_value: true,
                series: retain_nontrivial_series(a_series),
            },
            SweepPlotData {
                title: "C entries".to_string(),
                description: "All nontrivial `C[i, j]` traces, shown without entry labels."
                    .to_string(),
                y_label: "|entry|".to_string(),
                log_y: true,
                absolute_value: true,
                series: retain_nontrivial_series(c_series),
            },
            SweepPlotData {
                title: "D entries".to_string(),
                description: "All nontrivial `D[i, j]` traces, shown without entry labels."
                    .to_string(),
                y_label: "|entry|".to_string(),
                log_y: true,
                absolute_value: true,
                series: retain_nontrivial_series(d_series),
            },
        ],
    ))
}

fn collect_sos_sweeps_typed<R>(
    family: FilterFamilyChoice,
    order: usize,
    sample_rate: f64,
    ripple_db: f64,
    sweep_cutoffs: &[f64],
) -> Result<(String, String, Vec<SweepPlotData>), String>
where
    R: Float + Copy + RealField + ToPrimitive + CompensatedField,
{
    let first_spec =
        make_filter_spec_typed::<R>(family, order, sweep_cutoffs[0], sample_rate, ripple_db)?;
    let first_filter = design_digital_filter_sos(&first_spec).map_err(|err| err.to_string())?;
    let section_count = first_filter.sections().len();
    let mut gain_series = vec![Vec::with_capacity(sweep_cutoffs.len())];
    let mut numerator_series = vec![Vec::with_capacity(sweep_cutoffs.len()); section_count * 3];
    let mut denominator_series = vec![Vec::with_capacity(sweep_cutoffs.len()); section_count * 3];

    for &cutoff in sweep_cutoffs {
        let spec = make_filter_spec_typed::<R>(family, order, cutoff, sample_rate, ripple_db)?;
        let filter = design_digital_filter_sos(&spec).map_err(|err| err.to_string())?;
        if filter.sections().len() != section_count {
            return Err("SOS section count changed across cutoff sweep".to_string());
        }

        gain_series[0].push(scalar_to_f64(filter.gain())?);
        for (section_idx, section) in filter.sections().iter().enumerate() {
            push_section_coefficients(
                section,
                section_idx,
                &mut numerator_series,
                &mut denominator_series,
            )?;
        }
    }

    Ok((
        "SOS coefficient sweep vs cutoff".to_string(),
        "The selected SOS execution path is shown directly through its overall gain and per-section numerator and denominator coefficients."
            .to_string(),
        vec![
            SweepPlotData {
                title: "Overall gain".to_string(),
                description: "The magnitude of the cascade gain applied ahead of the section sequence."
                    .to_string(),
                y_label: "|gain|".to_string(),
                log_y: true,
                absolute_value: true,
                series: gain_series,
            },
            SweepPlotData {
                title: "Numerator coefficients".to_string(),
                description: "All nontrivial SOS numerator coefficient traces, shown without entry labels."
                    .to_string(),
                y_label: "|coefficient|".to_string(),
                log_y: true,
                absolute_value: true,
                series: retain_nontrivial_series(numerator_series),
            },
            SweepPlotData {
                title: "Denominator coefficients".to_string(),
                description: "All nontrivial SOS denominator coefficient traces, shown without entry labels."
                    .to_string(),
                y_label: "|coefficient|".to_string(),
                log_y: true,
                absolute_value: true,
                series: retain_nontrivial_series(denominator_series),
            },
        ],
    ))
}

/// Collects the delta-SOS parameter sweep shown when the delta execution
/// kernel is selected on the low-pass page.
fn collect_delta_sos_sweeps_typed<R>(
    family: FilterFamilyChoice,
    order: usize,
    sample_rate: f64,
    ripple_db: f64,
    sweep_cutoffs: &[f64],
) -> Result<(String, String, Vec<SweepPlotData>), String>
where
    R: Float + Copy + RealField + ToPrimitive + CompensatedField,
{
    let first_spec =
        make_filter_spec_typed::<R>(family, order, sweep_cutoffs[0], sample_rate, ripple_db)?;
    let first_filter = design_digital_filter_sos(&first_spec).map_err(|err| err.to_string())?;
    let first_delta = first_filter.to_delta_sos().map_err(|err| err.to_string())?;
    let section_count = first_delta.sections().len();
    let mut gain_series = vec![Vec::with_capacity(sweep_cutoffs.len())];
    let mut dynamics_series = vec![Vec::with_capacity(sweep_cutoffs.len()); section_count * 2];
    let mut output_series = vec![Vec::with_capacity(sweep_cutoffs.len()); section_count * 4];

    for &cutoff in sweep_cutoffs {
        let spec = make_filter_spec_typed::<R>(family, order, cutoff, sample_rate, ripple_db)?;
        let filter = design_digital_filter_sos(&spec).map_err(|err| err.to_string())?;
        let delta = filter.to_delta_sos().map_err(|err| err.to_string())?;
        if delta.sections().len() != section_count {
            return Err("delta-SOS section count changed across cutoff sweep".to_string());
        }

        gain_series[0].push(scalar_to_f64(delta.gain())?);
        for (section_idx, section) in delta.sections().iter().enumerate() {
            push_delta_section_coefficients(
                section,
                section_idx,
                &mut dynamics_series,
                &mut output_series,
            )?;
        }
    }

    Ok((
        "Delta-SOS parameter sweep vs cutoff".to_string(),
        "The selected delta-SOS execution path is shown through its overall gain, delta-domain dynamics parameters, and output coefficients."
            .to_string(),
        vec![
            SweepPlotData {
                title: "Overall gain".to_string(),
                description: "The magnitude of the cascade gain applied ahead of the delta-section sequence."
                    .to_string(),
                y_label: "|gain|".to_string(),
                log_y: true,
                absolute_value: true,
                series: gain_series,
            },
            SweepPlotData {
                title: "Delta dynamics parameters".to_string(),
                description: "All nontrivial `alpha0` and `alpha1` traces from the delta sections, shown without entry labels."
                    .to_string(),
                y_label: "|parameter|".to_string(),
                log_y: true,
                absolute_value: true,
                series: retain_nontrivial_series(dynamics_series),
            },
            SweepPlotData {
                title: "Delta output coefficients".to_string(),
                description: "All nontrivial `c*` and `d` traces from the delta sections, shown without entry labels."
                    .to_string(),
                y_label: "|coefficient|".to_string(),
                log_y: true,
                absolute_value: true,
                series: retain_nontrivial_series(output_series),
            },
        ],
    ))
}

/// Drops sweep traces that remain numerically zero across the full cutoff grid.
fn retain_nontrivial_series(series: Vec<Vec<f64>>) -> Vec<Vec<f64>> {
    series
        .into_iter()
        .filter(|values| {
            values
                .iter()
                .map(|value| value.abs())
                .fold(0.0_f64, f64::max)
                > 1.0e-10
        })
        .collect()
}

fn push_section_coefficients<R>(
    section: &SecondOrderSection<R>,
    section_idx: usize,
    numerator_series: &mut [Vec<f64>],
    denominator_series: &mut [Vec<f64>],
) -> Result<(), String>
where
    R: Float + Copy + RealField + ToPrimitive,
{
    for (coef_idx, coefficient) in section.numerator().into_iter().enumerate() {
        numerator_series[section_idx * 3 + coef_idx].push(scalar_to_f64(coefficient)?);
    }
    for (coef_idx, coefficient) in section.denominator().into_iter().enumerate() {
        denominator_series[section_idx * 3 + coef_idx].push(scalar_to_f64(coefficient)?);
    }
    Ok(())
}

/// Appends one delta section's plotted parameters into the shared sweep-series
/// buffers.
///
/// Missing coefficients for lower-order section variants are written as zeros
/// and later removed by [`retain_nontrivial_series`].
fn push_delta_section_coefficients<R>(
    section: &DeltaSection<R>,
    section_idx: usize,
    dynamics_series: &mut [Vec<f64>],
    output_series: &mut [Vec<f64>],
) -> Result<(), String>
where
    R: Float + Copy + RealField + ToPrimitive,
{
    let dynamics_base = section_idx * 2;
    let output_base = section_idx * 4;

    match *section {
        DeltaSection::Direct { d } => {
            dynamics_series[dynamics_base].push(0.0);
            dynamics_series[dynamics_base + 1].push(0.0);
            output_series[output_base].push(0.0);
            output_series[output_base + 1].push(0.0);
            output_series[output_base + 2].push(0.0);
            output_series[output_base + 3].push(scalar_to_f64(d)?);
        }
        DeltaSection::First { alpha0, c0, d } => {
            dynamics_series[dynamics_base].push(scalar_to_f64(alpha0)?);
            dynamics_series[dynamics_base + 1].push(0.0);
            output_series[output_base].push(scalar_to_f64(c0)?);
            output_series[output_base + 1].push(0.0);
            output_series[output_base + 2].push(0.0);
            output_series[output_base + 3].push(scalar_to_f64(d)?);
        }
        DeltaSection::Second {
            alpha0,
            alpha1,
            c1,
            c2,
            d,
        } => {
            dynamics_series[dynamics_base].push(scalar_to_f64(alpha0)?);
            dynamics_series[dynamics_base + 1].push(scalar_to_f64(alpha1)?);
            output_series[output_base].push(scalar_to_f64(c1)?);
            output_series[output_base + 1].push(scalar_to_f64(c2)?);
            output_series[output_base + 2].push(0.0);
            output_series[output_base + 3].push(scalar_to_f64(d)?);
        }
    }
    Ok(())
}

fn make_filter_spec_typed<R>(
    family: FilterFamilyChoice,
    order: usize,
    cutoff_angular: f64,
    sample_rate: f64,
    ripple_db: f64,
) -> Result<DigitalFilterSpec<R>, String>
where
    R: Float + Copy + RealField,
{
    let family = match family {
        FilterFamilyChoice::Butterworth => DigitalFilterFamily::Butterworth,
        FilterFamilyChoice::Chebyshev1 => DigitalFilterFamily::Chebyshev1 {
            ripple_db: cast_scalar::<R>(ripple_db)?,
        },
    };
    DigitalFilterSpec::new(
        order,
        family,
        FilterShape::Lowpass {
            cutoff: cast_scalar::<R>(cutoff_angular)?,
        },
        cast_scalar::<R>(sample_rate)?,
    )
    .map_err(|err| err.to_string())
}

fn cast_scalar<R>(value: f64) -> Result<R, String>
where
    R: Float + Copy,
{
    R::from(value).ok_or_else(|| format!("failed to cast scalar value {value}"))
}

fn cast_slice_to_precision<R>(values: &[f64]) -> Result<Vec<R>, String>
where
    R: Float + Copy,
{
    values.iter().copied().map(cast_scalar::<R>).collect()
}

fn scalar_to_f64<R>(value: R) -> Result<f64, String>
where
    R: ToPrimitive,
{
    value
        .to_f64()
        .ok_or_else(|| "failed to convert scalar to f64".to_string())
}

fn filter_summary(result: Result<FilterDesignData, String>) -> String {
    match result {
        Ok(data) => {
            let ripple_text = match data.family {
                FilterFamilyChoice::Butterworth => String::new(),
                FilterFamilyChoice::Chebyshev1 => format!(", ripple {:.2} dB", data.ripple_db),
            };
            format!(
                "{} order-{} lowpass at cutoff {:.3e} fs ({:.4}) with fs {:.2}{} designed in {}, using {} simulation in {}. Designed {} second-order sections with {} states, DC gain {:.3}, and {} / {} / {} sweep traces in the selected representation.",
                data.family.label(),
                data.order,
                data.cutoff_fraction,
                data.cutoff,
                data.sample_rate,
                ripple_text,
                data.design_precision.label(),
                data.sim_method.label(),
                data.sim_precision.label(),
                data.sections,
                data.state_order,
                data.dc_gain,
                data.sweep_plots.first().map_or(0, |plot| plot.series.len()),
                data.sweep_plots.get(1).map_or(0, |plot| plot.series.len()),
                data.sweep_plots.get(2).map_or(0, |plot| plot.series.len()),
            )
        }
        Err(message) => format!("Design failed: {message}"),
    }
}

fn build_sweep_plot(plot_data: Option<&SweepPlotData>, x: &[f64]) -> Plot {
    let Some(plot_data) = plot_data else {
        return build_empty_plot("Missing sweep plot", "cutoff / fs", "");
    };
    if plot_data.series.is_empty() {
        return build_empty_plot(
            &format!("{}: no nontrivial entries", plot_data.title),
            "cutoff / fs",
            &plot_data.y_label,
        );
    }

    let traces = plot_data
        .series
        .iter()
        .cloned()
        .map(|values| {
            (
                String::new(),
                x.to_vec(),
                if plot_data.absolute_value {
                    values
                        .into_iter()
                        .map(|value| value.abs().max(1.0e-60))
                        .collect::<Vec<_>>()
                } else {
                    values
                },
            )
        })
        .collect::<Vec<_>>();
    build_multiline_plot(
        &plot_data.title,
        "cutoff / fs",
        &plot_data.y_label,
        true,
        plot_data.log_y,
        false,
        traces,
    )
}

fn sweep_plot_title(result: &Result<FilterDesignData, String>, index: usize) -> String {
    result
        .as_ref()
        .ok()
        .and_then(|data| data.sweep_plots.get(index))
        .map(|plot| plot.title.clone())
        .unwrap_or_else(|| format!("Sweep {}", index + 1))
}

fn sweep_plot_description(result: &Result<FilterDesignData, String>, index: usize) -> String {
    result
        .as_ref()
        .ok()
        .and_then(|data| data.sweep_plots.get(index))
        .map(|plot| plot.description.clone())
        .unwrap_or_default()
}

fn build_multiline_plot(
    title: &str,
    x_label: &str,
    y_label: &str,
    log_x: bool,
    log_y: bool,
    show_legend: bool,
    series: Vec<(String, Vec<f64>, Vec<f64>)>,
) -> Plot {
    let mut plot = Plot::new();
    for (index, (name, x, y)) in series.into_iter().enumerate() {
        let line = Line::new()
            .color("#000000")
            .width(if show_legend { 2.0 } else { 1.0 })
            .dash(dash_style(index));
        let mut trace = Scatter::new(x, y).mode(Mode::Lines).line(line).name(name);
        if !show_legend {
            trace = trace.show_legend(false);
        }
        plot.add_trace(trace);
    }

    let mut x_axis = Axis::new().title(Title::with_text(x_label));
    if log_x {
        x_axis = x_axis.type_(AxisType::Log);
    }
    let mut y_axis = Axis::new().title(Title::with_text(y_label));
    if log_y {
        y_axis = y_axis.type_(AxisType::Log);
    }

    plot.set_layout(
        Layout::new()
            .title(Title::with_text(title))
            .show_legend(show_legend)
            .x_axis(x_axis)
            .y_axis(y_axis),
    );
    plot
}

fn build_empty_plot(title: &str, x_label: &str, y_label: &str) -> Plot {
    build_multiline_plot(title, x_label, y_label, false, false, false, Vec::new())
}

fn dash_style(index: usize) -> DashType {
    match index % 6 {
        0 => DashType::Solid,
        1 => DashType::Dash,
        2 => DashType::Dot,
        3 => DashType::DashDot,
        4 => DashType::LongDash,
        _ => DashType::LongDashDot,
    }
}

fn logspace(log10_start: f64, log10_stop: f64, n: usize) -> Vec<f64> {
    if n <= 1 {
        return vec![10.0_f64.powf(log10_start)];
    }
    let step = (log10_stop - log10_start) / ((n - 1) as f64);
    (0..n)
        .map(|index| 10.0_f64.powf(log10_start + (index as f64) * step))
        .collect()
}

const MIN_CUTOFF_FRACTION: f64 = 1.0e-6;
const MAX_CUTOFF_FRACTION: f64 = 0.49;
const MAX_SWEEP_CUTOFF_FRACTION: f64 = 0.45;

fn clamp_cutoff_fraction(value: f64) -> f64 {
    value.clamp(MIN_CUTOFF_FRACTION, MAX_CUTOFF_FRACTION)
}

fn cutoff_fraction_to_log10(value: f64) -> f64 {
    clamp_cutoff_fraction(value).log10()
}

fn log10_to_cutoff_fraction(value: f64) -> f64 {
    clamp_cutoff_fraction(10.0_f64.powf(value))
}

#[cfg(test)]
mod tests {
    use super::{
        FilterDesignInputs, FilterFamilyChoice, FilterNumericPrecision, FilterSimMethod,
        FilterSimPrecision, run_filter_design,
    };

    #[test]
    fn filter_design_demo_runs_for_butterworth_and_chebyshev() {
        let butterworth = run_filter_design(FilterDesignInputs {
            family: FilterFamilyChoice::Butterworth,
            order: 4,
            cutoff_fraction: 0.4,
            sample_rate: 20.0,
            ripple_db: 1.0,
            sim_method: FilterSimMethod::Sos,
            design_precision: FilterNumericPrecision::F64,
            sim_precision: FilterSimPrecision::F64,
        })
        .unwrap();
        assert!(!butterworth.magnitude_db.is_empty());
        assert_eq!(butterworth.sweep_plots.len(), 3);
        assert!(!butterworth.sweep_plots[1].series.is_empty());
        assert_eq!(
            butterworth.simulation_times.len(),
            butterworth.simulation_output.len()
        );

        let chebyshev = run_filter_design(FilterDesignInputs {
            family: FilterFamilyChoice::Chebyshev1,
            order: 4,
            cutoff_fraction: 0.4,
            sample_rate: 20.0,
            ripple_db: 1.0,
            sim_method: FilterSimMethod::DeltaSos,
            design_precision: FilterNumericPrecision::F32,
            sim_precision: FilterSimPrecision::F32,
        })
        .unwrap();
        assert!(!chebyshev.phase_deg.is_empty());
        assert_eq!(chebyshev.sweep_plots.len(), 3);
        assert_eq!(
            chebyshev.sweep_card_title,
            "Delta-SOS parameter sweep vs cutoff"
        );
        assert!(!chebyshev.sweep_plots[0].series.is_empty());
        assert_eq!(
            chebyshev.simulation_input.len(),
            chebyshev.simulation_output.len()
        );
    }

    #[test]
    fn butterworth_cutoff_tracks_selected_frequency_over_fs() {
        let sample_rate = 20.0;
        let cutoff_fraction = 0.125;
        let designed = run_filter_design(FilterDesignInputs {
            family: FilterFamilyChoice::Butterworth,
            order: 4,
            cutoff_fraction,
            sample_rate,
            ripple_db: 1.0,
            sim_method: FilterSimMethod::Sos,
            design_precision: FilterNumericPrecision::F64,
            sim_precision: FilterSimPrecision::F64,
        })
        .unwrap();

        let (nearest_index, nearest_frequency) = designed
            .response_frequency_over_fs
            .iter()
            .enumerate()
            .min_by(|(_, lhs), (_, rhs)| {
                (*lhs - cutoff_fraction)
                    .abs()
                    .total_cmp(&(*rhs - cutoff_fraction).abs())
            })
            .map(|(index, &frequency)| (index, frequency))
            .unwrap();

        assert!((nearest_frequency - cutoff_fraction).abs() <= 0.01);
        assert!((designed.magnitude_db[nearest_index] + 3.0).abs() <= 0.75);
    }
}
