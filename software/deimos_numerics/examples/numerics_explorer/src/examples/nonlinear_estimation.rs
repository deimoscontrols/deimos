use crate::demo_signal::{colored_signal, gaussianish_signal};
use crate::plot_helpers::{LineSeries, build_line_plot};
use crate::plotly_support::use_plotly_chart;
use faer::{Col, Mat};
use leptos::prelude::*;
use numerical::control::estimation::DiscreteKalmanFilter;
use numerical::embedded::alloc::estimation::{
    DiscreteExtendedKalmanModel, DiscreteNonlinearModel, ExtendedKalmanFilter,
    UnscentedKalmanFilter,
};
use numerical::control::lti::DiscreteStateSpace;
use plotly::Plot;
use plotly::common::DashType;

/// Interactive nonlinear-estimation page comparing a fixed-linearization
/// Kalman filter, an EKF, and a UKF on the same tracking problem.
#[component]
pub fn NonlinearEstimationPage() -> impl IntoView {
    let (plant_kind, set_plant_kind) = signal(TrackingPlantKind::SlantRange);
    let (sensor_height, set_sensor_height) = signal(8.0_f64);
    let (linear_reference, set_linear_reference) = signal(4.0_f64);
    let (true_process_noise, set_true_process_noise) = signal(0.18_f64);
    let (true_measurement_noise, set_true_measurement_noise) = signal(0.35_f64);
    let (assumed_process_noise, set_assumed_process_noise) = signal(0.18_f64);
    let (assumed_measurement_noise, set_assumed_measurement_noise) = signal(0.35_f64);
    let (pin_assumptions, set_pin_assumptions) = signal(true);

    let effective_assumed_process_noise = Memo::new(move |_| {
        if pin_assumptions.get() {
            true_process_noise.get()
        } else {
            assumed_process_noise.get()
        }
    });
    let effective_assumed_measurement_noise = Memo::new(move |_| {
        if pin_assumptions.get() {
            true_measurement_noise.get()
        } else {
            assumed_measurement_noise.get()
        }
    });

    use_plotly_chart("nonlinear-estimator-position-plot", move || {
        build_nonlinear_estimation_plot(
            plant_kind.get(),
            sensor_height.get(),
            linear_reference.get(),
            true_process_noise.get(),
            true_measurement_noise.get(),
            effective_assumed_process_noise.get(),
            effective_assumed_measurement_noise.get(),
            NonlinearEstimationPlot::Position,
        )
    });
    use_plotly_chart("nonlinear-estimator-error-plot", move || {
        build_nonlinear_estimation_plot(
            plant_kind.get(),
            sensor_height.get(),
            linear_reference.get(),
            true_process_noise.get(),
            true_measurement_noise.get(),
            effective_assumed_process_noise.get(),
            effective_assumed_measurement_noise.get(),
            NonlinearEstimationPlot::Error,
        )
    });

    let summary = move || {
        nonlinear_estimation_summary(
            plant_kind.get(),
            sensor_height.get(),
            linear_reference.get(),
            true_process_noise.get(),
            true_measurement_noise.get(),
            effective_assumed_process_noise.get(),
            effective_assumed_measurement_noise.get(),
        )
    };

    view! {
        <div class="page">
            <header class="page-header">
                <p class="eyebrow">"Estimation"</p>
                <h1>"Nonlinear Tracking Workbench"</h1>
                <p>
                    "A constant-velocity target is observed through selectable linear and nonlinear sensors."
                    " The comparison uses the same truth trajectory, command input, and measurement sequence"
                    " for a fixed-linearization Kalman filter, an EKF, and a UKF."
                </p>
            </header>

            <div class="control-layout">
                <aside class="control-card">
                    <section>
                        <h2>"Settings"</h2>
                        <p class="section-copy">
                            "Choose the measurement model, then tune the world and filter assumptions on the same run."
                            " The fixed-linearization Kalman baseline always linearizes the chosen sensor at one fixed"
                            " reference position."
                        </p>

                        <div class="control-row">
                            <label for="nonlinear-plant-kind">"Plant / sensor model"</label>
                            <select
                                id="nonlinear-plant-kind"
                                prop:value=move || plant_kind.get().as_key().to_string()
                                on:change=move |ev| {
                                    let next_kind =
                                        TrackingPlantKind::from_key(&event_target_value(&ev));
                                    set_plant_kind.set(next_kind);
                                    match next_kind {
                                        TrackingPlantKind::Linear | TrackingPlantKind::SlantRange => {
                                            set_sensor_height.set(8.0);
                                            set_linear_reference.set(4.0);
                                        }
                                        TrackingPlantKind::Quintic => {
                                            set_sensor_height.set(2.0);
                                            set_linear_reference.set(4.0);
                                        }
                                    }
                                }
                            >
                                <option value="linear">"Linear position sensor"</option>
                                <option value="slant-range">"Slant-range sensor"</option>
                                <option value="quintic">"Fifth-order sensor distortion"</option>
                            </select>
                        </div>

                        <div class="control-row">
                            <label for="nonlinear-sensor-height">"Sensor height / scale"</label>
                            <output>{move || format!("{:.2}", sensor_height.get())}</output>
                            <input
                                id="nonlinear-sensor-height"
                                type="range"
                                min="2.0"
                                max="12.0"
                                step="0.1"
                                prop:value=move || sensor_height.get().to_string()
                                prop:disabled=move || plant_kind.get() == TrackingPlantKind::Linear
                                on:input=move |ev| {
                                    if let Ok(value) = event_target_value(&ev).parse::<f64>() {
                                        set_sensor_height.set(value.max(2.0));
                                    }
                                }
                            />
                        </div>

                        <div class="control-row">
                            <label for="nonlinear-linear-reference">"Linearization reference"</label>
                            <output>{move || format!("{:.2}", linear_reference.get())}</output>
                            <input
                                id="nonlinear-linear-reference"
                                type="range"
                                min="1.0"
                                max="12.0"
                                step="0.1"
                                prop:value=move || linear_reference.get().to_string()
                                on:input=move |ev| {
                                    if let Ok(value) = event_target_value(&ev).parse::<f64>() {
                                        set_linear_reference.set(value.max(1.0));
                                    }
                                }
                            />
                        </div>

                        <div class="control-row">
                            <label for="nonlinear-true-process-noise">"True process noise"</label>
                            <output>{move || format!("{:.3}", true_process_noise.get())}</output>
                            <input
                                id="nonlinear-true-process-noise"
                                type="range"
                                min="0.02"
                                max="2.00"
                                step="0.01"
                                prop:value=move || true_process_noise.get().to_string()
                                on:input=move |ev| {
                                    if let Ok(value) = event_target_value(&ev).parse::<f64>() {
                                        set_true_process_noise.set(value.max(0.02));
                                    }
                                }
                            />
                        </div>

                        <div class="control-row">
                            <label for="nonlinear-true-measurement-noise">"True measurement noise"</label>
                            <output>{move || format!("{:.3}", true_measurement_noise.get())}</output>
                            <input
                                id="nonlinear-true-measurement-noise"
                                type="range"
                                min="0.05"
                                max="2.00"
                                step="0.01"
                                prop:value=move || true_measurement_noise.get().to_string()
                                on:input=move |ev| {
                                    if let Ok(value) = event_target_value(&ev).parse::<f64>() {
                                        set_true_measurement_noise.set(value.max(0.05));
                                    }
                                }
                            />
                        </div>
                    </section>

                    <section>
                        <h2>"Filter assumptions"</h2>
                        <p class="section-copy">
                            "When the assumptions are pinned, all three filters use matched process and measurement"
                            " covariances. Unlock the sliders to study mismatch. The fixed-linearization KF keeps one"
                            " constant measurement Jacobian, while the EKF and UKF use the same nonlinear model directly."
                        </p>

                        <div class="control-row">
                            <label for="nonlinear-pin-assumptions">"Pin assumptions to truth"</label>
                            <input
                                id="nonlinear-pin-assumptions"
                                type="checkbox"
                                prop:checked=move || pin_assumptions.get()
                                on:change=move |ev| set_pin_assumptions.set(event_target_checked(&ev))
                            />
                        </div>

                        <div class="control-row">
                            <label for="nonlinear-assumed-process-noise">"Assumed process noise"</label>
                            <output>{move || format!("{:.3}", effective_assumed_process_noise.get())}</output>
                            <input
                                id="nonlinear-assumed-process-noise"
                                type="range"
                                min="0.02"
                                max="2.00"
                                step="0.01"
                                prop:value=move || effective_assumed_process_noise.get().to_string()
                                prop:disabled=move || pin_assumptions.get()
                                on:input=move |ev| {
                                    if let Ok(value) = event_target_value(&ev).parse::<f64>() {
                                        set_assumed_process_noise.set(value.max(0.02));
                                    }
                                }
                            />
                        </div>

                        <div class="control-row">
                            <label for="nonlinear-assumed-measurement-noise">"Assumed measurement noise"</label>
                            <output>{move || format!("{:.3}", effective_assumed_measurement_noise.get())}</output>
                            <input
                                id="nonlinear-assumed-measurement-noise"
                                type="range"
                                min="0.05"
                                max="2.00"
                                step="0.01"
                                prop:value=move || effective_assumed_measurement_noise.get().to_string()
                                prop:disabled=move || pin_assumptions.get()
                                on:input=move |ev| {
                                    if let Ok(value) = event_target_value(&ev).parse::<f64>() {
                                        set_assumed_measurement_noise.set(value.max(0.05));
                                    }
                                }
                            />
                        </div>
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
                                <h2>"Nonlinear tracking traces"</h2>
                                <p>"Trajectory and error views for the same linearized KF, EKF, and UKF run."</p>
                            </div>
                        </div>
                        <div class="plot-subsection">
                            <div class="plot-header">
                                <div>
                                    <h2>"Position estimate"</h2>
                                    <p>
                                        "Truth and the three competing filters on the same tracking run."
                                    </p>
                                </div>
                            </div>
                            <div id="nonlinear-estimator-position-plot" class="plot-surface"></div>
                        </div>

                        <div class="plot-subsection">
                            <div class="plot-header">
                                <div>
                                    <h2>"Absolute error"</h2>
                                    <p>"Position-estimation error on the same tracking run."</p>
                                </div>
                            </div>
                            <div id="nonlinear-estimator-error-plot" class="plot-surface"></div>
                        </div>
                    </article>

                    <section class="home-grid">
                        <article class="home-card">
                            <h2>"Tracking problem"</h2>
                            <p class="section-copy">
                                "The state is `[position, velocity]^T` with constant-velocity dynamics driven by an"
                                " acceleration command plus an unmodeled acceleration disturbance. Only the measurement"
                                " model changes across the three plant options."
                            </p>
                        </article>

                        <article class="home-card">
                            <h2>"Measurement model"</h2>
                            <p class="section-copy">
                                {move || plant_kind.get().measurement_copy()}
                            </p>
                        </article>

                        <article class="home-card">
                            <h2>"How To Use It"</h2>
                            <p class="section-copy">
                                {move || plant_kind.get().usage_copy()}
                            </p>
                        </article>
                    </section>
                </div>
            </div>
        </div>
    }
}

#[derive(Clone, Copy)]
enum NonlinearEstimationPlot {
    Position,
    Error,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TrackingPlantKind {
    Linear,
    SlantRange,
    Quintic,
}

impl TrackingPlantKind {
    fn as_key(self) -> &'static str {
        match self {
            Self::Linear => "linear",
            Self::SlantRange => "slant-range",
            Self::Quintic => "quintic",
        }
    }

    fn from_key(key: &str) -> Self {
        match key {
            "linear" => Self::Linear,
            "quintic" => Self::Quintic,
            _ => Self::SlantRange,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Linear => "linear position sensor",
            Self::SlantRange => "slant-range sensor",
            Self::Quintic => "fifth-order sensor distortion",
        }
    }

    fn measurement_copy(self) -> &'static str {
        match self {
            Self::Linear => {
                "The measurement is `y_k = position_k + n_k`, so this case is fully linear. The sensor-height / scale control is unused here; it matters only for the nonlinear sensor options. The measurement error mixes deterministic Gaussian-like jitter with a smaller colored bias term."
            }
            Self::SlantRange => {
                "The measurement is `y_k = sqrt(position_k^2 + h^2) + n_k`, where `h` is the sensor-height offset above the track. This is a moderate nonlinearity: the linear KF can lag if its fixed reference is poor, while the EKF and UKF usually track closely. The measurement error again includes Gaussian-like jitter plus a smaller colored bias."
            }
            Self::Quintic => {
                "The measurement is `y_k = x_k + 0.35 x_k^5 / s^4 + n_k`, where `s` is the displayed scale parameter. This preserves the sign of position but injects stronger higher-order curvature than the slant-range model, which is where the UKF should separate most clearly from the EKF. The additive sensor noise keeps the raw measurement visibly noisy on top of that distortion. The quintic preset intentionally uses a low scale so the UKF advantage is visible."
            }
        }
    }

    fn usage_copy(self) -> &'static str {
        match self {
            Self::Linear => {
                "Use the linear sensor as a sanity check. All three filters should nearly coincide, which shows that the nonlinear estimators collapse back to ordinary Kalman behavior when the sensor is actually linear."
            }
            Self::SlantRange => {
                "Increase sensor height or move the fixed linearization point away from the target path to make the slant-range sensor more nonlinear from the linear KF's point of view. The EKF should recover most of that gap, while the UKF will usually stay close on this moderate nonlinearity."
            }
            Self::Quintic => {
                "This sensor exaggerates higher-order curvature. The built-in quintic preset drops the scale to `2.0`, which makes the sigma-point treatment visibly better than EKF linearization on the same run. The page compares only the filter trajectories and errors, rather than trying to coerce the nonlinear sensor output into a pseudo-position trace. Move the fixed linearization point away from the truth path or unlock the assumption sliders to add even more mismatch."
            }
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct RangeTrackingModel {
    dt: f64,
    sensor_height: f64,
    plant_kind: TrackingPlantKind,
}

impl DiscreteNonlinearModel<f64> for RangeTrackingModel {
    fn state_dim(&self) -> usize {
        2
    }

    fn input_dim(&self) -> usize {
        1
    }

    fn output_dim(&self) -> usize {
        1
    }

    fn transition(&self, x: &[f64], u: &[f64], next_state: &mut [f64]) {
        let dt = self.dt;
        let acceleration = u[0];
        next_state[0] = x[0] + dt * x[1] + 0.5 * dt * dt * acceleration;
        next_state[1] = x[1] + dt * acceleration;
    }

    fn output(&self, x: &[f64], _u: &[f64], output: &mut [f64]) {
        output[0] = measurement_value(self.plant_kind, x[0], self.sensor_height);
    }
}

impl DiscreteExtendedKalmanModel<f64> for RangeTrackingModel {
    fn transition_jacobian(&self, _x: &[f64], _u: &[f64], jacobian: &mut Mat<f64>) {
        let dt = self.dt;
        jacobian[(0, 0)] = 1.0;
        jacobian[(0, 1)] = dt;
        jacobian[(1, 0)] = 0.0;
        jacobian[(1, 1)] = 1.0;
    }

    fn output_jacobian(&self, x: &[f64], _u: &[f64], jacobian: &mut Mat<f64>) {
        let slope = measurement_slope(self.plant_kind, x[0], self.sensor_height);
        jacobian[(0, 0)] = slope;
        jacobian[(0, 1)] = 0.0;
    }
}

struct NonlinearEstimationDemo {
    times: Vec<f64>,
    truth_position: Vec<f64>,
    linear_position: Vec<f64>,
    ekf_position: Vec<f64>,
    ukf_position: Vec<f64>,
    linear_abs_error: Vec<f64>,
    ekf_abs_error: Vec<f64>,
    ukf_abs_error: Vec<f64>,
    linear_rmse: f64,
    ekf_rmse: f64,
    ukf_rmse: f64,
    best_filter: &'static str,
}

fn build_nonlinear_estimation_plot(
    plant_kind: TrackingPlantKind,
    sensor_height: f64,
    linear_reference: f64,
    true_process_noise: f64,
    true_measurement_noise: f64,
    assumed_process_noise: f64,
    assumed_measurement_noise: f64,
    which: NonlinearEstimationPlot,
) -> Plot {
    match run_nonlinear_estimation_demo(
        plant_kind,
        sensor_height,
        linear_reference,
        true_process_noise,
        true_measurement_noise,
        assumed_process_noise,
        assumed_measurement_noise,
    ) {
        Ok(demo) => match which {
            NonlinearEstimationPlot::Position => build_line_plot(
                "Position estimate",
                "time (s)",
                "position",
                false,
                vec![
                    LineSeries::lines("truth", demo.times.clone(), demo.truth_position),
                    LineSeries::lines("linearized KF", demo.times.clone(), demo.linear_position)
                        .with_dash(DashType::Dash),
                    LineSeries::lines("EKF", demo.times.clone(), demo.ekf_position)
                        .with_dash(DashType::DashDot),
                    LineSeries::lines("UKF", demo.times, demo.ukf_position)
                        .with_dash(DashType::LongDash),
                ],
            ),
            NonlinearEstimationPlot::Error => build_line_plot(
                "Absolute position error",
                "time (s)",
                "|estimate - truth|",
                false,
                vec![
                    LineSeries::lines("linearized KF", demo.times.clone(), demo.linear_abs_error)
                        .with_dash(DashType::Dash),
                    LineSeries::lines("EKF", demo.times.clone(), demo.ekf_abs_error)
                        .with_dash(DashType::DashDot),
                    LineSeries::lines("UKF", demo.times, demo.ukf_abs_error)
                        .with_dash(DashType::LongDash),
                ],
            ),
        },
        Err(message) => build_line_plot(&message, "time (s)", "", false, Vec::new()),
    }
}

fn nonlinear_estimation_summary(
    plant_kind: TrackingPlantKind,
    sensor_height: f64,
    linear_reference: f64,
    true_process_noise: f64,
    true_measurement_noise: f64,
    assumed_process_noise: f64,
    assumed_measurement_noise: f64,
) -> String {
    match run_nonlinear_estimation_demo(
        plant_kind,
        sensor_height,
        linear_reference,
        true_process_noise,
        true_measurement_noise,
        assumed_process_noise,
        assumed_measurement_noise,
    ) {
        Ok(demo) => format!(
            "Plant: {}. Sensor height / scale {:.2}, fixed linearization at position {:.2}. Truth uses process {:.3} and measurement {:.3}; filters assume process {:.3} and measurement {:.3}. RMSE: linearized KF {:.3}, EKF {:.3}, UKF {:.3}. Best on this run: {}.",
            plant_kind.label(),
            sensor_height,
            linear_reference,
            true_process_noise,
            true_measurement_noise,
            assumed_process_noise,
            assumed_measurement_noise,
            demo.linear_rmse,
            demo.ekf_rmse,
            demo.ukf_rmse,
            demo.best_filter,
        ),
        Err(err) => format!("Nonlinear tracking setup failed: {err}"),
    }
}

fn run_nonlinear_estimation_demo(
    plant_kind: TrackingPlantKind,
    sensor_height: f64,
    linear_reference: f64,
    true_process_noise: f64,
    true_measurement_noise: f64,
    assumed_process_noise: f64,
    assumed_measurement_noise: f64,
) -> Result<NonlinearEstimationDemo, String> {
    let dt = 0.1;
    let nonlinear_model = RangeTrackingModel {
        dt,
        sensor_height,
        plant_kind,
    };

    let linear_measurement_slope = measurement_slope(plant_kind, linear_reference, sensor_height);
    let linear_measurement_bias = measurement_value(plant_kind, linear_reference, sensor_height)
        - linear_measurement_slope * linear_reference;

    let linear_system = DiscreteStateSpace::new(
        Mat::from_fn(2, 2, |row, col| match (row, col) {
            (0, 0) => 1.0,
            (0, 1) => dt,
            (1, 0) => 0.0,
            (1, 1) => 1.0,
            _ => 0.0,
        }),
        Mat::from_fn(2, 1, |row, _| if row == 0 { 0.5 * dt * dt } else { dt }),
        Mat::from_fn(1, 2, |_, col| {
            if col == 0 {
                linear_measurement_slope
            } else {
                0.0
            }
        }),
        Mat::zeros(1, 1),
        dt,
    )
    .map_err(|err| err.to_string())?;

    let assumed_q = assumed_process_noise * assumed_process_noise;
    let w = Mat::from_fn(2, 2, |row, col| match (row, col) {
        (0, 0) => 0.25 * dt.powi(4) * assumed_q,
        (0, 1) | (1, 0) => 0.5 * dt.powi(3) * assumed_q,
        (1, 1) => dt.powi(2) * assumed_q,
        _ => 0.0,
    });
    let v = Mat::from_fn(1, 1, |_, _| {
        assumed_measurement_noise * assumed_measurement_noise
    });

    let x_hat0 = Mat::from_fn(2, 1, |row, _| match row {
        0 => linear_reference,
        1 => -0.15,
        _ => 0.0,
    });
    let x_hat0_vec = Col::from_fn(2, |row| match row {
        0 => linear_reference,
        1 => -0.15,
        _ => 0.0,
    });
    let p0 = Mat::from_fn(2, 2, |row, col| {
        if row == col {
            if row == 0 { 6.0 } else { 1.5 }
        } else {
            0.0
        }
    });

    let mut linear_kf = DiscreteKalmanFilter::from_state_space(
        &linear_system,
        w.clone(),
        v.clone(),
        x_hat0.clone(),
        p0.clone(),
    )
    .map_err(|err| err.to_string())?;
    let mut ekf = ExtendedKalmanFilter::new(
        nonlinear_model,
        w.clone(),
        v.clone(),
        x_hat0_vec.clone(),
        p0.clone(),
    )
    .map_err(|err| err.to_string())?;
    let mut ukf = UnscentedKalmanFilter::new(nonlinear_model, w, v, x_hat0_vec, p0)
        .and_then(|filter| filter.with_sigma_parameters(0.45, 2.0, 0.0))
        .map_err(|err| err.to_string())?;

    let n_steps = 110;
    let mut truth = [6.0_f64, -0.25_f64];
    let mut times = Vec::with_capacity(n_steps);
    let mut truth_position = Vec::with_capacity(n_steps);
    let mut linear_position = Vec::with_capacity(n_steps);
    let mut ekf_position = Vec::with_capacity(n_steps);
    let mut ukf_position = Vec::with_capacity(n_steps);

    for step in 0..n_steps {
        let t = (step as f64) * dt;
        let command = tracking_command(step);
        let disturbance = true_process_noise * colored_signal(step, 0.23);
        let measurement_noise = gaussianish_signal(step, 29) + 0.20 * colored_signal(step, 1.41);
        let measurement = measurement_value(plant_kind, truth[0], sensor_height)
            + true_measurement_noise * measurement_noise;

        let input = Mat::from_fn(1, 1, |_, _| command);
        let linear_measurement = Mat::from_fn(1, 1, |_, _| measurement - linear_measurement_bias);
        let input_slice = [command];
        let measurement_slice = [measurement];

        let linear_update = linear_kf
            .step(input.as_ref(), linear_measurement.as_ref())
            .map_err(|err| err.to_string())?;
        let ekf_update = ekf
            .step(&input_slice, &measurement_slice)
            .map_err(|err| err.to_string())?;
        let ukf_update = ukf
            .step(&input_slice, &measurement_slice)
            .map_err(|err| err.to_string())?;

        times.push(t);
        truth_position.push(truth[0]);
        linear_position.push(linear_update.state[(0, 0)]);
        ekf_position.push(ekf_update.state[0]);
        ukf_position.push(ukf_update.state[0]);

        let applied_acceleration = command + disturbance;
        truth = [
            truth[0] + dt * truth[1] + 0.5 * dt * dt * applied_acceleration,
            truth[1] + dt * applied_acceleration,
        ];
    }

    let linear_abs_error = absolute_error(&linear_position, &truth_position);
    let ekf_abs_error = absolute_error(&ekf_position, &truth_position);
    let ukf_abs_error = absolute_error(&ukf_position, &truth_position);
    let linear_rmse = rmse(&linear_position, &truth_position);
    let ekf_rmse = rmse(&ekf_position, &truth_position);
    let ukf_rmse = rmse(&ukf_position, &truth_position);

    let best_filter = if linear_rmse <= ekf_rmse && linear_rmse <= ukf_rmse {
        "linearized KF"
    } else if ekf_rmse <= ukf_rmse {
        "EKF"
    } else {
        "UKF"
    };

    Ok(NonlinearEstimationDemo {
        times,
        truth_position,
        linear_position,
        ekf_position,
        ukf_position,
        linear_abs_error,
        ekf_abs_error,
        ukf_abs_error,
        linear_rmse,
        ekf_rmse,
        ukf_rmse,
        best_filter,
    })
}

fn measurement_value(plant_kind: TrackingPlantKind, position: f64, sensor_height: f64) -> f64 {
    match plant_kind {
        TrackingPlantKind::Linear => position,
        TrackingPlantKind::SlantRange => {
            (position * position + sensor_height * sensor_height).sqrt()
        }
        TrackingPlantKind::Quintic => {
            let scale = sensor_height.max(1.0);
            position + 0.35 * position.powi(5) / scale.powi(4)
        }
    }
}

fn measurement_slope(plant_kind: TrackingPlantKind, position: f64, sensor_height: f64) -> f64 {
    match plant_kind {
        TrackingPlantKind::Linear => 1.0,
        TrackingPlantKind::SlantRange => {
            let denom = (position * position + sensor_height * sensor_height).sqrt();
            if denom > 0.0 { position / denom } else { 0.0 }
        }
        TrackingPlantKind::Quintic => {
            let scale = sensor_height.max(1.0);
            1.0 + 1.75 * position.powi(4) / scale.powi(4)
        }
    }
}

fn absolute_error(estimate: &[f64], truth: &[f64]) -> Vec<f64> {
    estimate
        .iter()
        .zip(truth.iter())
        .map(|(estimate, truth)| (estimate - truth).abs())
        .collect()
}

fn rmse(estimate: &[f64], truth: &[f64]) -> f64 {
    let mse = estimate
        .iter()
        .zip(truth.iter())
        .map(|(estimate, truth)| {
            let error = estimate - truth;
            error * error
        })
        .sum::<f64>()
        / (estimate.len().max(1) as f64);
    mse.sqrt()
}

fn tracking_command(step: usize) -> f64 {
    let k = step as f64;
    0.10 * (0.09 * k).sin() + if step >= 30 { 0.10 } else { 0.0 }
        - if step >= 72 { 0.14 } else { 0.0 }
}

#[cfg(test)]
mod tests {
    use super::{TrackingPlantKind, run_nonlinear_estimation_demo};

    #[test]
    fn quintic_preset_gives_ukf_visible_advantage() {
        let demo = run_nonlinear_estimation_demo(
            TrackingPlantKind::Quintic,
            2.0,
            4.0,
            0.18,
            0.35,
            0.18,
            0.35,
        )
        .expect("demo should run");

        assert!(
            demo.ukf_rmse < 0.75 * demo.ekf_rmse,
            "expected UKF to beat EKF materially for the quintic preset, got ekf={} ukf={}",
            demo.ekf_rmse,
            demo.ukf_rmse
        );
    }
}
