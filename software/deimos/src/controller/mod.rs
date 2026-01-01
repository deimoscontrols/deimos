//! Control loop and integration with data pipeline and calc orchestrator

pub mod channel;
pub mod context;
mod controller_state;
mod peripheral_state;
mod timing;

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Condvar, Mutex, RwLock};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use flaw::MedianFilter;

#[cfg(feature = "python")]
use pyo3::prelude::*;

#[cfg(feature = "python")]
use crate::python::BackendErr;

use crate::controller::context::LoopMethod;
use crate::{
    SOCKET_BUFFER_LEN,
    calc::Calc,
    logging,
    peripheral::{HootlRunHandle, HootlTransport, Peripheral, PluginMap, parse_binding},
};
use deimos_shared::states::*;

use crate::calc::{CalcOrchestrator, FieldName, PeripheralInputName};
use crate::dispatcher::{
    Dispatcher, LatestValueDispatcher, LatestValueHandle, LowPassDispatcher, Row, fmt_time,
};
use crate::socket::udp::UdpSocket;
use crate::socket::{Socket, SocketAddr, SocketOrchestrator, SocketRecvMeta};
use context::{ControllerCtx, LossOfContactPolicy, Termination};
use controller_state::ControllerState;
use peripheral_state::ConnState;
use timing::TimingPID;
use tracing::{debug, error, info, warn};

/// Peripheral inputs set manually from outside the control program.
pub type ManualInputMap = Arc<RwLock<HashMap<FieldName, f64>>>;

pub(crate) fn manual_inputs_default() -> ManualInputMap {
    Arc::new(RwLock::new(HashMap::new()))
}

/// Boolean predicates to support the ReadySignal condvar
/// because condvars can generate spurious wake signals.
#[derive(Clone, Copy, Debug, Default)]
struct ReadyState {
    ready: bool,
    finished: bool,
}

/// Classic predicate-signal pair for inter-thread signaling
/// using OS-scheduled condition variable.
#[derive(Debug, Default)]
struct ReadySignal {
    state: Mutex<ReadyState>,
    cvar: Condvar,
}

impl ReadySignal {
    /// Clear predicates.
    fn reset(&self) {
        let mut state = self.state.lock().unwrap();
        state.ready = false;
        state.finished = false;
    }

    /// Set ready predicate and signal condvar.
    fn mark_ready(&self) {
        let mut state = self.state.lock().unwrap();
        if !state.ready {
            state.ready = true;
            self.cvar.notify_all();
        }
    }

    /// Set finished predicate and signal condvar.
    fn mark_finished(&self) {
        let mut state = self.state.lock().unwrap();
        state.finished = true;
        self.cvar.notify_all();
    }

    /// Check ready predicate.
    fn is_ready(&self) -> bool {
        self.state.lock().unwrap().ready
    }

    /// Wait for signal indicating the thread is either ready or already finished.
    /// Uses efficient-but-imprecise OS-scheduled waiting.
    fn wait_ready_or_finished(&self) -> ReadyState {
        let mut state = self.state.lock().unwrap();
        while !state.ready && !state.finished {
            state = self.cvar.wait(state).unwrap();
        }
        *state
    }
}

/// Drop-guard to guarantee that the ready signal is marked
/// finished if the controller exits the loop for any reason.
struct ReadyFinishGuard {
    ready: Arc<ReadySignal>,
}

impl Drop for ReadyFinishGuard {
    fn drop(&mut self) {
        self.ready.mark_finished();
    }
}

fn default_ready_signal() -> Arc<ReadySignal> {
    Arc::new(ReadySignal::default())
}

/// The controller implements the control loop,
/// synchronizes sample reporting time between the peripherals,
/// and dispatches measured data, calculations, and metrics to the data pipeline.
#[derive(Serialize, Deserialize)]
pub struct Controller {
    // Input config, which is passed to appendages during their init.
    pub ctx: ControllerCtx,

    // Appendages
    sockets: BTreeMap<String, Box<dyn Socket>>,
    dispatchers: BTreeMap<String, Box<dyn Dispatcher>>,
    peripherals: BTreeMap<String, Box<dyn Peripheral>>,
    orchestrator: CalcOrchestrator,

    #[serde(skip, default = "default_ready_signal")]
    ready: Arc<ReadySignal>,
}

/// A snapshot of the values from all peripherals and calcs
/// at the end of a cycle.
#[cfg_attr(feature = "python", pyclass)]
#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    /// Cycle-end UTC system time in RFC3339 format with nanoseconds.
    pub system_time: String,

    /// [ns] Cycle-end time in nanoseconds since the start of run.
    pub timestamp: i64,

    /// Map of the latest readings from all peripherals and calcs.
    pub values: HashMap<String, f64>,
}

/// A handle to a [Controller] running via [Controller::run_nonblocking] that allows
/// reading and writing values from outside the control program during operation
/// and signaling the controller to shut down.
///
/// Signals the controller to shut down when dropped.
#[cfg_attr(feature = "python", pyclass)]
pub struct RunHandle {
    /// Signal to stop the controller.
    termination: Arc<AtomicBool>,

    /// Link to the running controller to get a snapshot of the output.
    latest: LatestValueHandle,

    /// Thread handle for the running controller.
    join: Option<std::thread::JoinHandle<Result<String, String>>>,

    /// Manual input values for peripherals to set.
    manual_input_values: ManualInputMap,

    /// Names of all peripheral inputs that can be set manually.
    manual_input_names: Vec<String>,

    /// Shared readiness signal from the controller.
    ready: Arc<ReadySignal>,
}

impl Default for Controller {
    fn default() -> Self {
        // Include a UDP socket by default, but otherwise blank
        let mut sockets: BTreeMap<String, Box<dyn Socket>> = BTreeMap::new();
        sockets.insert("udp".to_string(), Box::new(UdpSocket::new()));

        let dispatchers = BTreeMap::new();
        let peripherals = BTreeMap::new();
        let orchestrator = CalcOrchestrator::default();
        let ctx = ControllerCtx::default();
        let ready = default_ready_signal();

        Self {
            ctx,
            sockets,
            dispatchers,
            peripherals,
            orchestrator,
            ready,
        }
    }
}

impl Controller {
    /// Initialize a fresh controller with no dispatchers, peripherals, or calcs.
    /// A UDP socket is included by default, but can be removed.
    pub fn new(ctx: ControllerCtx) -> Self {
        Self {
            ctx,
            ..Default::default()
        }
    }

    /// Read-only access to calc nodes
    pub fn calcs(&self) -> &BTreeMap<String, Box<dyn Calc>> {
        self.orchestrator.calcs()
    }

    /// Read-only access to peripherals
    pub fn peripherals(&self) -> &BTreeMap<String, Box<dyn Peripheral>> {
        &self.peripherals
    }

    /// Read-only access to dispatchers by name.
    pub fn dispatcher(&self, name: &str) -> Option<&Box<dyn Dispatcher>> {
        self.dispatchers.get(name)
    }

    /// Mutable access to dispatchers by name.
    pub fn dispatcher_mut(&mut self, name: &str) -> Option<&mut Box<dyn Dispatcher>> {
        self.dispatchers.get_mut(name)
    }

    /// List dispatcher names.
    pub fn dispatcher_names(&self) -> Vec<String> {
        self.dispatchers.keys().cloned().collect()
    }

    /// Remove a dispatcher by name.
    pub fn remove_dispatcher(&mut self, name: &str) -> Option<Box<dyn Dispatcher>> {
        self.dispatchers.remove(name)
    }

    /// Read-only access to edges from calcs to peripherals
    pub fn peripheral_input_sources(&self) -> &BTreeMap<PeripheralInputName, FieldName> {
        self.orchestrator.peripheral_input_sources()
    }

    /// Peripheral inputs that can be written manually.
    pub fn manual_input_names(&self) -> Vec<FieldName> {
        self.orchestrator.manual_input_names(&self.peripherals)
    }

    /// Run the control program on a separate thread and return a handle for coordination.
    ///
    /// `latest_value_cutoff_freq` enables an
    /// optional second-order Butterworth low-pass filter
    /// cutoff frequency to apply to latest-value data.
    /// If the selected frequency is outside the viable
    /// range for the filter, the cutoff frequency will
    /// be clamped to the viable bounds and a warning
    /// will be emitted.
    ///
    /// If `wait_for_ready` is true, block until the controller completes its first cycle.
    ///
    /// `plugins` provides a mechanism to register user-defined Peripheral objects.
    pub fn run_nonblocking(
        self,
        plugins: Option<PluginMap<'static>>,
        latest_value_cutoff_freq: Option<f64>,
        wait_for_ready: bool,
    ) -> Result<RunHandle, String> {
        let mut controller = self;
        controller.ready.reset();
        info!("Building nonblocking controller.");

        // Attach a latest-value dispatcher to expose live data.
        let existing_names = BTreeSet::from_iter(controller.dispatcher_names().clone());
        //   Make sure we don't overwrite an existing dispatcher name.
        let mut latest_name = "latest".to_string();
        if existing_names.contains(&latest_name) {
            let mut suffix = 1;
            loop {
                let candidate = format!("latest_{suffix}");
                if !existing_names.contains(&candidate) {
                    latest_name = candidate;
                    break;
                }
                suffix += 1;
            }
        }

        // Add the handle to get the latest values, possibly with a filter applied.
        let (latest_dispatcher, latest_handle) = LatestValueDispatcher::new();
        let latest_dispatcher: Box<dyn Dispatcher> = match latest_value_cutoff_freq {
            Some(cutoff_hz) => LowPassDispatcher::new(latest_dispatcher, cutoff_hz),
            None => latest_dispatcher,
        };
        controller.add_dispatcher(&latest_name, latest_dispatcher);

        // Set up machinery for interacting with the controller during operation.
        let termination_signal = Arc::new(AtomicBool::new(false));
        let manual_input_values = manual_inputs_default();
        controller.ctx.manual_inputs = manual_input_values.clone();
        let manual_input_names = controller.manual_input_names();

        // Make a handle to the termination signal.
        let term_for_thread = termination_signal.clone();
        let ready_for_thread = controller.ready.clone();
        let ready_for_handle = controller.ready.clone();

        // Run the controller on a separate thread.
        let handle = thread::Builder::new()
            .name("controller-run".to_string())
            .spawn(move || {
                let _ready_guard = ReadyFinishGuard {
                    ready: ready_for_thread,
                };
                // Run to completion.
                let result = controller.run(&plugins, Some(&*term_for_thread));

                // Remove the temporary latest-value handle when we're done.
                controller.remove_dispatcher(&latest_name);

                result
            })
            .expect("Failed to spawn controller thread");

        info!("Spawned nonblocking controller thread.");

        let mut run_handle = RunHandle {
            termination: termination_signal,
            latest: latest_handle,
            join: Some(handle),
            manual_input_values,
            manual_input_names,
            ready: ready_for_handle,
        };

        if !wait_for_ready {
            return Ok(run_handle);
        }

        info!("Waiting for controller thread to indicate readiness.");
        let ready_state = run_handle.ready.wait_ready_or_finished();
        if ready_state.ready {
            return Ok(run_handle);
        }

        warn!("Controller run did not indicate readiness.");
        // If we waited for readiness but the wait ended without readiness being indicated,
        // then the control program already ended.
        let err = match run_handle.join.take() {
            Some(handle) => match handle.join() {
                Ok(Ok(msg)) => format!("Controller exited before ready: {msg}"),
                Ok(Err(e)) => e,
                Err(e) => format!("Controller thread panicked: {e:?}"),
            },
            None => "Controller thread already joined or not started".to_string(),
        };

        Err(err)
    }

    /// Register a calc function
    pub fn add_calc(&mut self, name: &str, calc: Box<dyn Calc>) {
        self.orchestrator.add_calc(name, calc);
    }

    /// Add multiple calcs
    ///
    /// # Panics
    /// * If, for any calc to add, a calc with this name already exists
    pub fn add_calcs(&mut self, mut calcs: BTreeMap<String, Box<dyn Calc>>) {
        while let Some((name, calc)) = calcs.pop_first() {
            self.orchestrator.add_calc(&name, calc);
        }
    }

    /// Remove all calcs and peripheral input sources
    pub fn clear_calcs(&mut self) {
        self.orchestrator.clear_calcs();
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

    /// Replace a peripheral with a hootl wrapper and start its driver.
    pub fn attach_hootl_driver(
        &mut self,
        peripheral_name: &str,
        transport: HootlTransport,
        end: Option<SystemTime>,
    ) -> Result<HootlRunHandle, String> {
        if let Some(existing) = self.peripherals.get(peripheral_name) {
            if existing.kind() == "HootlPeripheral" {
                return Err(format!(
                    "Peripheral `{peripheral_name}` is already a HootlPeripheral"
                ));
            }
        }

        let inner = self
            .peripherals
            .remove(peripheral_name)
            .ok_or_else(|| format!("Peripheral `{peripheral_name}` not found"))?;

        let (wrapper, driver) = crate::peripheral::hootl::build_hootl_pair(inner, transport, end);
        match driver.run(&self.ctx) {
            Ok(handle) => {
                self.peripherals
                    .insert(peripheral_name.to_owned(), Box::new(wrapper));
                Ok(handle)
            }
            Err(err) => {
                self.peripherals
                    .insert(peripheral_name.to_owned(), wrapper.into_inner());
                Err(err)
            }
        }
    }

    /// Register a data pipeline dispatcher by name.
    pub fn add_dispatcher(&mut self, name: &str, dispatcher: Box<dyn Dispatcher>) {
        if self.dispatchers.contains_key(name) {
            warn!("Dispatcher '{name}' overwritten.");
        }
        self.dispatchers.insert(name.to_string(), dispatcher);
    }

    /// Register a socket
    pub fn add_socket(&mut self, name: &str, socket: Box<dyn Socket>) {
        if self.sockets.contains_key(name) {
            warn!("Socket '{name}' overwritten.");
        }
        self.sockets.insert(name.to_string(), socket);
    }

    /// Remove a socket by name.
    pub fn remove_socket(&mut self, name: &str) -> Option<Box<dyn Socket>> {
        self.sockets.remove(name)
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

    fn socket_names(&self) -> Vec<String> {
        self.sockets.keys().cloned().collect()
    }

    fn socket_by_index_mut<'a>(
        &'a mut self,
        socket_names: &'a [String],
        socket_id: usize,
    ) -> Option<&'a mut Box<dyn Socket>> {
        socket_names
            .get(socket_id)
            .and_then(|name| self.sockets.get_mut(name))
    }

    /// Connect an entry in the calc graph to a command to be sent to the peripheral
    pub fn set_peripheral_input_source(&mut self, input_field: &str, source_field: &str) {
        self.orchestrator
            .set_peripheral_input_source(input_field, source_field);
    }

    /// Open sockets, bind ports, etc.
    /// No-op if called multiple times without closing sockets.
    pub fn open_sockets(&mut self) -> Result<(), String> {
        let mut buf = [0_u8; SOCKET_BUFFER_LEN];
        for sock in self.sockets.values_mut() {
            if !sock.is_open() {
                sock.open(&self.ctx)?;
            }
            loop {
                if sock.recv(&mut buf, Duration::ZERO).is_none() {
                    break;
                }
            }
        }

        Ok(())
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
        addresses: Option<&Vec<SocketAddr>>,
        binding_timeout_ms: u16,
        configuring_timeout_ms: u16,
        plugins: &Option<PluginMap>,
    ) -> Result<BTreeMap<SocketAddr, Box<dyn Peripheral>>, String> {
        // Make sure sockets are configured and ports are bound
        self.open_sockets()?;

        let mut binding_buf = vec![0_u8; SOCKET_BUFFER_LEN];
        let buf = binding_buf.as_mut_slice();
        let mut available_peripherals = BTreeMap::new();

        let binding_msg = BindingInput {
            configuring_timeout_ms,
        };
        binding_msg.write_bytes(&mut buf[..BindingInput::BYTE_LEN]);

        // Start the clock at transmission
        let start_of_binding = Instant::now();

        // Send binding requests
        if let Some(addresses) = addresses {
            let socket_names = self.socket_names();
            // Bind specific modules with a (hopefully) nonzero timeout
            //    Send unicast request to bind
            for (socket_id, peripheral_id) in addresses.iter() {
                let socket = self
                    .socket_by_index_mut(&socket_names, *socket_id)
                    .ok_or_else(|| format!("Socket index {socket_id} out of range."))?;
                socket
                    .send(*peripheral_id, &buf[..BindingInput::BYTE_LEN])
                    .map_err(|e| {
                        format!("Failed to send binding request to {peripheral_id:?}: {e}")
                    })?;
            }
        } else {
            // Bind any modules on the local network
            for socket in self.sockets.values_mut() {
                socket
                    .broadcast(&buf[..BindingInput::BYTE_LEN])
                    .map_err(|e| format!("Failed to broadcast binding request: {e}"))?;
            }
        }

        // Collect binding responses
        while start_of_binding.elapsed().as_millis() <= binding_timeout_ms as u128 {
            for (sid, (socket_name, socket)) in self.sockets.iter_mut().enumerate() {
                let mut rxbuf = [0_u8; SOCKET_BUFFER_LEN];
                if let Some(meta) = socket.recv(&mut rxbuf, Duration::ZERO) {
                    // If this is from the right port and it's not capturing our own
                    // broadcast binding request, bind the module
                    // let recvd = &udp_buf[..BindingOutput::BYTE_LEN];
                    let amt = meta.size;
                    if amt == BindingOutput::BYTE_LEN {
                        let binding_response = BindingOutput::read_bytes(&rxbuf[..amt]);
                        match parse_binding(&binding_response, plugins) {
                            Ok(parsed) => {
                                let pid = parsed.id();
                                let addr = (sid, pid);
                                // Update the socket's address map
                                socket
                                    .update_map(pid, meta.token)
                                    .map_err(|e| format!("Failed to update socket mapping: {e}"))?;
                                // Update the controller's address map
                                available_peripherals.insert(addr, parsed);
                            }
                            Err(e) => warn!("{e}"),
                        }
                    } else {
                        warn!(
                            "Received malformed binding response on socket {sid} ({socket_name}) with {amt} bytes"
                        );
                    }
                }
            }
        }

        Ok(available_peripherals)
    }

    /// Scan the local network for peripherals that are available to bind,
    /// giving `timeout_ms` for peripherals to respond.
    pub fn scan(
        &mut self,
        timeout_ms: u16,
        plugins: &Option<PluginMap>,
    ) -> Result<BTreeMap<SocketAddr, Box<dyn Peripheral>>, String> {
        // Ping with the longer desired timeout for hearing back from the peripherals,
        // but a zero timeout for the peripherals returning to Binding.
        self.bind(None, timeout_ms, 0, plugins)
    }

    /// Safe the peripherals and shut down the controller
    #[cold]
    fn terminate(
        &mut self,
        state: &ControllerState,
        peripheral_input_buffer: &mut [f64],
        packet_index: u64,
        socket_orchestrator: &mut SocketOrchestrator,
    ) {
        self.ready.reset();
        peripheral_input_buffer.fill(0.0);
        let mut err_rollup: Vec<String> = Vec::new();

        // Send peripherals default state.
        //
        // Send multiple times to each peripheral to reduce probability of
        // packet loss; in the event that the shutdown message is missed,
        // the peripheral will still return to its default state on reaching
        // its loss-of-contact limit.
        for j in 0..3 {
            for (addr, ps) in state.peripheral_state.iter() {
                // Build default state packet
                let p = &self.peripherals[&ps.name];
                let n = p.operating_roundtrip_input_size();
                let (sid, pid) = addr;
                let mut buf = [0_u8; SOCKET_BUFFER_LEN];
                p.emit_operating_roundtrip(
                    j + packet_index,
                    0,
                    0,
                    &peripheral_input_buffer[..n],
                    &mut buf[..n],
                );

                // Transmit default state packet
                let send_result = socket_orchestrator.send(*sid, *pid, &buf[..n]);
                if let Err(err) = send_result {
                    let msg = format!("Failed to send shutdown packet to `{}`: {err}", &ps.name);
                    error!("{msg}");
                    err_rollup.push(msg);
                }
            }
        }

        // Reset dispatchers.
        self.dispatchers
            .values_mut()
            .filter_map(|d| d.terminate().err())
            .for_each(|e| err_rollup.push(e));

        // Reset calc orchestrator.
        let _ = self
            .orchestrator
            .terminate()
            .map_err(|e| err_rollup.push(e.to_string()));

        if let Ok(mut guard) = self.ctx.manual_inputs.write() {
            guard.clear();
        } else {
            let msg = "Manual input map lock poisoned; stale manual inputs may persist.";
            error!("{msg}");
            err_rollup.push(msg.to_string());
        }

        // Log all errors encountered during shutdown
        if !err_rollup.is_empty() {
            error!("Encountered errors during termination: {err_rollup:?}");
        }
    }

    /// Handle an incoming packet from a socket.
    #[inline]
    fn process_socket_packet(
        &mut self,
        controller_state: &mut ControllerState,
        addresses: &[SocketAddr],
        start_of_operating: Instant,
        meta: &SocketRecvMeta,
        payload: &[u8],
        cycle_index: u64,
    ) {
        let amt = meta.size;
        let pid = match meta.pid {
            Some(x) => x,
            None => return,
        };

        let addr = (meta.socket_id, pid);
        if !addresses.contains(&addr) {
            // To avoid being packet-flooded, we do nothing here
            return;
        }

        let ps = match controller_state.peripheral_state.get_mut(&addr) {
            Some(ps) => ps,
            None => return,
        };
        if !matches!(ps.conn_state, ConnState::Operating { .. }) {
            return;
        }
        let p = &self.peripherals[&ps.name];
        let n = p.operating_roundtrip_output_size();

        if amt != n {
            if cycle_index > 10 {
                // During the first few cycles, we might catch a configuration response
                // coming in late, which isn't concerning.
                warn!("Received malformed packet from peripheral `{}`", &ps.name);
            }
            return;
        }

        let last_packet_id = ps.metrics.operating_metrics.id;
        let metrics = self
            .orchestrator
            .consume_peripheral_outputs(&ps.name, &mut |outputs: &mut [f64]| {
                p.parse_operating_roundtrip(&payload[..n], outputs)
            });

        if metrics.id > last_packet_id {
            ps.metrics.operating_metrics = metrics;
            ps.metrics.last_received_time_ns = (meta.time - start_of_operating).as_nanos() as i64;
            ps.metrics.loss_of_contact_counter = 0.0;
            let cycle_lag_count =
                (metrics.last_input_id as i64) - (cycle_index.saturating_sub(1) as i64);
            ps.metrics.cycle_lag_count = cycle_lag_count as f64;
        }
    }

    /// Check if a packet is a reconnection attempt (Binding or Configuring response)
    /// and, if so, update peripheral reconnection state and address maps.
    /// Returns `true` if this was a reconnection packet and `false` otherwise.
    #[inline]
    fn handle_reconnect_packet(
        &mut self,
        controller_state: &mut ControllerState,
        socket_orchestrator: &mut SocketOrchestrator,
        meta: &SocketRecvMeta,
        payload: &[u8],
        reconnect_step_timeout: Duration,
    ) -> bool {
        let now = Instant::now();

        // Handle Binding response
        if meta.size == BindingOutput::BYTE_LEN {
            // Parse.
            let binding_response = BindingOutput::read_bytes(payload);
            let pid = binding_response.peripheral_id;
            let addr = (meta.socket_id, pid);

            // Check if this is a response from one of our attached peripherals.
            // It might be from a peripheral on the network that is not associated with this control program.
            let ps = match controller_state.peripheral_state.get_mut(&addr) {
                Some(ps) => ps,
                None => return false, // Indicate that this was not a reconnection packet
            };

            info!("Processed Binding response from peripheral {}", ps.name);

            // Check if we were expecting a Binding response from this peripheral.
            // This might be a Binding response arriving late after we've already
            // transitioned to Configuring.
            let reconnect_deadline = match ps.conn_state {
                ConnState::Binding {
                    reconnect_deadline, ..
                } => reconnect_deadline,
                _ => return false, // Indicate that this was not a reconnection packet.
            };

            // Update address maps.
            // If the peripheral's IP address was reassigned or it was physically
            // connected to a different location in the network, its address might have changed.
            let update_result = socket_orchestrator.update_map(meta.socket_id, pid, meta.token);

            // If we're unable to talk to its socket to update the address map,
            // mark it as Disconnected again.
            if let Err(err) = update_result {
                error!("{err}");
                ps.conn_state = ConnState::Disconnected {
                    deadline: reconnect_deadline,
                };
                return true;
            }

            // Build Configuring packet.
            let config_input = ConfiguringInput {
                dt_ns: self.ctx.dt_ns,
                timeout_to_operating_ns: 0, // Start immediately on next cycle
                loss_of_contact_limit: self.ctx.peripheral_loss_of_contact_limit,
                mode: Mode::Roundtrip,
            };
            let p = &self.peripherals[&ps.name];
            let num_to_write = p.configuring_input_size();

            // Transmit Configuring packet.
            let mut buf = [0_u8; SOCKET_BUFFER_LEN];
            p.emit_configuring(config_input, &mut buf[..num_to_write]);
            let send_result = socket_orchestrator.send(meta.socket_id, pid, &buf[..num_to_write]);

            // Mark disconnected if the socket fails.
            if let Err(err) = send_result {
                error!("{err}");
                ps.conn_state = ConnState::Disconnected {
                    deadline: reconnect_deadline,
                };
                return true;
            }

            // Update peripheral state.
            ps.acknowledged_configuration = false;
            ps.conn_state = ConnState::Configuring {
                configuring_timeout: now + reconnect_step_timeout,
                reconnect_deadline,
            };

            info!("Sent Configuring input packet to peripheral {}", ps.name);

            // Indicate that this was a reconnection packet.
            return true;
        }

        // Handle Configuring response.
        if meta.size == ConfiguringOutput::BYTE_LEN {
            // Get this peripheral's state info.
            let pid = match meta.pid {
                Some(pid) => pid,
                None => return false,
            };
            let addr = (meta.socket_id, pid);
            let ps = match controller_state.peripheral_state.get_mut(&addr) {
                Some(ps) => ps,
                None => return false, // Indicate that this was not a reconnection packet.
            };

            // Check if we were expecting a Configuring response from this peripheral.
            // It's possible that this is arriving late after we've already transitioned
            // to another state.
            let reconnect_deadline = match ps.conn_state {
                ConnState::Configuring {
                    reconnect_deadline, ..
                } => reconnect_deadline,
                _ => return false, // Indicate that this was not a reconnection packet.
            };

            // Check whether the configuration was acknowledged by the peripheral.
            // If not, mark it as Disconnected again.
            let ack = ConfiguringOutput::read_bytes(payload);
            match ack.acknowledge {
                AcknowledgeConfiguration::Ack => {
                    ps.acknowledged_configuration = true;
                    ps.metrics.loss_of_contact_counter = 0.0;
                    ps.metrics.operating_metrics = OperatingMetrics::default();
                    ps.conn_state = ConnState::Operating();
                }
                _ => {
                    warn!(
                        "Peripheral {} rejected configuration during reconnect.",
                        ps.name
                    );
                    ps.conn_state = ConnState::Disconnected {
                        deadline: reconnect_deadline,
                    };
                }
            }

            info!(
                "Peripheral {} ackwnowledged configuration and reentered Operating state.",
                ps.name
            );

            // Indicate that this was a reconnection packet.
            return true;
        }

        // If it didn't match a Binding or Configuring response,
        // indicate that this was not a reconnection packet.
        false
    }

    /// Start the control program.
    ///
    /// `plugins` provides a mechanism to register user-defined Peripheral objects.
    /// `termination_signal` signals the controller to shut down when set to `true`.
    pub fn run(
        &mut self,
        plugins: &Option<PluginMap>,
        termination_signal: Option<&AtomicBool>,
    ) -> Result<String, String> {
        self.ready.reset();
        // Start log file.
        let (log_file, _logging_guards) =
            logging::init_logging(&self.ctx.op_dir, &self.ctx.op_name)
                .map_err(|err| format!("Failed to initialize logging: {err}"))?;
        let log_file_canonicalized = log_file
            .canonicalize()
            .map_err(|e| format!("Failed to resolve log file path: {e}"))?;
        info!("Starting op \"{}\".", &self.ctx.op_name);
        info!("Using op dir \"{}\".", &self.ctx.op_dir.to_string_lossy());
        info!("Logging to file {:?} .", log_file_canonicalized);

        // Clear stale manual inputs.
        if let Ok(mut guard) = self.ctx.manual_inputs.write() {
            guard.clear();
        } else {
            let msg = "Manual input map lock poisoned; stale manual inputs may persist.";
            error!("{msg}");
            return Err(msg.to_string());
        }

        // Check config
        if matches!(self.ctx.loop_method, LoopMethod::Efficient) && self.ctx.dt_ns < 20_000_000 {
            warn!(
                "Using Efficient loop method for cycle rates higher than 50Hz is likely to cause degraded performance."
            );
        }

        //    Set up peripheral I/O buffers
        //    with maximum size of a standard packet
        let peripheral_input_buffer = &mut [0.0_f64; 1522 / 8 + 1];
        let txbuf: &mut [u8; 1522] = &mut [0_u8; SOCKET_BUFFER_LEN];
        let rxbuf: &mut [u8; 1522] = &mut [0_u8; SOCKET_BUFFER_LEN];

        // Set up core affinity
        let core_ids = core_affinity::get_core_ids().unwrap_or_default();
        let mut aux_core_cycle = {
            // Set core affinity, if possible
            // This may not be available on every platform, so it should not break if not available
            let n_cores = core_ids.len();

            // Make a cycle over the cores that are available for auxiliary functions
            // other than the control loop. Because many modern CPUs present one extra fake "core"
            // per real core due to hyperthreading functionality, the first two "cores" are both
            // reserved for the main thread to avoid sharing resources between the hard-realtime
            // part and the less timing-sensitive dispatchers.
            let aux_core_cycle = if n_cores > 2 {
                core_ids[2..].iter().cycle()
            } else {
                core_ids[0..1].iter().cycle()
            };

            // If we're in performant loop mode, consume the first core for the control loop.
            // This is critical to prevent context-switching overhead, which causes cycle lag
            // and missed packets.
            //
            // While the last core is less likely to be overutilized, the first core is more
            // likely to be a high-performance core on a heterogeneous computing device.
            //
            // If we're in efficient loop mode, prioritize being a good neighbor to other processes
            // by not hogging a specific core.
            if let Some(core) = core_ids.first()
                && matches!(self.ctx.loop_method, LoopMethod::Performant)
            {
                let succeeded = core_affinity::set_for_current(*core);
                if !succeeded {
                    warn!("Failed to set main thread core affinity.");
                } else {
                    info!(
                        "Set control loop core affinity to {core:?} for loop method {:?}.",
                        self.ctx.loop_method
                    );
                }
            }

            aux_core_cycle
        };

        // Pre-allocated reusable byte buffer pool for transmitting on sockets.
        // Make sure sockets are configured and ports are bound
        info!("Opening peripheral comm sockets.");
        self.open_sockets()
            .map_err(|e| format!("Failed to open sockets: {e}"))?;

        // Scan to get peripheral addresses
        info!("Scanning for available units.");
        let available_peripherals = self
            .scan(100, plugins)
            .map_err(|e| format!("Failed to scan for peripherals: {e}"))?;
        info!("Found available units: {:?}", &available_peripherals);

        // Check that all required peripherals are available.
        {
            let peripheral_set =
                BTreeSet::from_iter(available_peripherals.keys().map(|(_sid, pid)| *pid));
            let missing_peripherals: Vec<String> = self
                .peripherals
                .iter()
                .filter(|(_pname, p)| !peripheral_set.contains(&p.id()))
                .map(|(pname, _p)| pname.clone())
                .collect();
            if missing_peripherals.len() > 0 {
                // Report error
                let msg = format!(
                    "Required peripherals not found on any sockets: {missing_peripherals:?}"
                );
                error!("{msg}");

                // Close sockets and exit
                self.sockets.values_mut().for_each(|sock| sock.close());

                return Err(msg);
            }
        }

        // Initialize state using scanned addresses
        info!("Initializing controller run state.");
        let mut controller_state =
            ControllerState::new(&self.peripherals, &available_peripherals, &self.ctx);
        let addresses = controller_state
            .peripheral_state
            .keys()
            .copied()
            .collect::<Vec<SocketAddr>>();

        // Initialize calc graph
        info!("Initializing calc orchestrator.");
        self.orchestrator
            .init(self.ctx.clone(), &self.peripherals)
            .map_err(|e| format!("Failed to initialize calc orchestrator: {e}"))?;
        self.orchestrator
            .eval()
            .map_err(|e| format!("Failed to evaluate calc orchestrator during init: {e}"))?;

        // Set up dispatcher(s)
        // FUTURE: send metrics to calcs so that they can be used as calc inputs
        info!("Initializing dispatchers.");
        let (n_metrics, n_channels, mut channel_values) = {
            let mut channel_names = Vec::new();
            let metric_channel_names = controller_state.get_names_to_write();
            let io_channel_names = self.orchestrator.get_dispatch_names();
            channel_names.extend(metric_channel_names.iter().cloned());
            channel_names.extend(io_channel_names.iter().cloned());
            let n_metrics = metric_channel_names.len();
            let n_io = io_channel_names.len();
            let n_channels = n_metrics + n_io;
            let channel_values = vec![0.0; n_channels]; // Storage for dispatched values.
            for dispatcher in self.dispatchers.values_mut() {
                dispatcher
                    .init(&self.ctx, &channel_names, aux_core_cycle.next().unwrap().id)
                    .unwrap();
            }

            (n_metrics, n_channels, channel_values)
        };
        info!("Dispatching data for {n_channels} channels.");

        // Bind & configure peripherals.
        let mut all_peripherals_acknowledged = false;
        'configuring_retry: for i in 0..10 {
            info!("Binding peripherals.");

            // If this is a retry, wait for peripherals to time out back to binding.
            if i > 0 {
                // Some peripherals may have received their configuration and proceeded to operating,
                // in which case we need to wait for them to time out and return to Connecting before retry,
                // plus a buffer for the peripheral to proceed back to Binding.
                let pad_ns = 20_000_000;
                let retry_wait = Duration::from_nanos(
                    self.ctx.peripheral_loss_of_contact_limit as u64 * self.ctx.dt_ns as u64
                        + self.ctx.timeout_to_operating_ns as u64
                        + self.ctx.configuring_timeout_ms as u64 * 1000
                        + pad_ns,
                );
                debug!(?retry_wait, "Waiting to retry configuring.");
                thread::sleep(retry_wait);
            }

            // Track binding window deadlines for each peripheral.
            let binding_deadline =
                Instant::now() + Duration::from_millis(self.ctx.binding_timeout_ms as u64);
            for ps in controller_state.peripheral_state.values_mut() {
                ps.conn_state = ConnState::Binding {
                    binding_timeout: binding_deadline,
                    reconnect_deadline: None,
                };
            }

            // Clear buffers.
            loop {
                let mut received_any = false;
                for sock in self.sockets.values_mut() {
                    if sock.recv(rxbuf, Duration::ZERO).is_some() {
                        received_any = true;
                    }
                }
                if !received_any {
                    break;
                }
            }

            // Bind.
            let bound_peripherals = self
                .bind(
                    Some(&addresses),
                    self.ctx.binding_timeout_ms,
                    self.ctx.configuring_timeout_ms,
                    plugins,
                )
                .map_err(|e| format!("Failed to bind peripherals: {e}"))?;

            // Operating countdown starts as soon as peripherals receive binding input,
            // so start the clock now.
            let start_of_operating_countdown = Instant::now();

            // Configure peripherals.
            //    Send configuration to each peripheral.
            info!("Configuring peripherals.");
            let config_input = ConfiguringInput {
                dt_ns: self.ctx.dt_ns,
                timeout_to_operating_ns: self.ctx.timeout_to_operating_ns,
                loss_of_contact_limit: self.ctx.peripheral_loss_of_contact_limit,
                mode: Mode::Roundtrip,
            };
            //     Track configuring window deadlines for peripherals that receive config packets.
            let configuring_deadline = start_of_operating_countdown
                + Duration::from_millis(self.ctx.configuring_timeout_ms as u64);

            let socket_names = self.socket_names();
            for addr in addresses.iter() {
                //     Write configuring packet for this peripheral.
                let (sid, pid) = addr;
                let p = bound_peripherals
                    .get(&(*sid, *pid))
                    .ok_or(format!("Did not find {pid:?} in bound peripherals."))?;
                let num_to_write = p.configuring_input_size();
                p.emit_configuring(config_input, &mut txbuf[..num_to_write]);

                //     Transmit configuring packet.
                let socket = self
                    .socket_by_index_mut(&socket_names, *sid)
                    .ok_or_else(|| format!("Socket index {sid} out of range."))?;
                socket
                    .send(*pid, &txbuf[..num_to_write])
                    .map_err(|e| format!("Failed to send configuration to {pid:?}: {e:?}"))?;

                //     Log expected peripheral state transition.
                let ps = controller_state.peripheral_state.get_mut(addr).unwrap();
                ps.conn_state = ConnState::Configuring {
                    configuring_timeout: configuring_deadline,
                    reconnect_deadline: None,
                };
            }

            //    Wait for peripherals to acknowledge their configuration.
            let operating_timeout = Duration::from_nanos(self.ctx.timeout_to_operating_ns as u64);

            info!("Waiting for peripherals to acknowledge configuration.");
            while start_of_operating_countdown.elapsed() < operating_timeout {
                for (sid, (socket_name, socket)) in self.sockets.iter_mut().enumerate() {
                    if let Some(meta) = socket.recv(rxbuf, Duration::ZERO) {
                        let amt = meta.size;

                        // Make sure the packet is the right size and the peripheral ID is recognized.
                        match meta.pid {
                            Some(pid) => {
                                // Parse the (potential) peripheral's response
                                let p = bound_peripherals.get(&(sid, pid)).unwrap();
                                if amt != p.configuring_output_size() {
                                    warn!(
                                        "Received malformed configuration response from peripheral {pid:?} on socket {sid} ({socket_name})."
                                    );
                                    continue;
                                }
                                let ack = ConfiguringOutput::read_bytes(&rxbuf[..amt]);
                                let addr = (sid, pid);

                                // Check if this is peripheral belongs to this controller.
                                if !controller_state.peripheral_state.contains_key(&addr) {
                                    continue;
                                }

                                // Check status.
                                match ack.acknowledge {
                                    AcknowledgeConfiguration::Ack => {
                                        let ps = controller_state
                                            .peripheral_state
                                            .get_mut(&addr)
                                            .unwrap();
                                        ps.acknowledged_configuration = true;
                                        // Move this peripheral to operating once it acknowledges.
                                        ps.conn_state = ConnState::Operating();
                                    }
                                    _ => {
                                        return Err(format!(
                                            "Peripheral at {addr:?} rejected configuration."
                                        ));
                                    }
                                }
                            }
                            _ => {
                                // This is not a peripheral in the address table, so don't spend
                                // any cycles on a response.
                            }
                        }
                    }
                }

                all_peripherals_acknowledged = controller_state
                    .peripheral_state
                    .values()
                    .all(|ps| ps.acknowledged_configuration);
            }

            if all_peripherals_acknowledged {
                // Track operating transition deadlines for each peripheral.
                for ps in controller_state.peripheral_state.values_mut() {
                    ps.conn_state = ConnState::Operating();
                }
                info!("All peripherals acknowledged configuration.");
                break 'configuring_retry;
            } else {
                // Figure out which peripherals were missing.
                let peripherals_not_acknowledged = controller_state
                    .peripheral_state
                    .iter()
                    .filter_map(|(_k, v)| (!v.acknowledged_configuration).then_some(v.name.clone()))
                    .collect::<Vec<_>>();
                warn!(
                    "Peripherals did not acknowledge configuration: {peripherals_not_acknowledged:?}"
                );
            }
        }

        //    If we reached the end of timeout into Operating and all peripherals
        //    acknowledged their configuration, continue to operating
        if !all_peripherals_acknowledged {
            return Err("Some peripherals did not acknowledge their configuration.".to_string());
        }

        // Create socket orchestrator to manage socket polling strategy.
        info!("Initializing socket orchestrator.");
        let worker_timeout = match self.ctx.loop_method {
            LoopMethod::Performant => Duration::ZERO,
            LoopMethod::Efficient => Duration::from_nanos((self.ctx.dt_ns as u64 / 100).max(1_000)),
        };
        let sockets = std::mem::take(&mut self.sockets)
            .into_iter()
            .collect::<Vec<_>>();
        let mut socket_orchestrator = SocketOrchestrator::new(sockets, &self.ctx, worker_timeout)?;

        //    Pre-allocate storage for reconnection logic
        let reconnect_step_timeout = {
            let min_timeout = Duration::from_millis(10);
            let dt_timeout = Duration::from_nanos(self.ctx.dt_ns as u64 * 3);
            if dt_timeout > min_timeout {
                dt_timeout
            } else {
                min_timeout
            }
        };
        let reconnect_step_timeout_ms =
            reconnect_step_timeout.as_millis().min(u16::MAX as u128) as u16;
        let mut reconnect_broadcasts: BTreeMap<usize, Instant> = BTreeMap::new();
        let mut reconnect_targets: Vec<Vec<(SocketAddr, Option<Instant>)>> = (0
            ..socket_orchestrator.socket_count())
            .map(|_| Vec::new())
            .collect();

        //    Init timing
        info!("Initializing timing controllers.");
        let start_of_operating = Instant::now();
        let cycle_duration = Duration::from_nanos(self.ctx.dt_ns as u64);
        let mut target_time = cycle_duration;
        let mut peripheral_timing: BTreeMap<SocketAddr, (TimingPID, MedianFilter<i64, 7>)> =
            BTreeMap::new();
        for addr in controller_state.peripheral_state.keys() {
            let max_clock_rate_err = 5e-2; // at least 5% tolerance for dev units using onboard clocks
            let ki = 0.00001 * (self.ctx.dt_ns as f64 / 10_000_000_f64);
            // FUTURE: The timing controller gains are hand-tuned and could use more scrutiny
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

        //    Run timed loop
        info!("Entering control loop.");
        let mut i: u64 = 0;
        controller_state.controller_metrics.cycle_time_margin_ns = self.ctx.dt_ns as f64;
        let mut ready_signaled = false;
        loop {
            let time = SystemTime::now();
            let mut t = start_of_operating.elapsed();

            i += 1;
            let tmean: i64 = (target_time - cycle_duration / 2).as_nanos() as i64; // Time to drive peripheral packet arrivals toward
            let timestamp = target_time.as_nanos() as i64;

            // Record timing margin
            let controller_timing_margin = (target_time.as_secs_f64() - t.as_secs_f64()) * 1e9;
            controller_state.controller_metrics.cycle_time_margin_ns = controller_timing_margin;

            // Check for loss of contact
            match self.ctx.loss_of_contact_policy {
                // Exit on loss of contact
                LossOfContactPolicy::Terminate() => {
                    let mut lost_name: Option<String> = None;
                    let limit = self.ctx.controller_loss_of_contact_limit as f64;
                    for p in controller_state.peripheral_state.values_mut() {
                        if p.metrics.loss_of_contact_counter >= limit {
                            p.conn_state = ConnState::Disconnected { deadline: None };
                            if lost_name.is_none() {
                                lost_name = Some(p.name.clone());
                            }
                        }
                    }
                    if let Some(name) = lost_name {
                        self.terminate(
                            &controller_state,
                            peripheral_input_buffer,
                            i,
                            &mut socket_orchestrator,
                        );
                        self.sockets = socket_orchestrator.close().into_iter().collect();
                        let reason = format!("Lost contact with peripheral `{}`", name);
                        error!("{reason}");
                        return Err(reason);
                    }
                }
                // Non-blocking reconnection attempt
                LossOfContactPolicy::Reconnect(reconnect_timeout) => {
                    let now = Instant::now();
                    let limit = self.ctx.controller_loss_of_contact_limit as f64;
                    let mut expired_name: Option<String> = None;
                    for p in controller_state.peripheral_state.values_mut() {
                        // Check loss of contact
                        if p.metrics.loss_of_contact_counter >= limit
                            && matches!(p.conn_state, ConnState::Operating { .. })
                        {
                            let deadline = reconnect_timeout.map(|d| now + d);
                            p.conn_state = ConnState::Disconnected { deadline };
                            warn!("Lost contact with peripheral `{}`", p.name);
                        }

                        // Check binding and configuring deadlines
                        match p.conn_state {
                            ConnState::Binding {
                                binding_timeout,
                                reconnect_deadline,
                            } => {
                                if now >= binding_timeout {
                                    p.conn_state = ConnState::Disconnected {
                                        deadline: reconnect_deadline,
                                    };
                                    // We don't warn here, because if the disconnected state
                                    // persists for a while (like if someone is moving a peripheral
                                    // from one room to another), logging here every few milliseconds
                                    // would produce large and unhelpful log files.
                                }
                            }
                            ConnState::Configuring {
                                configuring_timeout,
                                reconnect_deadline,
                            } => {
                                if now >= configuring_timeout {
                                    p.conn_state = ConnState::Disconnected {
                                        deadline: reconnect_deadline,
                                    };
                                    warn!(
                                        "Did not receive Configuring response from peripheral `{}`",
                                        p.name
                                    );
                                }
                            }
                            _ => {}
                        }

                        // Check overall reconnection deadline (if there is one)
                        let deadline = match p.conn_state {
                            ConnState::Binding {
                                reconnect_deadline, ..
                            } => reconnect_deadline,
                            ConnState::Configuring {
                                reconnect_deadline, ..
                            } => reconnect_deadline,
                            ConnState::Disconnected { deadline } => deadline,
                            ConnState::Operating { .. } => None,
                        };
                        if let Some(deadline) = deadline {
                            if now >= deadline {
                                expired_name = Some(p.name.clone());
                            }
                        }
                    }

                    // Exit if any reconnection attempts have passed the overall
                    // reconnection deadline
                    if let Some(name) = expired_name {
                        self.terminate(
                            &controller_state,
                            peripheral_input_buffer,
                            i,
                            &mut socket_orchestrator,
                        );
                        self.sockets = socket_orchestrator.close().into_iter().collect();
                        let reason =
                            format!("Reconnect timeout exceeded for peripheral `{}`", name);
                        error!("{reason}");
                        return Err(reason);
                    }
                }
            }

            // Check termination criteria
            if let Some(criterion) = &self.ctx.termination_criteria {
                let terminating = match criterion {
                    Termination::Timeout(d) => {
                        if &t >= d {
                            let msg = format!("Reached full duration {:?} at {:?}", &d, &t);
                            info!("{msg}");
                            Some(Ok(msg))
                        } else {
                            None
                        }
                    }
                    Termination::Scheduled(t_sched) => {
                        if t_sched >= &time {
                            let msg = format!(
                                "Reached scheduled termination time {} at {} after {i} cycles",
                                fmt_time(*t_sched),
                                fmt_time(time)
                            );
                            info!("{msg}");
                            Some(Ok(msg))
                        } else {
                            None
                        }
                    }
                };

                if let Some(reason) = terminating {
                    self.terminate(
                        &controller_state,
                        peripheral_input_buffer,
                        i,
                        &mut socket_orchestrator,
                    );
                    self.sockets = socket_orchestrator.close().into_iter().collect();
                    return reason;
                }
            }

            // Check external termination signal
            if let Some(s) = termination_signal {
                if s.load(std::sync::atomic::Ordering::Relaxed) {
                    let msg = format!("External termination signal received at {t:?}");
                    let reason = Ok(msg.clone());
                    info!("{msg}");

                    self.terminate(
                        &controller_state,
                        peripheral_input_buffer,
                        i,
                        &mut socket_orchestrator,
                    );
                    self.sockets = socket_orchestrator.close().into_iter().collect();
                    return reason;
                }
            }

            // Periodically broadcast bind requests on sockets with Disconnected peripherals.
            // To avoid overwhelming the network, we only do this once per reconnect attempt window
            // per socket.
            if let LossOfContactPolicy::Reconnect(_) = self.ctx.loss_of_contact_policy {
                let now = Instant::now();

                // Figure out which peripherals are reconnecting on which sockets
                for (addr, ps) in controller_state.peripheral_state.iter_mut() {
                    // Only visit peripherals in Disconnected state
                    let deadline = match ps.conn_state {
                        ConnState::Disconnected { deadline } => deadline,
                        _ => continue,
                    };

                    // If we're already past the reconnection deadline, skip this one
                    if deadline.map_or(false, |deadline| now >= deadline) {
                        continue;
                    }

                    // If this is a Disconnected peripheral inside its reconnection window,
                    // add it to the list to attempt reconnection.
                    if let Some(targets) = reconnect_targets.get_mut(addr.0) {
                        targets.push((*addr, deadline));
                    }
                }

                // Send bind requests
                for (sid, targets) in reconnect_targets.iter_mut().enumerate() {
                    // Don't spam sockets that don't have any peripherals reconnecting
                    if targets.is_empty() {
                        continue; // Go to the next socket
                    }

                    // Check if it has been long enough since our last broadcast on this socket.
                    // If not, skip broadcasting on this socket until a later cycle.
                    if let Some(last) = reconnect_broadcasts.get(&sid).copied() {
                        if now.duration_since(last) < reconnect_step_timeout {
                            continue; // Go to the next socket
                        }
                    }

                    // Build binding packet
                    let binding_msg = BindingInput {
                        configuring_timeout_ms: reconnect_step_timeout_ms,
                    };
                    // Send broadcast binding packet on this socket only
                    let mut binding_buf = [0_u8; SOCKET_BUFFER_LEN];
                    binding_msg.write_bytes(&mut binding_buf[..BindingInput::BYTE_LEN]);
                    let send_result =
                        socket_orchestrator.broadcast(sid, &binding_buf[..BindingInput::BYTE_LEN]);

                    // If we have lost the ability to transmit on this socket,
                    // log the error, but let the loss of contact logic handle
                    // whether this means the controller should exit.
                    if let Err(err) = send_result {
                        error!("{err}");
                        continue;
                    }

                    // Log this time as the most recent broadcast on this socket
                    reconnect_broadcasts.insert(sid, now);

                    // Transition affected reconnecting peripherals on this socket to Binding
                    let binding_deadline = now + reconnect_step_timeout;
                    for (addr, deadline) in targets.iter().copied() {
                        if let Some(ps) = controller_state.peripheral_state.get_mut(&addr) {
                            ps.acknowledged_configuration = false;
                            ps.conn_state = ConnState::Binding {
                                binding_timeout: binding_deadline,
                                reconnect_deadline: deadline,
                            };
                        }
                    }
                }

                // Clear list of peripherals that are actively reconnecting
                // so that it can be repopulated fresh on the next cycle.
                reconnect_targets
                    .iter_mut()
                    .for_each(|targets| targets.clear());
            }

            // Set manual peripheral inputs from outside the expression graph
            if self.ctx.enable_manual_inputs {
                match self.ctx.manual_inputs.read() {
                    Ok(guard) => {
                        for (name, value) in guard.iter() {
                            if let Err(err) = self.orchestrator.set_manual_input(name, *value) {
                                warn!("{err}");
                            }
                        }
                    }
                    Err(_) => {
                        warn!("Manual input map lock poisoned; skipping manual writes.");
                    }
                }
            }

            // Send next control input
            for (addr, ps) in controller_state.peripheral_state.iter_mut() {
                // Don't spam Operating inputs to peripherals that are in the process
                // of being reconnected
                if !matches!(ps.conn_state, ConnState::Operating { .. }) {
                    continue;
                }
                let p = &self.peripherals[&ps.name];

                // Send packet
                let n = p.operating_roundtrip_input_size();
                let phase_delta_ns = ps.metrics.requested_phase_delta_ns as i64;
                let period_delta_ns = ps.metrics.requested_period_delta_ns as i64;
                //    Write inputs for this peripheral
                self.orchestrator
                    .provide_peripheral_inputs(&ps.name, |vals| {
                        peripheral_input_buffer[..n]
                            .iter_mut()
                            .zip(vals)
                            .for_each(|(old, new)| {
                                *old = new;
                            })
                    });
                //    Form packet to send to this peripheral
                let (sid, pid) = addr;
                //    Transmit the packet
                p.emit_operating_roundtrip(
                    i,
                    period_delta_ns,
                    phase_delta_ns,
                    &peripheral_input_buffer[..n],
                    &mut txbuf[..n],
                );
                let send_result = socket_orchestrator.send(*sid, *pid, &txbuf[..n]);
                if let Err(_e) = send_result {
                    // If transmission fails, the peripheral is responsible for
                    // registering that contact has been lost and will eventually exit the operating state,
                    // after which the controller will start its loss of contact counter for that peripheral,
                    // potentially doubling the number of cycles without active control compared to the loss of contact limit.
                    //
                    // That said, some resilience is required here, because transmission will fail a few times per day
                    // in a typical configuration while the control server's DHCP IP address lease is renewed.
                    // This can be prevented by setting up indefinite leases on a managed router, but we shouldn't
                    // expect that level of micromanagement from a typical user.
                }
            }

            // Receive packets until the start of the next cycle
            //     Unless we hear from each connected peripheral, assume we missed the packet
            for ps in controller_state.peripheral_state.values_mut() {
                if matches!(ps.conn_state, ConnState::Operating()) {
                    ps.metrics.loss_of_contact_counter += 1.0;
                }
            }

            let mut worker_error: Option<String> = None;
            t = start_of_operating.elapsed();
            while start_of_operating.elapsed() < target_time {
                // Set maximum time to wait for next packet.
                let timeout = match self.ctx.loop_method {
                    LoopMethod::Performant => Duration::ZERO, // Busy-wait
                    LoopMethod::Efficient => target_time.saturating_sub(t), // Thread-waker
                };

                // Wait for the next packet
                match socket_orchestrator.recv(rxbuf, timeout) {
                    Ok(Some(meta)) => {
                        let payload = &rxbuf[..meta.size];
                        // Check if this is a reconnection attempt, and if so,
                        // handle the peripheral state transition and address map update.
                        let was_reconnection = self.handle_reconnect_packet(
                            &mut controller_state,
                            &mut socket_orchestrator,
                            &meta,
                            payload,
                            reconnect_step_timeout,
                        );
                        if was_reconnection {
                            continue;
                        }

                        // If this is not a reconnection attempt, process normally.
                        self.process_socket_packet(
                            &mut controller_state,
                            &addresses,
                            start_of_operating,
                            &meta,
                            payload,
                            i,
                        );
                    }
                    Ok(None) => {}
                    Err(err) => {
                        worker_error = Some(err);
                        break;
                    }
                }

                // Exit if any sockets have failed.
                if worker_error.is_some() {
                    break;
                }

                t = start_of_operating.elapsed();
            }

            // Exit if any socket has failed.
            if let Some(err) = worker_error {
                self.terminate(
                    &controller_state,
                    peripheral_input_buffer,
                    i,
                    &mut socket_orchestrator,
                );
                self.sockets = socket_orchestrator.close().into_iter().collect();
                return Err(err);
            }

            // Calculate timing deltas
            // in order to drive all modules toward target sample time
            for ps in controller_state.peripheral_state.values_mut() {
                // If we missed a packet from this peripheral, do nothing until
                // we hear from it again
                if ps.metrics.loss_of_contact_counter > 0.0
                    || !matches!(ps.conn_state, ConnState::Operating())
                {
                    ps.metrics.requested_phase_delta_ns = 0.0;
                    continue;
                }

                // Update phase error estimate
                let dt_err_i64_ns = tmean - ps.metrics.last_received_time_ns;
                let dt_err_ns = dt_err_i64_ns as f64;
                ps.metrics.raw_timing_delta_ns = dt_err_ns;

                // Update the filter and controller for this peripheral's timing
                // Use median filter for data rates above 10Hz, otherwise raw value
                let (c, f) = peripheral_timing.get_mut(&ps.addr).unwrap();
                ps.metrics.filtered_timing_delta_ns = f.update(dt_err_i64_ns) as f64;
                let (period_delta_ns, phase_delta_ns) =
                    c.update(ps.metrics.filtered_timing_delta_ns);

                ps.metrics.requested_phase_delta_ns = phase_delta_ns;
                ps.metrics.requested_period_delta_ns = period_delta_ns;
            }

            // Run calcs
            if let Err(err) = self.orchestrator.eval() {
                self.sockets = socket_orchestrator.close().into_iter().collect();
                return Err(err);
            }

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
            let mut dispatch_errors = Vec::new();
            for (name, dispatcher) in self.dispatchers.iter_mut() {
                if let Err(err) = dispatcher.consume(time, timestamp, channel_values.clone()) {
                    error!("{err}");
                    dispatch_errors.push(format!("{name}: {err}"));
                }
            }

            if !dispatch_errors.is_empty() {
                let msg = dispatch_errors
                    .into_iter()
                    .map(|e| format!("\n  {e}"))
                    .collect::<Vec<_>>()
                    .join("");
                self.sockets = socket_orchestrator.close().into_iter().collect();
                return Err(format!("Dispatcher error(s): {msg}"));
            }

            if !ready_signaled {
                self.ready.mark_ready();
                ready_signaled = true;
            }

            // Update next target time
            target_time += cycle_duration;
        }
    }
}

impl RunHandle {
    /// Signal the controller to stop.
    pub fn stop(&self) {
        self.termination
            .store(true, std::sync::atomic::Ordering::Relaxed);
    }

    /// Check if the controller thread is still running.
    pub fn is_running(&self) -> bool {
        self.join
            .as_ref()
            .map(|h| !h.is_finished())
            .unwrap_or(false)
    }

    /// Check if the controller has completed its first cycle.
    pub fn is_ready(&self) -> bool {
        self.ready.is_ready()
    }

    /// Wait for the controller thread to finish.
    pub fn join(&mut self) -> Result<String, String> {
        match self.join.take() {
            Some(h) => match h.join() {
                Ok(Ok(msg)) => Ok(msg),
                Ok(Err(e)) => Err(e),
                Err(e) => Err(format!("Controller thread panicked: {e:?}")),
            },
            None => Err("Controller thread already joined or not started".to_string()),
        }
    }

    /// Get the latest row: (system_time, timestamp, channel_values).
    pub fn latest_row(&self) -> (String, i64, Vec<f64>) {
        let row: Arc<Row> = self.latest.latest_row();
        (
            row.system_time.clone(),
            row.timestamp,
            row.channel_values.clone(),
        )
    }

    /// Column headers including timestamp/time.
    pub fn headers(&self) -> Vec<String> {
        self.latest.headers()
    }

    /// Read the latest row mapped to header names.
    pub fn read(&self) -> Snapshot {
        let headers = self.latest.headers();
        let row = self.latest.latest_row();
        let mut map = HashMap::new();
        // First two headers are timestamp, time.
        if headers.len() >= 2 {
            for (name, val) in headers.iter().skip(2).zip(row.channel_values.iter()) {
                map.insert(name.clone(), *val);
            }
        }
        Snapshot {
            system_time: row.system_time.clone(),
            timestamp: row.timestamp,
            values: map,
        }
    }

    /// List peripheral inputs that can be written manually.
    pub fn available_inputs(&self) -> Vec<String> {
        self.manual_input_names.clone()
    }

    /// Write values to peripheral inputs not driven by calcs.
    pub fn write(&self, values: HashMap<String, f64>) -> Result<(), String> {
        if !self.is_running() {
            return Err("Controller is not running".to_string());
        }

        if values.is_empty() {
            return Ok(());
        }

        let allowed = &self.manual_input_names;
        for name in values.keys() {
            if !allowed.contains(name) {
                return Err(format!(
                    "Manual input `{name}` is not available for manual writes"
                ));
            }
        }

        let mut manual_guard = self
            .manual_input_values
            .write()
            .map_err(|_| "Manual input map lock poisoned".to_string())?;
        for (name, value) in values {
            manual_guard.insert(name, value);
        }
        Ok(())
    }
}

impl Drop for RunHandle {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(feature = "python")]
#[pymethods]
impl RunHandle {
    #[pyo3(name = "stop")]
    fn py_stop(&self) {
        self.stop();
    }

    #[pyo3(name = "is_running")]
    fn py_is_running(&self) -> bool {
        self.is_running()
    }

    #[pyo3(name = "is_ready")]
    fn py_is_ready(&self) -> bool {
        self.is_ready()
    }

    #[pyo3(name = "join")]
    fn py_join(&mut self) -> PyResult<String> {
        self.join()
            .map_err(|e| PyErr::from(BackendErr::RunErr { msg: e }))
    }

    #[pyo3(name = "latest_row")]
    fn py_latest_row(&self) -> (String, i64, Vec<f64>) {
        self.latest_row()
    }

    #[pyo3(name = "headers")]
    fn py_headers(&self) -> Vec<String> {
        self.headers()
    }

    #[pyo3(name = "read")]
    fn py_read(&self) -> Snapshot {
        self.read()
    }

    #[pyo3(name = "available_inputs")]
    fn py_available_inputs(&self) -> Vec<String> {
        self.available_inputs()
    }

    #[pyo3(name = "write")]
    fn py_write(&self, values: HashMap<String, f64>) -> PyResult<()> {
        self.write(values)
            .map_err(|e| PyErr::from(BackendErr::RunErr { msg: e }))
    }

    fn __enter__(slf: PyRefMut<'_, Self>) -> PyResult<PyRefMut<'_, Self>> {
        Ok(slf)
    }

    fn __exit__(
        &mut self,
        _exc_type: Option<Py<PyAny>>,
        _exc: Option<Py<PyAny>>,
        _traceback: Option<Py<PyAny>>,
    ) -> PyResult<bool> {
        self.stop();
        Ok(false)
    }
}

#[cfg(feature = "python")]
#[pymethods]
impl Snapshot {
    #[getter]
    #[pyo3(name = "system_time")]
    fn py_system_time(&self) -> String {
        self.system_time.clone()
    }

    #[getter]
    #[pyo3(name = "timestamp")]
    fn py_timestamp(&self) -> i64 {
        self.timestamp
    }

    #[getter]
    #[pyo3(name = "values")]
    fn py_values(&self) -> HashMap<String, f64> {
        self.values.clone()
    }
}

#[cfg(test)]
mod test {

    /// Make sure that we can serialize _and_ deserialize a full controller.
    /// It is possible to produce a system where a serialized output is not able to be
    /// deserialized without error due to type ambiguity in `dyn Trait` collections,
    /// which is resolved via type tagging here.
    #[test]
    fn test_ser_roundtrip() {
        use super::*;

        let mut controller = Controller::default();
        let per = crate::peripheral::analog_i_rev_2::AnalogIRev2 { serial_number: 0 };
        controller
            .peripherals
            .insert("test".to_owned(), Box::new(per));

        let serialized = serde_json::to_string(&controller).unwrap();
        let deserialized = serde_json::from_str::<Controller>(&serialized).unwrap();
        let reserialized = serde_json::to_string(&deserialized).unwrap();

        assert_eq!(serialized, reserialized);
        debug!("Serialized controller state: {serialized}");
    }
}
