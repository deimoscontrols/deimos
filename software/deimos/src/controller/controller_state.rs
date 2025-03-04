use std::collections::BTreeMap;

use crate::peripheral::Peripheral;
use deimos_shared::peripherals::PeripheralId;

use crate::socket::SocketAddr;

use super::peripheral_state::*;

/// Controller metrics during operating state
#[derive(Default)]
pub(crate) struct ControllerOperatingMetrics {
    pub cycle_time_margin_ns: f64,
}

impl ControllerOperatingMetrics {
    pub fn names_to_write(&self) -> Vec<String> {
        const OUT: [&str; 1] = ["ctrl.cycle_time_margin_ns"];
        const {
            assert!(OUT.len() == Self::num_to_write());
        }
        Vec::<String>::from_iter(OUT.iter().map(|&x| x.to_owned()))
    }

    pub const fn num_to_write() -> usize {
        1
    }

    pub fn write_metric_values(&self, out: &mut [f64]) {
        out[0] = self.cycle_time_margin_ns;
    }
}

/// Controller run-time internal state that is not serialized
#[derive(Default)]
pub(crate) struct ControllerState {
    pub peripheral_state: BTreeMap<SocketAddr, PeripheralState>,
    names_to_write: Vec<String>,
    vals_to_write: Vec<f64>,
    num_to_write: usize,
    pub controller_metrics: ControllerOperatingMetrics,
}

impl ControllerState {
    /// Initialize new operating state
    pub fn new(
        peripherals: &BTreeMap<String, Box<dyn Peripheral>>,
        bind_result: &BTreeMap<SocketAddr, Box<dyn Peripheral>>,
    ) -> Self {
        // Map IDs to names
        let mut state = Self::default();
        let bound_pids = bind_result
            .keys()
            .map(|(_sid, pid)| *pid)
            .collect::<Vec<PeripheralId>>();
        let pid_name_map = BTreeMap::from_iter(peripherals.iter().map(|(name, p)| (p.id(), name)));

        // Make sure all expected peripherals were bound
        for p in peripherals.values() {
            let id = p.id();
            assert!(
                bound_pids.contains(&id),
                "Peripheral `{}` not found in bind result",
                pid_name_map[&id]
            );
        }

        for (addr, p) in bind_result.iter() {
            // Check if this is one of the peripherals we intend to operate
            let expected_this_peripheral = pid_name_map.contains_key(&p.id());
            if !expected_this_peripheral {
                println!("Unexpected peripheral with id {:?}", &p.id());
                continue;
            }

            // If this is an expected unit, add an entry for its state
            let (_sid, pid) = addr;
            let name = pid_name_map[pid];
            let ps = PeripheralState::new(name, *addr, p);
            state.peripheral_state.insert(*addr, ps);
        }

        // Set up I/O names and values
        state.names_to_write = state.build_names_to_write();
        state.vals_to_write = vec![0.0_f64; state.names_to_write.len()];

        // Peripheral metrics
        let mut n = 0;
        for state in state.peripheral_state.values() {
            n += state.metric_full_names.len();
        }

        // Controller metrics
        n += state.controller_metrics.names_to_write().len();

        state.num_to_write = n;

        state
    }

    /// Build metric names to write to database, combining entity names with field names
    fn build_names_to_write(&self) -> Vec<String> {
        let mut names = Vec::new();

        // Peripheral metrics
        for state in self.peripheral_state.values() {
            for chname in state.metric_full_names.iter() {
                names.push(chname.clone());
            }
        }

        // Controller metrics
        names.extend_from_slice(&self.controller_metrics.names_to_write()[..]);

        names
    }

    /// Get metric names to write to database
    pub fn get_names_to_write(&self) -> Vec<String> {
        self.names_to_write.clone()
    }

    /// Get metric values to write to database
    pub fn write_vals(&self, dst: &mut [f64]) {
        // Peripheral metrics
        let mut i = 0;
        for state in self.peripheral_state.values() {
            let nmetrics = state.metric_full_names.len();
            state.write_metric_values(&mut dst[i..i + nmetrics]);
            i += nmetrics;
        }

        // Controller metrics
        let nmetrics = ControllerOperatingMetrics::num_to_write();
        self.controller_metrics
            .write_metric_values(&mut dst[i..i + nmetrics]);
    }
}
