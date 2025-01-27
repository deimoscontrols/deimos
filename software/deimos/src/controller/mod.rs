//! Control loop and integration with data pipeline and calc orchestrator

pub mod context;
mod controller_state;
mod peripheral_state;
mod timing;

use std::collections::BTreeMap;
use std::time::{Duration, Instant, SystemTime};

use context::ControllerCtx;
use core_affinity;

use serde::{Deserialize, Serialize};

use flaw::MedianFilter;

use deimos_shared::{
    calcs::Calc,
    peripherals::{parse_binding, Peripheral, PluginMap},
    states::*,
};
use thread_priority::DeadlineFlags;

use crate::dispatcher::Dispatcher;
use crate::orchestrator::Orchestrator;
use crate::socket::udp::UdpSuperSocket;
use crate::socket::{SuperSocket, SuperSocketAddr};
use controller_state::ControllerState;
use timing::TimingPID;

/// The controller implements the control loop,
/// synchronizes sample reporting time between the peripherals,
/// and dispatches measured data, calculations, and metrics to the data pipeline.
#[derive(Serialize, Deserialize)]
pub struct Controller {
    // Input config, which is passed to appendages during their init
    ctx: ControllerCtx,

    // Appendages
    sockets: Vec<Box<dyn SuperSocket>>,
    dispatchers: Vec<Box<dyn Dispatcher>>,
    peripherals: BTreeMap<String, Box<dyn Peripheral>>,
    orchestrator: Orchestrator,
}

impl Default for Controller {
    fn default() -> Self {
        // Include a UDP socket by default, but otherwise blank
        let sockets: Vec<Box<dyn SuperSocket>> = vec![Box::new(UdpSuperSocket::new())];

        let dispatchers = Vec::new();
        let peripherals = BTreeMap::new();
        let orchestrator = Orchestrator::default();
        let ctx = ControllerCtx::default();

        Self {
            ctx,
            sockets,
            dispatchers,
            peripherals,
            orchestrator,
        }
    }
}

impl Controller {
    /// Initialize a fresh controller with no dispatchers, peripherals, or calcs.
    /// A UDP socket is included by default, but can be removed.
    pub fn new(dt_ns: u32) -> Self {
        let mut c = Self::default();
        c.ctx.dt_ns = dt_ns;
        c.ctx.timeout_to_operating_ns = (dt_ns * 2).max(1_000_000);

        c
    }

    /// Register a hardware module
    pub fn add_peripheral(&mut self, name: &str, p: Box<dyn Peripheral>) {
        assert!(
            !self.peripherals.contains_key(name),
            "Peripheral name is duplicated"
        );
        // Add the standard set of calcs that come with this peripheral, if any
        self.orchestrator
            .add_calcs(p.standard_calcs(name.to_owned()));
        // Register the peripheral
        self.peripherals.insert(name.to_owned(), p);
    }

    /// Register a data pipeline dispatcher
    pub fn add_dispatcher(&mut self, dispatcher: Box<dyn Dispatcher>) {
        self.dispatchers.push(dispatcher);
    }

    /// Register a calc function
    pub fn add_calc(&mut self, name: &str, calc: Box<dyn Calc>) {
        self.orchestrator.add_calc(name, calc);
    }

    /// Register a socket
    pub fn add_socket(&mut self, socket: Box<dyn SuperSocket>) {
        self.sockets.push(socket);
    }

    /// Remove all peripherals
    pub fn clear_peripherals(&mut self) {
        self.peripherals.clear();
    }

    /// Remove all dispatchers
    pub fn clear_dispatchers(&mut self) {
        self.dispatchers.clear();
    }

    /// Remove all sockets
    pub fn clear_sockets(&mut self) {
        self.sockets.clear();
    }

    /// Remove all calcs and peripheral input sources
    pub fn clear_calcs(&mut self) {
        self.orchestrator.clear_calcs();
    }

    /// Connect an entry in the calc graph to a command to be sent to the peripheral
    pub fn set_peripheral_input_source(&mut self, input_field: &str, source_field: &str) {
        self.orchestrator
            .set_peripheral_input_source(input_field, source_field);
    }

    /// Request specific peripherals to bind or scan the network,
    /// giving `binding_timeout_ms` for peripherals to respond
    /// and requesting a window of `configuring_timeout_ms` after binding
    /// to provide configuration.
    /// ```text
    ///              binding timeout window
    ///                  /
    ///                 /           
    ///             |----|         timeout to operating
    ///             |--------------|
    ///             |      \
    /// sent binding|       \
    ///             |    configuring window
    ///    peripherals
    ///     transition
    /// to configuring            
    /// ```
    /// To broadcast scan for available peripherals, provide no addresses
    /// and set configuring_timeout_ms to 0.
    pub fn bind(
        &mut self,
        addresses: Option<&Vec<SuperSocketAddr>>,
        binding_timeout_ms: u16,
        configuring_timeout_ms: u16,
        plugins: Option<PluginMap>,
    ) -> BTreeMap<SuperSocketAddr, Box<dyn Peripheral>> {
        let mut buf = vec![0_u8; 1522];
        let mut available_peripherals = BTreeMap::new();

        let binding_msg = BindingInput {
            configuring_timeout_ms,
        };
        binding_msg.write_bytes(&mut buf[..BindingInput::BYTE_LEN]);

        if let Some(addresses) = addresses {
            // Bind specific modules with a (hopefully) nonzero timeout
            //    Send unicast request to bind
            for (socket_id, peripheral_id) in addresses.iter() {
                self.sockets[*socket_id]
                    .send(*peripheral_id, &buf[..BindingInput::BYTE_LEN])
                    .unwrap();
            }
        } else {
            // Bind any modules on the local network
            for socket in self.sockets.iter_mut() {
                socket.broadcast(&buf[..BindingInput::BYTE_LEN]).unwrap();
            }
        }

        //    Collect binding responses
        let start_of_binding = Instant::now();
        while start_of_binding.elapsed().as_millis() <= (binding_timeout_ms + 1) as u128 {
            for (sid, socket) in self.sockets.iter_mut().enumerate() {
                if let Some((_pid, _rxtime, recvd)) = socket.recv() {
                    // If this is from the right port and it's not capturing our own
                    // broadcast binding request, bind the module
                    // let recvd = &udp_buf[..BindingOutput::BYTE_LEN];
                    let amt = recvd.len();
                    if amt == BindingOutput::BYTE_LEN {
                        let binding_response = BindingOutput::read_bytes(recvd);
                        match parse_binding(&binding_response, &plugins) {
                            Ok(parsed) => {
                                let pid = parsed.id();
                                let addr = (sid, pid);
                                // Update the socket's address map
                                socket.update_map(pid);
                                // Update the controller's address map
                                available_peripherals.insert(addr, parsed);
                            }
                            Err(e) => println!("{e}"),
                        }
                    } else {
                        println!("Received malformed response on socket {sid}")
                    }
                }
            }
        }

        available_peripherals
    }

    /// Scan the local network for peripherals that are available to bind,
    /// giving `binding_timeout_ms` for peripherals to respond
    pub fn scan(
        &mut self,
        timeout_ms: u16,
        plugins: Option<PluginMap>,
    ) -> BTreeMap<SuperSocketAddr, Box<dyn Peripheral>> {
        // Ping with the longer desired timeout
        self.bind(None, timeout_ms, 0, plugins.clone())
    }

    pub fn run(&mut self) {
        // Set core affinity, if possible
        // This may not be available on every platform, so it should not break if not available
        let core_ids = core_affinity::get_core_ids().unwrap_or_default();
        let n_cores = core_ids.len();

        // Make a cycle over the cores that are available for auxiliary functions
        // other than the control loop. Because many modern CPUs present one extra fake "core"
        // per real core due to hyperthreading functionality, only every second core is
        // assumed to represent a real independent computing resource.
        let mut aux_core_cycle;
        if n_cores > 2 {
            aux_core_cycle = core_ids[2..].iter().step_by(2).cycle();
        } else {
            aux_core_cycle = core_ids[0..1].iter().step_by(2).cycle();
        }

        // Consume the first core for the control loop
        // While the last core is less likely to be overutilized, the first core is more
        // likely to be a high-performance core on a heterogeneous computing device
        if let Some(core) = core_ids.first() {
            core_affinity::set_for_current(*core);
        }

        // Set filled deadline scheduling to indicate that the control loop thread
        // should stay fully occupied. This is not available on Windows, in which
        // case, use max priority as a next-best option.
        // If both options fail, continue as-is.
        let _ = match thread_priority::set_current_thread_priority(
            thread_priority::ThreadPriority::Deadline {
                runtime: Duration::from_nanos(1),
                deadline: Duration::from_nanos(1),
                period: Duration::from_nanos(1),
                flags: DeadlineFlags::RESET_ON_FORK, // Children do not inherit deadline scheduling
            },
        ) {
            Ok(_) => (),
            Err(_) => {
                let _ = thread_priority::set_current_thread_priority(
                    thread_priority::ThreadPriority::Max,
                );
            }
        };

        // Buffer for writing bytes to send on sockets
        let txbuf = &mut [0_u8; 1522][..];

        // Scan to get peripheral addresses
        println!("Scanning for available units");
        let available_peripherals = self.scan(100, None);

        // Initialize state using scanned addresses
        println!("Initializing state");
        let mut controller_state = ControllerState::new(&self.peripherals, &available_peripherals);
        let addresses = controller_state
            .peripheral_state
            .keys()
            .copied()
            .collect::<Vec<SuperSocketAddr>>();

        // Initialize calc graph
        println!("Initializing calc orchestrator");
        self.orchestrator.init(self.ctx.dt_ns, &self.peripherals);
        self.orchestrator.eval(); // Populate constants, etc

        // Set up dispatcher(s)
        // TODO: send metrics to calcs so that they can be used as calc inputs
        println!("Initializing dispatchers");
        let mut channel_names = Vec::new();
        let metric_channel_names = controller_state.get_names_to_write();
        let io_channel_names = self.orchestrator.get_dispatch_names();
        channel_names.extend(metric_channel_names.iter().cloned());
        channel_names.extend(io_channel_names.iter().cloned());
        let n_metrics = metric_channel_names.len();
        let n_io = io_channel_names.len();
        let n_channels = n_metrics + n_io;
        let mut channel_values = vec![0.0; n_channels];
        for dispatcher in self.dispatchers.iter_mut() {
            let core_assignment = aux_core_cycle.next().unwrap();
            dispatcher
                .initialize(&self.ctx, self.ctx.dt_ns, &channel_names, *core_assignment)
                .unwrap();
        }
        println!("Dispatching data for {n_channels} channels.");

        // Bind & configure
        let mut all_peripherals_acknowledged = false;

        'configuring_retry: for i in 0..10 {
            println!("Binding peripherals");

            // Wait for peripherals to time out back to binding
            if i > 0 {
                std::thread::sleep(Duration::from_nanos(
                    self.ctx.peripheral_loss_of_contact_limit as u64 * self.ctx.dt_ns as u64,
                ));
            }

            // Clear buffers
            while self.sockets.iter_mut().any(|sock| sock.recv().is_some()) {}

            // Bind
            let dt_ms = (self.ctx.dt_ns / 1_000_000) as u16;
            let _bound_peripherals = self.bind(Some(&addresses), 10, 20.max(dt_ms), None);

            // Configuring starts as soon as peripherals receive binding input
            let start_of_configuring = Instant::now();

            // Clear buffer
            while self.sockets.iter_mut().any(|sock| sock.recv().is_some()) {}

            // Configure peripherals
            //    Send configuration to each peripheral
            println!("Configuring peripherals");
            let config_input = ConfiguringInput {
                dt_ns: self.ctx.dt_ns,
                timeout_to_operating_ns: self.ctx.timeout_to_operating_ns,
                loss_of_contact_limit: self.ctx.peripheral_loss_of_contact_limit,
                ..Default::default()
            };
            let num_to_write = ConfiguringInput::BYTE_LEN;
            config_input.write_bytes(&mut txbuf[..num_to_write]);

            for (sid, pid) in addresses.iter() {
                self.sockets[*sid]
                    .send(*pid, &txbuf[..num_to_write])
                    .unwrap();
            }

            //    Wait for peripherals to acknowledge their configuration
            let operating_timeout = Duration::from_nanos(self.ctx.timeout_to_operating_ns as u64);

            println!("Waiting for peripherals to acknowledge configuration");
            while start_of_configuring.elapsed() < operating_timeout {
                for (sid, socket) in self.sockets.iter_mut().enumerate() {
                    if let Some((pid, _rxtime, buf)) = socket.recv() {
                        let amt = buf.len();
                        // Make sure the packet is the right size and the peripheral ID is recognized
                        match (pid, amt) {
                            (Some(pid), ConfiguringOutput::BYTE_LEN) => {
                                // Parse the (potential) peripheral's response
                                let ack = ConfiguringOutput::read_bytes(buf);
                                let addr = (sid, pid);

                                match ack.acknowledge {
                                    AcknowledgeConfiguration::Ack => {
                                        controller_state
                                            .peripheral_state
                                            .get_mut(&addr)
                                            .unwrap()
                                            .acknowledged_configuration = true;
                                    }
                                    _ => panic!("Peripheral at {addr:?} rejected configuration"),
                                }
                            }
                            _ => {
                                println!(
                                    "Received malformed configuration response from socket {sid}"
                                )
                            }
                        }
                    }
                }

                all_peripherals_acknowledged = controller_state
                    .peripheral_state
                    .values()
                    .map(|ps| ps.acknowledged_configuration)
                    .all(|x| x);
            }

            if all_peripherals_acknowledged {
                break 'configuring_retry;
            }
        }

        //    If we reached the end of timeout into Operating and all peripherals
        //    acknowledged their configuration, continue to operating
        if !all_peripherals_acknowledged {
            panic!("Some peripherals did not acknowledge their configuration");
        }

        //    Init timing
        println!("Initializing timing controllers");
        let start_of_operating = Instant::now();
        let cycle_duration = Duration::from_nanos(self.ctx.dt_ns as u64);
        let mut target_time = cycle_duration;
        let mut peripheral_timing: BTreeMap<SuperSocketAddr, (TimingPID, MedianFilter<i64, 7>)> =
            BTreeMap::new();
        for addr in controller_state.peripheral_state.keys() {
            let max_clock_rate_err = 5e-2; // at least 5% tolerance for dev units using onboard clocks
            let ki = 0.00001 * (self.ctx.dt_ns as f64 / 10_000_000_f64);
            let timing_controller = TimingPID {
                kp: 0.005 * (self.ctx.dt_ns as f64 / 10_000_000_f64), // Tuned at 100Hz
                ki,
                kd: 0.001 / (self.ctx.dt_ns as f64 / 10_000_000_f64),

                v: 0.0,
                integral: 0.0,
                max_integral: max_clock_rate_err * (self.ctx.dt_ns as f64) / ki,
            };

            let timing_filter = MedianFilter::<i64, 7>::new(0);

            peripheral_timing.insert(*addr, (timing_controller, timing_filter));
        }

        //    Set up peripheral I/O buffers
        //    with maximum size of a standard packet
        let mut peripheral_input_buffer = [0.0_f64; 1522];

        //    Run
        println!("Entering control loop");
        let mut i: u64 = 0;
        controller_state.controller_metrics.cycle_time_margin_ns = self.ctx.dt_ns as f64;
        loop {
            i += 1;
            let mut t = start_of_operating.elapsed();
            let tmean = (target_time - cycle_duration / 2).as_nanos() as i64; // Time to drive peripheral packet arrivals toward
            let time = SystemTime::now();
            let timestamp = target_time.as_nanos() as i64;

            // Record timing margin
            controller_state.controller_metrics.cycle_time_margin_ns =
                (target_time.as_secs_f64() - t.as_secs_f64()) * 1e9;

            // Send next input
            for (addr, ps) in controller_state.peripheral_state.iter_mut() {
                let p = &self.peripherals[&ps.name];

                // Send packet
                let n = p.operating_roundtrip_input_size();
                let phase_delta_ns = ps.metrics.requested_phase_delta_ns as i64;
                let period_delta_ns = ps.metrics.requested_period_delta_ns as i64;

                self.orchestrator
                    .provide_peripheral_inputs(&ps.name, |vals| {
                        peripheral_input_buffer[..n]
                            .iter_mut()
                            .zip(vals)
                            .for_each(|(old, new)| {
                                let _ = core::mem::replace(old, new);
                            })
                    });

                // TODO: log transmit errors
                let (sid, pid) = addr;
                p.emit_operating_roundtrip(
                    i,
                    period_delta_ns,
                    phase_delta_ns,
                    &peripheral_input_buffer[..n],
                    &mut txbuf[..n],
                );

                self.sockets[*sid].send(*pid, &txbuf[..n]).unwrap();
            }

            // Receive packets until the start of the next cycle
            //     Unless we hear from each peripheral, assume we missed the packet
            for ps in controller_state.peripheral_state.values_mut() {
                ps.metrics.loss_of_contact_counter += 1.0;
            }
            while t < target_time {
                // Otherwise, process incoming packets

                for (sid, sock) in self.sockets.iter_mut().enumerate() {
                    if let Some((pid, rxtime, buf)) = sock.recv() {
                        let amt = buf.len();
                        let pid = match pid {
                            Some(x) => x,
                            None => continue,
                        };

                        let addr = (sid, pid);
                        if !addresses.contains(&addr) {
                            continue;
                        }

                        // Get the info for the peripheral at this address
                        let ps = controller_state.peripheral_state.get_mut(&addr).unwrap();
                        let p = &self.peripherals[&ps.name];
                        let n = p.operating_roundtrip_output_size();

                        // Check packet size
                        // TODO: log malformed packets
                        if amt != n {
                            continue;
                        }

                        // Parse packet,
                        // running the parsing inside the calc consumer to avoid copying
                        let last_packet_id = ps.metrics.operating_metrics.id;
                        let metrics = self.orchestrator.consume_peripheral_outputs(
                            &ps.name,
                            &mut |outputs: &mut [f64]| {
                                p.parse_operating_roundtrip(&buf[..n], outputs)
                            },
                        );

                        // If this packet is in-order, take it
                        if metrics.id > last_packet_id {
                            // ps.outputs.copy_from_slice(&outputs);
                            ps.metrics.operating_metrics = metrics;
                            ps.metrics.last_received_time_ns =
                                (rxtime - start_of_operating).as_nanos() as i64;

                            // Reset cycles since last contact
                            ps.metrics.loss_of_contact_counter = 0.0;

                            // Check if this peripheral is sync'd to the controller
                            let cycle_lag_count =
                                (metrics.last_input_id as i64) - (i.saturating_sub(1) as i64);
                            ps.metrics.cycle_lag_count = cycle_lag_count as f64;
                        }
                    }
                }

                t = start_of_operating.elapsed();
            }

            // Calculate timing deltas
            // in order to drive all modules toward target sample time
            for ps in controller_state.peripheral_state.values_mut() {
                // If we missed a packet from this peripheral, do nothing until
                // we hear from it again
                if ps.metrics.loss_of_contact_counter > 0.0 {
                    ps.metrics.requested_phase_delta_ns = 0.0;
                    continue;
                }

                // Update phase error estimate
                let dt_err_i64_ns = tmean - ps.metrics.last_received_time_ns;
                let dt_err_ns = dt_err_i64_ns as f64;
                ps.metrics.raw_timing_delta_ns = dt_err_ns;

                // Update the filter and controller for this peripheral's timing
                // Use median filter for data rates above 10Hz, otherwise raw value
                let (ref mut c, ref mut f) = peripheral_timing.get_mut(&ps.addr).unwrap();
                ps.metrics.filtered_timing_delta_ns = f.update(dt_err_i64_ns) as f64;
                let (period_delta_ns, phase_delta_ns) =
                    c.update(ps.metrics.filtered_timing_delta_ns);

                ps.metrics.requested_phase_delta_ns = phase_delta_ns;
                ps.metrics.requested_period_delta_ns = period_delta_ns;
            }

            // Run calcs
            self.orchestrator.eval();

            // Send outputs to db
            //    Write metrics
            controller_state.write_vals(&mut channel_values[..n_metrics]);
            //    Write io and calcs
            self.orchestrator.provide_dispatcher_outputs(|vals| {
                channel_values[n_metrics..]
                    .iter_mut()
                    .zip(vals)
                    .for_each(|(old, new)| {
                        *old = new;
                    })
            });
            //    Send to dispatcher
            for dispatcher in self.dispatchers.iter_mut() {
                dispatcher
                    .consume(time, timestamp, channel_values.clone())
                    .unwrap();
            }

            // Update next target time
            target_time += cycle_duration;
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_ser_roundtrip() {
        let mut controller = Controller::default();
        let per = deimos_shared::peripherals::analog_i_rev_2::AnalogIRev2 { serial_number: 0 };
        controller
            .peripherals
            .insert("test".to_owned(), Box::new(per));

        let serialized = serde_json::to_string(&controller).unwrap();
        let deserialized = serde_json::from_str::<Controller>(&serialized).unwrap();
        let reserialized = serde_json::to_string(&deserialized).unwrap();

        assert_eq!(serialized, reserialized);
        println!("{serialized}");
    }
}
