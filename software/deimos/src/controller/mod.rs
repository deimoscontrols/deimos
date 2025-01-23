//! Control loop and integration with data pipeline and calc orchestrator

mod controller_state;
mod peripheral_state;
mod timing;

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::net::{SocketAddr, UdpSocket};
use std::time::{Duration, Instant, SystemTime};

use core_affinity;
use local_ip_address;
use thread_priority::DeadlineFlags;

use serde::{Deserialize, Serialize};

use flaw::MedianFilter;

use deimos_shared::{
    calcs::Calc,
    peripherals::{parse_binding, Peripheral, PeripheralId, PluginMap},
    states::*,
    CONTROLLER_RX_PORT, PERIPHERAL_RX_PORT,
};

use crate::dispatcher::Dispatcher;
use crate::orchestrator::Orchestrator;
use controller_state::ControllerState;
use timing::TimingPID;

/// The controller implements the control loop,
/// synchronizes sample reporting time between the peripherals,
/// and dispatches measured data, calculations, and metrics to the data pipeline.
#[derive(Serialize, Deserialize, Default)]
pub struct Controller {
    // Input config
    dt_ns: u32,
    timeout_to_operating_ns: u32,
    loss_of_contact_limit: u16,

    // Appendages
    #[serde(skip)]
    socket: RefCell<Option<UdpSocket>>,
    dispatchers: Vec<Box<dyn Dispatcher>>,
    peripherals: BTreeMap<String, Box<dyn Peripheral>>,
    orchestrator: Orchestrator,
}

impl Controller {
    /// Initialize a fresh controller with no dispatchers, peripherals, or calcs.
    pub fn new(dt_ns: u32, timeout_to_operating_ns: u32, loss_of_contact_limit: u16) -> Self {
        let dispatchers = Vec::new();
        let peripherals = BTreeMap::new();
        let orchestrator = Orchestrator::default();
        let socket = RefCell::new(None);

        Self {
            dt_ns,
            timeout_to_operating_ns,
            loss_of_contact_limit,

            socket,
            dispatchers,
            peripherals,
            orchestrator,
        }
    }

    /// Get the UDP socket for receiving comms from hardware peripherals
    fn get_socket(&mut self) -> &mut UdpSocket {
        let socket_maybe = self.socket.get_mut();

        if socket_maybe.is_none() {
            // Bind the socket
            let socket = UdpSocket::bind(format!("0.0.0.0:{CONTROLLER_RX_PORT}")).unwrap();
            socket.set_nonblocking(true).unwrap();
            // Store the socket so that we don't have to initialize it again
            *socket_maybe = Some(socket);
        };

        socket_maybe.as_mut().unwrap()
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
    ///             binding timeout window
    ///                 /
    ///                 /           
    ///             |----|         timeout to operating
    ///             |--------------|
    ///             |      \
    /// sent binding|       \
    ///             |    configuring window
    /// peripherals
    ///     transition
    /// to configuring            
    /// ```
    /// To broadcast scan for available peripherals, provide no addresses
    /// and set configuring_timeout_ms to 0.
    pub fn bind(
        &mut self,
        addresses: Option<&Vec<SocketAddr>>,
        binding_timeout_ms: u16,
        configuring_timeout_ms: u16,
        plugins: Option<PluginMap>,
    ) -> BTreeMap<PeripheralId, (SocketAddr, Box<dyn Peripheral>)> {
        let mut available_peripherals = BTreeMap::new();

        // Buffer for UDP packets
        let udp_buf = &mut [0_u8; 1522][..];

        // Get local IP address so that we can scan for modules to bind
        let local_addr: std::net::IpAddr = local_ip_address::local_ip().unwrap();
        if !local_addr.is_ipv4() {
            panic!("IPV6 not handled yet");
        }

        let binding_msg = BindingInput {
            configuring_timeout_ms,
        };

        if let Some(addresses) = addresses {
            // Bind specific modules with a (hopefully) nonzero timeout
            binding_msg.write_bytes(&mut udp_buf[..BindingInput::BYTE_LEN]);
            //    Send unicast request to bind
            for addr in addresses.iter() {
                self.get_socket()
                    .send_to(&udp_buf[..BindingInput::BYTE_LEN], addr)
                    .unwrap();
            }
        } else {
            // Bind any modules on the local network
            binding_msg.write_bytes(&mut udp_buf[..BindingInput::BYTE_LEN]);
            self.get_socket().set_broadcast(true).unwrap();
            self.get_socket()
                .send_to(
                    &udp_buf[..BindingInput::BYTE_LEN],
                    format!("255.255.255.255:{PERIPHERAL_RX_PORT}"),
                )
                .unwrap();
            self.get_socket().set_broadcast(false).unwrap();
        }

        //    Collect binding responses
        let start_of_binding = Instant::now();
        while start_of_binding.elapsed().as_millis() <= (binding_timeout_ms + 1) as u128 {
            if let Ok((amt, src_socket_addr)) = self.get_socket().recv_from(udp_buf) {
                // If this is from the right port and it's not capturing our own
                // broadcast binding request, bind the module
                let recvd = &udp_buf[..BindingOutput::BYTE_LEN];
                if src_socket_addr.port() == PERIPHERAL_RX_PORT
                    && src_socket_addr.ip() != local_addr
                    && amt == BindingOutput::BYTE_LEN
                {
                    let binding_response = BindingOutput::read_bytes(recvd);
                    match parse_binding(&binding_response, &plugins) {
                        Ok(parsed) => {
                            let id = parsed.id();
                            available_peripherals.insert(id, (src_socket_addr, parsed));
                        }
                        Err(e) => println!("{e}"),
                    }
                } else {
                    println!("Received malformed response from {src_socket_addr:?}")
                }
            }
        }

        available_peripherals
    }

    /// Scan the local network for peripherals that are available to bind,
    /// giving `binding_timeout_ms` for peripherals to respond
    pub fn scan(
        &mut self,
        binding_timeout_ms: u16,
        plugins: Option<PluginMap>,
    ) -> BTreeMap<PeripheralId, (SocketAddr, Box<dyn Peripheral>)> {
        // Ping with the longer desired timeout
        self.bind(None, binding_timeout_ms, 0, plugins.clone())
    }

    pub fn run(&mut self, op_name: &str) {
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
        // should stay fully occupied
        let _ = thread_priority::set_current_thread_priority(
            thread_priority::ThreadPriority::Deadline {
                runtime: Duration::from_nanos(1),
                deadline: Duration::from_nanos(1),
                period: Duration::from_nanos(1),
                flags: DeadlineFlags::RESET_ON_FORK, // Children do not inherit deadline scheduling
            },
        );
        // let _ = thread_priority::set_current_thread_priority(thread_priority::ThreadPriority::Max);

        // Buffer for UDP packets
        let udp_buf = &mut [0_u8; 1522][..];

        // Initialize calc graph
        println!("Initializing calc orchestrator");
        self.orchestrator.init(self.dt_ns, &self.peripherals);
        self.orchestrator.eval(); // Populate constants, etc

        // Scan to get peripheral addresses
        println!("Scanning for available units");
        let available_peripherals = self.scan(100, None);

        // Initialize state using scanned addresses
        println!("Initializing state");
        let mut controller_state = ControllerState::new(&self.peripherals, &available_peripherals);
        let addresses = controller_state
            .peripheral_state
            .keys().copied()
            .collect::<Vec<SocketAddr>>();

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
                .initialize(self.dt_ns, &channel_names, op_name, *core_assignment)
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
                    self.loss_of_contact_limit as u64 * self.dt_ns as u64,
                ));
            }

            // Clear buffer
            while self.get_socket().recv_from(udp_buf).is_ok() {}

            // Bind
            let dt_ms = (self.dt_ns / 1_000_000) as u16;
            let _bound_peripherals = self.bind(Some(&addresses), 10, 20.max(dt_ms), None);

            // Configuring starts as soon as peripherals receive binding input
            let start_of_configuring = Instant::now();

            // Clear buffer
            while self.get_socket().recv_from(udp_buf).is_ok() {}

            // Configure peripherals
            //    Send configuration to each peripheral
            println!("Configuring peripherals");
            let config_input = ConfiguringInput {
                dt_ns: self.dt_ns,
                timeout_to_operating_ns: self.timeout_to_operating_ns,
                loss_of_contact_limit: self.loss_of_contact_limit,
                ..Default::default()
            };
            config_input.write_bytes(&mut udp_buf[..ConfiguringInput::BYTE_LEN]);
            for addr in addresses.iter() {
                self.get_socket()
                    .send_to(&udp_buf[..ConfiguringInput::BYTE_LEN], addr)
                    .unwrap();
            }

            //    Wait for peripherals to acknowledge their configuration
            let operating_timeout = Duration::from_nanos(self.timeout_to_operating_ns as u64);

            println!("Waiting for peripherals to acknowledge configuration");
            while start_of_configuring.elapsed() < operating_timeout {
                if let Ok((amt, src_socket_addr)) = self.get_socket().recv_from(udp_buf) {
                    if src_socket_addr.port() == PERIPHERAL_RX_PORT
                        && addresses.contains(&src_socket_addr)
                        && amt == ConfiguringOutput::BYTE_LEN
                    {
                        let ack =
                            ConfiguringOutput::read_bytes(&udp_buf[..ConfiguringOutput::BYTE_LEN]);
                        match ack.acknowledge {
                            AcknowledgeConfiguration::Ack => {
                                controller_state
                                    .peripheral_state
                                    .get_mut(&src_socket_addr)
                                    .unwrap()
                                    .acknowledged_configuration = true;
                            }
                            _ => panic!("Peripheral at {src_socket_addr} rejected configuration"),
                        }
                    } else {
                        println!(
                            "Received malformed configuration response from {src_socket_addr:?}"
                        )
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
        let cycle_duration = Duration::from_nanos(self.dt_ns as u64);
        let mut target_time = cycle_duration;
        let mut peripheral_timing: BTreeMap<SocketAddr, (TimingPID, MedianFilter<i64, 7>)> =
            BTreeMap::new();
        for addr in controller_state.peripheral_state.keys() {
            let max_clock_rate_err = 5e-2; // at least 5% tolerance for dev units using onboard clocks
            let ki = 0.00001 * (self.dt_ns as f64 / 10_000_000_f64);
            let timing_controller = TimingPID {
                kp: 0.005 * (self.dt_ns as f64 / 10_000_000_f64), // Tuned at 100Hz
                ki,
                kd: 0.001 / (self.dt_ns as f64 / 10_000_000_f64),

                v: 0.0,
                integral: 0.0,
                max_integral: max_clock_rate_err * (self.dt_ns as f64) / ki,
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
        controller_state.controller_metrics.cycle_time_margin_ns = self.dt_ns as f64;
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

                p.emit_operating_roundtrip(
                    i,
                    period_delta_ns,
                    phase_delta_ns,
                    &peripheral_input_buffer[..n],
                    &mut udp_buf[..n],
                );
                // TODO: log transmit errors
                let _ = self.get_socket().send_to(&udp_buf[..n], addr);
            }

            // Receive packets until the start of the next cycle
            //     Unless we hear from each peripheral, assume we missed the packet
            for ps in controller_state.peripheral_state.values_mut() {
                ps.metrics.loss_of_contact_counter += 1.0;
            }
            while t < target_time {
                // Otherwise, process incoming packets
                t = start_of_operating.elapsed();

                if let Ok((amt, src_socket_addr)) = self.get_socket().recv_from(udp_buf) {
                    if !(src_socket_addr.port() == PERIPHERAL_RX_PORT
                        && addresses.contains(&src_socket_addr))
                    // TODO: faster method using treemap to handle large numbers of peripherals
                    {
                        continue;
                    }

                    // Get the info for the peripheral at this address
                    let ps = controller_state
                        .peripheral_state
                        .get_mut(&src_socket_addr)
                        .unwrap();
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
                            p.parse_operating_roundtrip(&udp_buf[..n], outputs)
                        },
                    );

                    // If this packet is in-order, take it
                    // TODO: handle upserting out of order data? maybe send to different db
                    if metrics.id > last_packet_id {
                        // ps.outputs.copy_from_slice(&outputs);
                        ps.metrics.operating_metrics = metrics;
                        ps.metrics.last_received_time_ns = t.as_nanos() as i64;

                        // Reset cycles since last contact
                        ps.metrics.loss_of_contact_counter = 0.0;

                        // Check if this peripheral is sync'd to the controller
                        let cycle_lag_count =
                            (metrics.last_input_id as i64) - (i.saturating_sub(1) as i64);
                        ps.metrics.cycle_lag_count = cycle_lag_count as f64;
                    }
                }
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
