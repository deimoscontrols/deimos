use std::time::{Duration, Instant};

use crate::peripheral::Peripheral;
use deimos_shared::{peripherals::PeripheralId, states::OperatingMetrics};

use crate::socket::SocketAddr;

/// Number of metric slots `write_metric_values` populates per peripheral.
///
/// `Peripheral::metric_names()` and `Peripheral::metric_units()` defaults must
/// return exactly this many entries, and any override on a peripheral must
/// match the same length until `write_metric_values` is generalized to drive
/// from the metric name list. The invariant is asserted in
/// `PeripheralState::new`.
pub(crate) const PERIPHERAL_METRIC_COUNT: usize = 8;

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

    /// Has this peripheral produced at least one in-order operating packet?
    pub has_received_packet: bool,

    //
    // Baked values not intended to change after init
    //
    /// Output channel names including peripheral name
    pub metric_full_names: Vec<String>,

    /// Declared engineering units for each metric channel, parallel to
    /// `metric_full_names`. Populated from `Peripheral::metric_units` at init.
    pub metric_units: Vec<Option<String>>,
}

impl PeripheralState {
    #[allow(clippy::borrowed_box)] // Fixing this with trait objects requires using Any
    pub fn new(
        name: &String,
        addr: SocketAddr,
        p: &Box<dyn Peripheral>,
        ctx: &crate::controller::context::ControllerCtx,
    ) -> Self {
        // Metric names come from the peripheral; default impl returns the canonical
        // timing/health set, but a peripheral may override.
        let mnames = p
            .metric_names()
            .into_iter()
            .map(|orig| format!("{name}.metrics.{orig}"))
            .collect::<Vec<_>>();

        let munits = p.metric_units();
        assert_eq!(
            munits.len(),
            mnames.len(),
            "Peripheral `{name}`: metric_units().len() != metric_names().len()"
        );
        // `write_metric_values` writes exactly `PERIPHERAL_METRIC_COUNT` slots
        // by hardcoded index. A peripheral that overrides `metric_names()` to
        // a different length would silently corrupt the metric stream or panic
        // at write time. Lock the invariant here until `write_metric_values`
        // is generalized to drive from `metric_names()`.
        assert_eq!(
            mnames.len(),
            PERIPHERAL_METRIC_COUNT,
            "Peripheral `{name}`: metric_names() returned {} entries, expected {PERIPHERAL_METRIC_COUNT}. \
             Overriding metric_names() to a different length is currently unsupported \
             until write_metric_values is generalized.",
            mnames.len()
        );

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
            has_received_packet: false,
            metric_full_names: mnames,
            metric_units: munits,
        }
    }

    /// Write the `PERIPHERAL_METRIC_COUNT` canonical timing/health metrics into
    /// `out` by hardcoded positional index. The slot order matches the default
    /// `Peripheral::metric_names()` list. `out.len()` must be at least
    /// `PERIPHERAL_METRIC_COUNT`; the length-equality invariant with
    /// `metric_names()` is enforced in `PeripheralState::new`.
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
