use std::time::{Duration, Instant};

use crate::peripheral::Peripheral;
use deimos_shared::{peripherals::PeripheralId, states::OperatingMetrics};

use crate::socket::SocketAddr;

/// Peripheral control metrics
#[derive(Default, Clone, Copy, Debug)]
pub(crate) struct PeripheralMetrics {
    /// Last set of peripheral-side comm metrics
    pub operating_metrics: OperatingMetrics,

    /// Time at which the last in-order packet from this peripheral
    /// was received by the controller
    pub last_received_time_ns: i64,

    /// Latest packet arrival timing delta from target time
    pub raw_timing_delta_ns: f64,

    /// Filtered packet arrival timing delta from target time
    pub filtered_timing_delta_ns: f64,

    /// Requested change in peripheral's sampling period
    pub requested_period_delta_ns: f64,

    /// Requested change in peripheral's sampling phase
    pub requested_phase_delta_ns: f64,

    /// How many cycles has it been since we heard from this peripheral?
    pub loss_of_contact_counter: f64,

    /// How many packets behind is the peripheral?
    pub cycle_lag_count: f64,
}

pub(crate) enum ConnState {
    Binding {
        binding_timeout: Instant,
        reconnect_deadline: Option<Instant>,
    },
    Configuring {
        configuring_timeout: Instant,
        reconnect_deadline: Option<Instant>,
    },
    Operating(),
    Disconnected {
        deadline: Option<Instant>,
    },
}

/// Peripheral-specific metrics, readings, channel names, etc
pub(crate) struct PeripheralState {
    /// Ethernet address
    pub addr: SocketAddr,

    /// Device unique ID
    #[allow(dead_code)]
    pub id: PeripheralId,

    /// Device name
    pub name: String,

    //
    // Values updated live during operation
    //
    /// Has this peripheral received and acknowledged its pre-operation
    /// configuration frame yet?
    pub acknowledged_configuration: bool,

    /// Connection state tracking binding/configuring/operating timeouts.
    pub conn_state: ConnState,

    /// Timing and comm metrics
    pub metrics: PeripheralMetrics,

    //
    // Baked values not intended to change after init
    //
    /// Output channel names including peripheral name
    pub metric_full_names: Vec<String>,
}

impl PeripheralState {
    #[allow(clippy::borrowed_box)] // Fixing this with trait objects requires using Any
    pub fn new(
        name: &String,
        addr: SocketAddr,
        p: &Box<dyn Peripheral>,
        ctx: &crate::controller::context::ControllerCtx,
    ) -> Self {
        // Metric names are pretty manual
        let mut mnames = Vec::new();
        for orig in [
            "cycle_time_ns",
            "cycle_time_margin_ns",
            "raw_timing_delta_ns",
            "filtered_timing_delta_ns",
            "requested_period_delta_ns",
            "requested_phase_delta_ns",
            "loss_of_contact_counter",
            "cycle_lag_count",
        ]
        .iter()
        {
            mnames.push(format!("{name}.metrics.{orig}"))
        }

        let metrics = PeripheralMetrics::default();
        let acknowledged_configuration = false;
        let conn_state = ConnState::Binding {
            binding_timeout: Instant::now() + Duration::from_millis(ctx.binding_timeout_ms as u64),
            reconnect_deadline: None,
        };
        let id = p.id();

        Self {
            addr,
            id,
            name: name.to_owned(),
            acknowledged_configuration,
            conn_state,
            metrics,
            metric_full_names: mnames,
        }
    }

    pub fn write_metric_values(&self, out: &mut [f64]) {
        out[0] = self.metrics.operating_metrics.cycle_time_ns as f64;
        out[1] = self.metrics.operating_metrics.cycle_time_margin_ns as f64;
        out[2] = self.metrics.raw_timing_delta_ns;
        out[3] = self.metrics.filtered_timing_delta_ns;
        out[4] = self.metrics.requested_period_delta_ns;
        out[5] = self.metrics.requested_phase_delta_ns;
        out[6] = self.metrics.loss_of_contact_counter;
        out[7] = self.metrics.cycle_lag_count;
    }
}
