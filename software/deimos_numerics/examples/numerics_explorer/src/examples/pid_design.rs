use crate::plot_helpers::{LineSeries, build_line_plot};
use crate::plotly_support::use_plotly_chart;
use faer::Mat;
use leptos::{ev::Event, prelude::*};
use numerical::control::lti::state_space::DiscretizationMethod;
use numerical::control::lti::{
    ContinuousStateSpace, ContinuousTransferFunction, FopdtModel, SopdtModel,
};
use numerical::control::synthesis::{
    AntiWindup, FrequencyPidParams, Pid, PidControllerKind, PidState, SimcPidParams,
    design_pid_from_continuous_state_space_frequency, design_pid_from_fopdt, design_pid_from_sopdt,
};
use plotly::Plot;
use plotly::common::DashType;
use std::collections::VecDeque;

/// Interactive PID-design workbench spanning low-order process models and one
/// fixed higher-order linear plant.
#[component]
pub fn PidDesignPage() -> impl IntoView {
    let (plant_kind, set_plant_kind) = signal(PidPlantKind::Fopdt);

    let (include_p, set_include_p) = signal(true);
    let (include_i, set_include_i) = signal(true);
    let (include_d, set_include_d) = signal(false);

    let (fopdt_gain, set_fopdt_gain) = signal(1.3_f64);
    let (fopdt_time_constant, set_fopdt_time_constant) = signal(3.2_f64);
    let (fopdt_delay, set_fopdt_delay) = signal(0.7_f64);
    let (fopdt_lambda, set_fopdt_lambda) = signal(2.0_f64);

    let (sopdt_gain, set_sopdt_gain) = signal(1.1_f64);
    let (sopdt_time_constant_1, set_sopdt_time_constant_1) = signal(2.8_f64);
    let (sopdt_time_constant_2, set_sopdt_time_constant_2) = signal(0.9_f64);
    let (sopdt_delay, set_sopdt_delay) = signal(0.4_f64);
    let (sopdt_lambda, set_sopdt_lambda) = signal(1.8_f64);

    let (general_gain, set_general_gain) = signal(1.8_f64);
    let (general_zero, set_general_zero) = signal(0.6_f64);
    let (general_crossover, set_general_crossover) = signal(0.9_f64);
    let (general_phase_margin, set_general_phase_margin) = signal(60.0_f64);

    let inputs = move || PidDesignInputs {
        plant_kind: plant_kind.get(),
        include_p: include_p.get(),
        include_i: include_i.get(),
        include_d: include_d.get(),
        fopdt_gain: fopdt_gain.get(),
        fopdt_time_constant: fopdt_time_constant.get(),
        fopdt_delay: fopdt_delay.get(),
        fopdt_lambda: fopdt_lambda.get(),
        sopdt_gain: sopdt_gain.get(),
        sopdt_time_constant_1: sopdt_time_constant_1.get(),
        sopdt_time_constant_2: sopdt_time_constant_2.get(),
        sopdt_delay: sopdt_delay.get(),
        sopdt_lambda: sopdt_lambda.get(),
        general_gain: general_gain.get(),
        general_zero: general_zero.get(),
        general_crossover: general_crossover.get(),
        general_phase_margin: general_phase_margin.get(),
    };

    use_plotly_chart("pid-design-output-plot", move || {
        build_pid_plot(inputs(), PidPlotKind::Output)
    });
    use_plotly_chart("pid-design-control-plot", move || {
        build_pid_plot(inputs(), PidPlotKind::Control)
    });

    let design_notes = move || design_notes_copy(plant_kind.get());
    let gains_text = move || pid_gains_text(inputs());
    let summary_text = move || pid_summary(inputs());

    let term_toggle = |id: &'static str,
                       label: &'static str,
                       checked: ReadSignal<bool>,
                       setter: WriteSignal<bool>| {
        view! {
            <label class="term-toggle" for=id>
                <input
                    id=id
                    type="checkbox"
                    prop:checked=move || checked.get()
                    on:change=move |ev: Event| setter.set(event_target_checked(&ev))
                />
                <span>{label}</span>
            </label>
        }
    };

    let plant_controls = move || match plant_kind.get() {
        PidPlantKind::Fopdt => view! {
            <>
                <div class="control-row">
                    <label for="pid-fopdt-gain">"Plant gain"</label>
                    <output>{move || format!("{:.2}", fopdt_gain.get())}</output>
                    <input
                        id="pid-fopdt-gain"
                        type="range"
                        min="0.4"
                        max="3.0"
                        step="0.05"
                        prop:value=move || fopdt_gain.get().to_string()
                        on:input=move |ev| {
                            if let Ok(value) = event_target_value(&ev).parse::<f64>() {
                                set_fopdt_gain.set(value.max(0.4));
                            }
                        }
                    />
                </div>

                <div class="control-row">
                    <label for="pid-fopdt-tau">"Time constant"</label>
                    <output>{move || format!("{:.2} s", fopdt_time_constant.get())}</output>
                    <input
                        id="pid-fopdt-tau"
                        type="range"
                        min="0.5"
                        max="10.0"
                        step="0.1"
                        prop:value=move || fopdt_time_constant.get().to_string()
                        on:input=move |ev| {
                            if let Ok(value) = event_target_value(&ev).parse::<f64>() {
                                set_fopdt_time_constant.set(value.max(0.5));
                            }
                        }
                    />
                </div>

                <div class="control-row">
                    <label for="pid-fopdt-delay">"Delay"</label>
                    <output>{move || format!("{:.2} s", fopdt_delay.get())}</output>
                    <input
                        id="pid-fopdt-delay"
                        type="range"
                        min="0.0"
                        max="3.0"
                        step="0.05"
                        prop:value=move || fopdt_delay.get().to_string()
                        on:input=move |ev| {
                            if let Ok(value) = event_target_value(&ev).parse::<f64>() {
                                set_fopdt_delay.set(value.max(0.0));
                            }
                        }
                    />
                </div>
            </>
        }
        .into_any(),
        PidPlantKind::Sopdt => view! {
            <>
                <div class="control-row">
                    <label for="pid-sopdt-gain">"Plant gain"</label>
                    <output>{move || format!("{:.2}", sopdt_gain.get())}</output>
                    <input
                        id="pid-sopdt-gain"
                        type="range"
                        min="0.4"
                        max="3.0"
                        step="0.05"
                        prop:value=move || sopdt_gain.get().to_string()
                        on:input=move |ev| {
                            if let Ok(value) = event_target_value(&ev).parse::<f64>() {
                                set_sopdt_gain.set(value.max(0.4));
                            }
                        }
                    />
                </div>

                <div class="control-row">
                    <label for="pid-sopdt-tau1">"Slow lag"</label>
                    <output>{move || format!("{:.2} s", sopdt_time_constant_1.get())}</output>
                    <input
                        id="pid-sopdt-tau1"
                        type="range"
                        min="0.6"
                        max="8.0"
                        step="0.1"
                        prop:value=move || sopdt_time_constant_1.get().to_string()
                        on:input=move |ev| {
                            if let Ok(value) = event_target_value(&ev).parse::<f64>() {
                                set_sopdt_time_constant_1.set(value.max(0.6));
                            }
                        }
                    />
                </div>

                <div class="control-row">
                    <label for="pid-sopdt-tau2">"Fast lag"</label>
                    <output>{move || format!("{:.2} s", sopdt_time_constant_2.get())}</output>
                    <input
                        id="pid-sopdt-tau2"
                        type="range"
                        min="0.2"
                        max="3.0"
                        step="0.05"
                        prop:value=move || sopdt_time_constant_2.get().to_string()
                        on:input=move |ev| {
                            if let Ok(value) = event_target_value(&ev).parse::<f64>() {
                                set_sopdt_time_constant_2.set(value.max(0.2));
                            }
                        }
                    />
                </div>

                <div class="control-row">
                    <label for="pid-sopdt-delay">"Delay"</label>
                    <output>{move || format!("{:.2} s", sopdt_delay.get())}</output>
                    <input
                        id="pid-sopdt-delay"
                        type="range"
                        min="0.0"
                        max="2.5"
                        step="0.05"
                        prop:value=move || sopdt_delay.get().to_string()
                        on:input=move |ev| {
                            if let Ok(value) = event_target_value(&ev).parse::<f64>() {
                                set_sopdt_delay.set(value.max(0.0));
                            }
                        }
                    />
                </div>
            </>
        }
        .into_any(),
        PidPlantKind::GeneralLinear => view! {
            <>
                <div class="control-row">
                    <label for="pid-general-gain">"Plant gain"</label>
                    <output>{move || format!("{:.2}", general_gain.get())}</output>
                    <input
                        id="pid-general-gain"
                        type="range"
                        min="0.6"
                        max="3.2"
                        step="0.05"
                        prop:value=move || general_gain.get().to_string()
                        on:input=move |ev| {
                            if let Ok(value) = event_target_value(&ev).parse::<f64>() {
                                set_general_gain.set(value.max(0.6));
                            }
                        }
                    />
                </div>

                <div class="control-row">
                    <label for="pid-general-zero">"Zero location"</label>
                    <output>{move || format!("{:.2}", general_zero.get())}</output>
                    <input
                        id="pid-general-zero"
                        type="range"
                        min="0.2"
                        max="1.4"
                        step="0.05"
                        prop:value=move || general_zero.get().to_string()
                        on:input=move |ev| {
                            if let Ok(value) = event_target_value(&ev).parse::<f64>() {
                                set_general_zero.set(value.max(0.2));
                            }
                        }
                    />
                </div>
            </>
        }
        .into_any(),
    };

    let tuning_controls = move || match plant_kind.get() {
        PidPlantKind::Fopdt => view! {
            <div class="control-row">
                <label for="pid-fopdt-lambda">"SIMC lambda"</label>
                <output>{move || format!("{:.2} s", fopdt_lambda.get())}</output>
                <input
                    id="pid-fopdt-lambda"
                    type="range"
                    min="0.4"
                    max="8.0"
                    step="0.1"
                    prop:value=move || fopdt_lambda.get().to_string()
                    on:input=move |ev| {
                        if let Ok(value) = event_target_value(&ev).parse::<f64>() {
                            set_fopdt_lambda.set(value.max(0.4));
                        }
                    }
                />
            </div>
        }
        .into_any(),
        PidPlantKind::Sopdt => view! {
            <div class="control-row">
                <label for="pid-sopdt-lambda">"SIMC lambda"</label>
                <output>{move || format!("{:.2} s", sopdt_lambda.get())}</output>
                <input
                    id="pid-sopdt-lambda"
                    type="range"
                    min="0.4"
                    max="8.0"
                    step="0.1"
                    prop:value=move || sopdt_lambda.get().to_string()
                    on:input=move |ev| {
                        if let Ok(value) = event_target_value(&ev).parse::<f64>() {
                            set_sopdt_lambda.set(value.max(0.4));
                        }
                    }
                />
            </div>
        }
        .into_any(),
        PidPlantKind::GeneralLinear => view! {
            <>
                <div class="control-row">
                    <label for="pid-general-crossover">"Target crossover"</label>
                    <output>{move || format!("{:.2} rad/s", general_crossover.get())}</output>
                    <input
                        id="pid-general-crossover"
                        type="range"
                        min="0.2"
                        max="2.5"
                        step="0.05"
                        prop:value=move || general_crossover.get().to_string()
                        on:input=move |ev| {
                            if let Ok(value) = event_target_value(&ev).parse::<f64>() {
                                set_general_crossover.set(value.max(0.2));
                            }
                        }
                    />
                </div>

                <div class="control-row">
                    <label for="pid-general-phase-margin">"Target phase margin"</label>
                    <output>{move || format!("{:.0} deg", general_phase_margin.get())}</output>
                    <input
                        id="pid-general-phase-margin"
                        type="range"
                        min="35.0"
                        max="85.0"
                        step="1.0"
                        prop:value=move || general_phase_margin.get().to_string()
                        on:input=move |ev| {
                            if let Ok(value) = event_target_value(&ev).parse::<f64>() {
                                set_general_phase_margin.set(value.clamp(35.0, 85.0));
                            }
                        }
                    />
                </div>
            </>
        }
        .into_any(),
    };

    view! {
        <div class="page">
            <header class="page-header">
                <p class="eyebrow">"Synthesis"</p>
                <h1>"PID Design Explorer"</h1>
                <p>
                    "Switch between low-order process models and a higher-order linear plant, then inspect how the"
                    " tuned closed-loop response and control effort change as you enable or disable the P, I, and D"
                    " terms."
                </p>
            </header>

            <div class="control-layout">
                <aside class="control-card">
                    <section>
                        <h2>"Plant family"</h2>
                        <p class="section-copy">
                            {move || plant_family_copy(plant_kind.get())}
                        </p>

                        <div class="control-row">
                            <label for="pid-plant-kind">"Plant model"</label>
                            <select
                                id="pid-plant-kind"
                                prop:value=move || plant_kind.get().as_key().to_string()
                                on:change=move |ev| {
                                    set_plant_kind.set(PidPlantKind::from_key(&event_target_value(&ev)));
                                }
                            >
                                <option value="fopdt">"FOPDT"</option>
                                <option value="sopdt">"SOPDT"</option>
                                <option value="general-linear">"General linear plant"</option>
                            </select>
                        </div>

                        {plant_controls}
                    </section>

                    <section>
                        <h2>"PID terms"</h2>
                        <p class="section-copy">
                            "The tuning backend solves either a PI or PIDF problem. Disabled terms are zeroed after"
                            " tuning so you can compare `P`, `PI`, `PD`, and `PID` behavior on the same plant."
                        </p>
                        <div class="term-toggle-grid">
                            {term_toggle("pid-include-p", "P", include_p, set_include_p)}
                            {term_toggle("pid-include-i", "I", include_i, set_include_i)}
                            {term_toggle("pid-include-d", "D", include_d, set_include_d)}
                        </div>
                    </section>

                    <section>
                        <h2>"Tuning target"</h2>
                        <p class="section-copy">{design_notes}</p>
                        {tuning_controls}
                    </section>

                    <section>
                        <h2>"Designed gains"</h2>
                        <p class="section-copy">{gains_text}</p>
                    </section>

                    <section>
                        <h2>"Run summary"</h2>
                        <p class="section-copy">{summary_text}</p>
                    </section>
                </aside>

                <div class="plots-grid wide">
                    <article class="plot-card">
                        <div class="plot-header">
                            <div>
                                <h2>"PID closed-loop traces"</h2>
                                <p>"Output and actuation views for the same tuned controller and term selection."</p>
                            </div>
                        </div>
                        <div class="plot-subsection">
                            <div class="plot-header">
                                <div>
                                    <h2>"Output response"</h2>
                                    <p>
                                        "Setpoint tracking under the tuned controller, with the plant's unit-step response"
                                        " shown for comparison."
                                    </p>
                                </div>
                            </div>
                            <div id="pid-design-output-plot" class="plot-surface"></div>
                        </div>

                        <div class="plot-subsection">
                            <div class="plot-header">
                                <div>
                                    <h2>"Control effort"</h2>
                                    <p>"Sampled PID command `u[k]` produced by the active P/I/D term selection."</p>
                                </div>
                            </div>
                            <div id="pid-design-control-plot" class="plot-surface"></div>
                        </div>
                    </article>
                </div>
            </div>
        </div>
    }
}

#[derive(Clone, Copy)]
enum PidPlotKind {
    Output,
    Control,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PidPlantKind {
    Fopdt,
    Sopdt,
    GeneralLinear,
}

impl PidPlantKind {
    fn as_key(self) -> &'static str {
        match self {
            Self::Fopdt => "fopdt",
            Self::Sopdt => "sopdt",
            Self::GeneralLinear => "general-linear",
        }
    }

    fn from_key(key: &str) -> Self {
        match key {
            "sopdt" => Self::Sopdt,
            "general-linear" => Self::GeneralLinear,
            _ => Self::Fopdt,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Fopdt => "FOPDT",
            Self::Sopdt => "SOPDT",
            Self::GeneralLinear => "general linear",
        }
    }
}

#[derive(Clone, Copy)]
struct PidDesignInputs {
    plant_kind: PidPlantKind,
    include_p: bool,
    include_i: bool,
    include_d: bool,
    fopdt_gain: f64,
    fopdt_time_constant: f64,
    fopdt_delay: f64,
    fopdt_lambda: f64,
    sopdt_gain: f64,
    sopdt_time_constant_1: f64,
    sopdt_time_constant_2: f64,
    sopdt_delay: f64,
    sopdt_lambda: f64,
    general_gain: f64,
    general_zero: f64,
    general_crossover: f64,
    general_phase_margin: f64,
}

impl Default for PidDesignInputs {
    fn default() -> Self {
        Self {
            plant_kind: PidPlantKind::Fopdt,
            include_p: true,
            include_i: true,
            include_d: false,
            fopdt_gain: 1.3,
            fopdt_time_constant: 3.2,
            fopdt_delay: 0.7,
            fopdt_lambda: 2.0,
            sopdt_gain: 1.1,
            sopdt_time_constant_1: 2.8,
            sopdt_time_constant_2: 0.9,
            sopdt_delay: 0.4,
            sopdt_lambda: 1.8,
            general_gain: 1.8,
            general_zero: 0.6,
            general_crossover: 0.9,
            general_phase_margin: 60.0,
        }
    }
}

struct PidDesignDemo {
    times: Vec<f64>,
    setpoint: Vec<f64>,
    open_loop_output: Vec<f64>,
    closed_loop_output: Vec<f64>,
    control_effort: Vec<f64>,
    active_pid: Pid<f64>,
    backend_family: PidControllerKind,
    active_terms: &'static str,
    tuning_summary: String,
    simulated_delay: f64,
    achieved_phase_margin_deg: Option<f64>,
}

struct DiscretePidPlant {
    a: Mat<f64>,
    b: Mat<f64>,
    c: Mat<f64>,
    d: Mat<f64>,
    sample_time: f64,
    input_delay_steps: usize,
    duration_seconds: f64,
}

impl DiscretePidPlant {
    fn simulate_open_loop_step(&self, n_steps: usize, input_level: f64) -> Vec<f64> {
        let mut state = vec![0.0; self.a.nrows()];
        let mut outputs = Vec::with_capacity(n_steps);
        let mut queue = VecDeque::from(vec![0.0; self.input_delay_steps]);

        for _ in 0..n_steps {
            let applied_input = if self.input_delay_steps == 0 {
                input_level
            } else {
                let delayed = queue.pop_front().unwrap_or(0.0);
                queue.push_back(input_level);
                delayed
            };
            outputs.push(self.output(&state, applied_input));
            state = self.step_state(&state, applied_input);
        }
        outputs
    }

    fn simulate_closed_loop(
        &self,
        pid: &Pid<f64>,
        n_steps: usize,
        setpoint: f64,
    ) -> Result<(Vec<f64>, Vec<f64>), String> {
        let mut plant_state = vec![0.0; self.a.nrows()];
        let mut pid_state = PidState::default();
        let mut outputs = Vec::with_capacity(n_steps);
        let mut controls = Vec::with_capacity(n_steps);
        let mut queue = VecDeque::from(vec![0.0; self.input_delay_steps]);

        for _ in 0..n_steps {
            let applied_input = if self.input_delay_steps == 0 {
                0.0
            } else {
                *queue.front().unwrap_or(&0.0)
            };
            let output = self.output(&plant_state, applied_input);
            let control = pid
                .step(&mut pid_state, self.sample_time, setpoint, output)
                .map_err(|err| err.to_string())?
                .saturated;

            let drive = if self.input_delay_steps == 0 {
                control
            } else {
                let delayed = queue.pop_front().unwrap_or(0.0);
                queue.push_back(control);
                delayed
            };

            outputs.push(output);
            controls.push(control);
            plant_state = self.step_state(&plant_state, drive);
        }

        Ok((outputs, controls))
    }

    fn recommended_steps(&self) -> usize {
        ((self.duration_seconds / self.sample_time).ceil() as usize).max(80)
    }

    fn output(&self, state: &[f64], input: f64) -> f64 {
        let mut value = 0.0;
        for row in 0..self.c.nrows() {
            for col in 0..self.c.ncols() {
                value += self.c[(row, col)] * state[col];
            }
        }
        value + self.d[(0, 0)] * input
    }

    fn step_state(&self, state: &[f64], input: f64) -> Vec<f64> {
        let mut next = vec![0.0; self.a.nrows()];
        for row in 0..self.a.nrows() {
            let mut value = self.b[(row, 0)] * input;
            for col in 0..self.a.ncols() {
                value += self.a[(row, col)] * state[col];
            }
            next[row] = value;
        }
        next
    }
}

fn build_pid_plot(inputs: PidDesignInputs, which: PidPlotKind) -> Plot {
    match run_pid_design_demo(inputs) {
        Ok(demo) => match which {
            PidPlotKind::Output => build_line_plot(
                "PID closed-loop response",
                "time (s)",
                "output",
                false,
                vec![
                    LineSeries::lines("setpoint", demo.times.clone(), demo.setpoint)
                        .with_dash(DashType::Dot),
                    LineSeries::lines("open-loop plant", demo.times.clone(), demo.open_loop_output)
                        .with_dash(DashType::Dash),
                    LineSeries::lines("closed-loop output", demo.times, demo.closed_loop_output)
                        .with_dash(DashType::Solid),
                ],
            ),
            PidPlotKind::Control => build_line_plot(
                "PID control effort",
                "time (s)",
                "u[k]",
                false,
                vec![LineSeries::lines(
                    "control effort",
                    demo.times,
                    demo.control_effort,
                )],
            ),
        },
        Err(message) => build_line_plot(&message, "", "", false, Vec::new()),
    }
}

fn pid_gains_text(inputs: PidDesignInputs) -> String {
    match run_pid_design_demo(inputs) {
        Ok(demo) => {
            let derivative_filter = demo
                .active_pid
                .derivative_filter()
                .map(|value| format!("{value:.3}"))
                .unwrap_or_else(|| "off".to_string());
            format!(
                "{} masked from {} tuning: Kp = {:.3}, Ki = {:.3}, Kd = {:.3}, derivative filter = {}.",
                demo.active_terms,
                controller_family_label(demo.backend_family),
                demo.active_pid.kp(),
                demo.active_pid.ki(),
                demo.active_pid.kd(),
                derivative_filter,
            )
        }
        Err(err) => format!("Design failed: {err}"),
    }
}

fn pid_summary(inputs: PidDesignInputs) -> String {
    match run_pid_design_demo(inputs) {
        Ok(demo) => {
            let delay_note = if demo.simulated_delay > 0.0 {
                format!(
                    " The sampled simulation applies {:.2} s of input delay.",
                    demo.simulated_delay
                )
            } else {
                String::new()
            };
            let phase_margin_note = match demo.achieved_phase_margin_deg {
                Some(phase_margin) => format!(" Achieved phase margin is {:.1} deg.", phase_margin),
                None => String::new(),
            };
            format!("{}{}{}", demo.tuning_summary, phase_margin_note, delay_note)
        }
        Err(err) => format!("PID design failed: {err}"),
    }
}

fn design_notes_copy(plant_kind: PidPlantKind) -> &'static str {
    match plant_kind {
        PidPlantKind::Fopdt => {
            "SIMC tuning on `K exp(-Ls) / (tau s + 1)`. Lower lambda is more aggressive and usually produces faster control effort growth."
        }
        PidPlantKind::Sopdt => {
            "SIMC tuning on `K exp(-Ls) / ((tau1 s + 1)(tau2 s + 1))`. The two lags change how much derivative action helps."
        }
        PidPlantKind::GeneralLinear => {
            "Frequency-domain tuning on a stable third-order state-space plant. Crossover and target phase margin drive the returned PI or PIDF controller."
        }
    }
}

fn plant_family_copy(plant_kind: PidPlantKind) -> &'static str {
    match plant_kind {
        PidPlantKind::Fopdt => {
            "A delayed first-order process model. This is the classic SIMC workflow for one dominant lag plus transport delay."
        }
        PidPlantKind::Sopdt => {
            "A delayed second-order process model with two real lags. This keeps more plant shape than FOPDT while staying interpretable."
        }
        PidPlantKind::GeneralLinear => {
            "A higher-order continuous SISO plant tuned from its exact linear model rather than a low-order delay surrogate."
        }
    }
}

fn controller_family_label(kind: PidControllerKind) -> &'static str {
    match kind {
        PidControllerKind::Pi => "PI",
        PidControllerKind::Pid => "PIDF",
    }
}

fn active_terms_label(include_p: bool, include_i: bool, include_d: bool) -> &'static str {
    match (include_p, include_i, include_d) {
        (true, false, false) => "P",
        (true, true, false) => "PI",
        (true, false, true) => "PD",
        (true, true, true) => "PID",
        (false, true, false) => "I",
        (false, false, true) => "D",
        (false, true, true) => "ID",
        (false, false, false) => "none",
    }
}

fn run_pid_design_demo(inputs: PidDesignInputs) -> Result<PidDesignDemo, String> {
    if !(inputs.include_p || inputs.include_i || inputs.include_d) {
        return Err("enable at least one of P, I, or D".to_string());
    }

    // Keep the sampled simulation noticeably finer than the tuned plant time
    // scales so the first few control moves and output bends are visible near
    // the start of the run.
    let simulation_dt = 0.01;
    let backend_family = if inputs.include_d {
        PidControllerKind::Pid
    } else {
        PidControllerKind::Pi
    };

    let (base_pid, plant, tuning_summary, achieved_phase_margin_deg) = match inputs.plant_kind {
        PidPlantKind::Fopdt => {
            let model = FopdtModel {
                gain: inputs.fopdt_gain,
                time_constant: inputs.fopdt_time_constant,
                delay: inputs.fopdt_delay,
            };
            let params =
                SimcPidParams::new(inputs.fopdt_lambda, backend_family, 10.0, AntiWindup::None)
                    .map_err(|err| err.to_string())?;
            let design = design_pid_from_fopdt(model, params).map_err(|err| err.to_string())?;
            let plant = make_delayed_process_plant(
                &[inputs.fopdt_gain],
                &[inputs.fopdt_time_constant, 1.0],
                inputs.fopdt_delay,
                simulation_dt,
                (10.0 * (inputs.fopdt_time_constant + inputs.fopdt_delay)).max(18.0),
            )?;
            (
                design.pid,
                plant,
                format!(
                    "SIMC {} tuning on {} with K = {:.2}, tau = {:.2} s, delay = {:.2} s, lambda = {:.2} s.",
                    controller_family_label(backend_family),
                    inputs.plant_kind.label(),
                    inputs.fopdt_gain,
                    inputs.fopdt_time_constant,
                    inputs.fopdt_delay,
                    inputs.fopdt_lambda,
                ),
                None,
            )
        }
        PidPlantKind::Sopdt => {
            let model = SopdtModel {
                gain: inputs.sopdt_gain,
                time_constant_1: inputs.sopdt_time_constant_1,
                time_constant_2: inputs.sopdt_time_constant_2,
                delay: inputs.sopdt_delay,
            };
            let params =
                SimcPidParams::new(inputs.sopdt_lambda, backend_family, 10.0, AntiWindup::None)
                    .map_err(|err| err.to_string())?;
            let design = design_pid_from_sopdt(model, params).map_err(|err| err.to_string())?;
            let denominator = [
                inputs.sopdt_time_constant_1 * inputs.sopdt_time_constant_2,
                inputs.sopdt_time_constant_1 + inputs.sopdt_time_constant_2,
                1.0,
            ];
            let plant = make_delayed_process_plant(
                &[inputs.sopdt_gain],
                &denominator,
                inputs.sopdt_delay,
                simulation_dt,
                (10.0
                    * (inputs.sopdt_time_constant_1
                        + inputs.sopdt_time_constant_2
                        + inputs.sopdt_delay))
                    .max(20.0),
            )?;
            (
                design.pid,
                plant,
                format!(
                    "SIMC {} tuning on {} with K = {:.2}, tau1 = {:.2} s, tau2 = {:.2} s, delay = {:.2} s, lambda = {:.2} s.",
                    controller_family_label(backend_family),
                    inputs.plant_kind.label(),
                    inputs.sopdt_gain,
                    inputs.sopdt_time_constant_1,
                    inputs.sopdt_time_constant_2,
                    inputs.sopdt_delay,
                    inputs.sopdt_lambda,
                ),
                None,
            )
        }
        PidPlantKind::GeneralLinear => {
            let continuous_plant = general_linear_plant(inputs.general_gain, inputs.general_zero)?;
            let params = FrequencyPidParams::new(
                inputs.general_crossover,
                inputs.general_phase_margin,
                backend_family,
                10.0,
                4.0,
                AntiWindup::None,
            )
            .map_err(|err| err.to_string())?;
            let design =
                design_pid_from_continuous_state_space_frequency(&continuous_plant, params)
                    .map_err(|err| err.to_string())?;
            let plant = discretize_general_linear_plant(&continuous_plant, simulation_dt, 20.0)?;
            (
                design.pid,
                plant,
                format!(
                    "Frequency-domain {} tuning on the third-order plant with gain {:.2}, zero at {:.2}, target crossover {:.2} rad/s, and target phase margin {:.0} deg.",
                    controller_family_label(backend_family),
                    inputs.general_gain,
                    inputs.general_zero,
                    inputs.general_crossover,
                    inputs.general_phase_margin,
                ),
                Some(design.achieved_phase_margin_deg),
            )
        }
    };

    let active_pid = mask_pid_terms(
        &base_pid,
        inputs.include_p,
        inputs.include_i,
        inputs.include_d,
    )?;

    let n_steps = plant.recommended_steps();
    let times = (0..n_steps)
        .map(|index| (index as f64) * plant.sample_time)
        .collect::<Vec<_>>();
    let setpoint = vec![1.0; n_steps];
    let open_loop_output = plant.simulate_open_loop_step(n_steps, 1.0);
    let (closed_loop_output, control_effort) =
        plant.simulate_closed_loop(&active_pid, n_steps, 1.0)?;

    Ok(PidDesignDemo {
        times,
        setpoint,
        open_loop_output,
        closed_loop_output,
        control_effort,
        active_pid,
        backend_family,
        active_terms: active_terms_label(inputs.include_p, inputs.include_i, inputs.include_d),
        tuning_summary,
        simulated_delay: (plant.input_delay_steps as f64) * plant.sample_time,
        achieved_phase_margin_deg,
    })
}

fn mask_pid_terms(
    base_pid: &Pid<f64>,
    include_p: bool,
    include_i: bool,
    include_d: bool,
) -> Result<Pid<f64>, String> {
    let kp = if include_p { base_pid.kp() } else { 0.0 };
    let ki = if include_i { base_pid.ki() } else { 0.0 };
    let kd = if include_d { base_pid.kd() } else { 0.0 };
    let derivative_filter = if include_d {
        base_pid.derivative_filter()
    } else {
        None
    };

    Pid::new(kp, ki, kd, derivative_filter, AntiWindup::None).map_err(|err| err.to_string())
}

fn make_delayed_process_plant(
    numerator: &[f64],
    denominator: &[f64],
    delay: f64,
    sample_time: f64,
    duration_seconds: f64,
) -> Result<DiscretePidPlant, String> {
    let tf = ContinuousTransferFunction::continuous(numerator.to_vec(), denominator.to_vec())
        .map_err(|err| err.to_string())?;
    let continuous = tf.to_state_space().map_err(|err| err.to_string())?;
    discretize_pid_plant(&continuous, sample_time, delay, duration_seconds)
}

fn discretize_general_linear_plant(
    continuous: &ContinuousStateSpace<f64>,
    sample_time: f64,
    duration_seconds: f64,
) -> Result<DiscretePidPlant, String> {
    discretize_pid_plant(continuous, sample_time, 0.0, duration_seconds)
}

fn discretize_pid_plant(
    continuous: &ContinuousStateSpace<f64>,
    sample_time: f64,
    delay: f64,
    duration_seconds: f64,
) -> Result<DiscretePidPlant, String> {
    let discrete = continuous
        .discretize(sample_time, DiscretizationMethod::ZeroOrderHold)
        .map_err(|err| err.to_string())?;
    Ok(DiscretePidPlant {
        a: discrete.a().to_owned(),
        b: discrete.b().to_owned(),
        c: discrete.c().to_owned(),
        d: discrete.d().to_owned(),
        sample_time,
        input_delay_steps: (delay / sample_time).round() as usize,
        duration_seconds,
    })
}

fn general_linear_plant(
    gain: f64,
    zero_location: f64,
) -> Result<ContinuousStateSpace<f64>, String> {
    let tf = ContinuousTransferFunction::continuous(
        vec![gain, gain * zero_location],
        vec![1.0, 5.1, 6.08, 1.68],
    )
    .map_err(|err| err.to_string())?;
    tf.to_state_space().map_err(|err| err.to_string())
}

#[cfg(test)]
mod tests {
    use super::{PidDesignInputs, PidPlantKind, run_pid_design_demo};

    #[test]
    fn pid_design_demo_runs_for_each_plant_family() {
        for plant_kind in [
            PidPlantKind::Fopdt,
            PidPlantKind::Sopdt,
            PidPlantKind::GeneralLinear,
        ] {
            let mut inputs = PidDesignInputs::default();
            inputs.plant_kind = plant_kind;
            let demo = run_pid_design_demo(inputs).unwrap();
            assert_eq!(demo.times.len(), demo.closed_loop_output.len());
            assert_eq!(demo.times.len(), demo.control_effort.len());
            assert!(demo.times.len() >= 80);
        }
    }

    #[test]
    fn pid_term_masking_zeroes_disabled_terms() {
        let mut inputs = PidDesignInputs::default();
        inputs.include_i = false;
        inputs.include_d = false;
        let demo = run_pid_design_demo(inputs).unwrap();
        assert_eq!(demo.active_pid.ki(), 0.0);
        assert_eq!(demo.active_pid.kd(), 0.0);
    }
}
