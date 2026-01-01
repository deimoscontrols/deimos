//! Peripheral wrapper that provides software-defined behavior with an internal state machine.

use std::collections::BTreeMap;
use std::net::{Ipv4Addr, SocketAddr as UdpSocketAddr, UdpSocket};
#[cfg(unix)]
use std::os::unix::net::{SocketAddr as UnixSocketAddr, UnixDatagram};
#[cfg(unix)]
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime};

use serde::{Deserialize, Serialize};

use deimos_shared::OperatingMetrics;
use deimos_shared::PERIPHERAL_RX_PORT;
use deimos_shared::peripherals::PeripheralId;
use deimos_shared::states::{
    BindingInput, BindingOutput, ByteStruct, ByteStructLen, ConfiguringInput, ConfiguringOutput,
};

#[cfg(feature = "python")]
use pyo3::prelude::*;
use tracing::error;
use tracing::info;
use tracing::warn;

use super::Peripheral;
use crate::calc::Calc;
use crate::controller::channel::{Endpoint, Msg};
use crate::controller::context::ControllerCtx;
use crate::py_json_methods;

#[cfg(feature = "python")]
use crate::python::{BackendErr, controller::Controller as PyController};

/// Peripheral wrapper that emits mock outputs using driver-owned state.
///
/// Note: this should be attached via `Controller::attach_hootl_driver` to keep
/// the shared driver state intact. JSON roundtrips will reset the link.
#[derive(Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "python", pyclass)]
pub struct HootlPeripheral {
    inner: Box<dyn Peripheral>,
    #[serde(skip, default = "default_state")]
    state: Arc<Mutex<HootlState>>,
}

impl HootlPeripheral {
    /// Wrap another peripheral.
    fn new_driver_owned(inner: Box<dyn Peripheral>, state: Arc<Mutex<HootlState>>) -> Self {
        Self { inner, state }
    }

    /// Extract wrapped peripheral.
    pub fn into_inner(self) -> Box<dyn Peripheral> {
        self.inner
    }
}

fn default_state() -> Arc<Mutex<HootlState>> {
    Arc::new(Mutex::new(HootlState::default()))
}

py_json_methods!(
    HootlPeripheral,
    Peripheral,
    // Note: we do not implement a python new method here
    // because this struct is not usable when initialized directly
    // in python, as the driver link is not maintained across the
    // interlanguage boundary.
    #[getter]
    fn serial_number(&self) -> u64 {
        self.inner.id().serial_number
    }
);

/// State info to be shared between the HOOTL driver
/// and its wrapped peripheral.
#[derive(Debug)]
struct HootlState {
    mode: HootlMode,
}

impl Default for HootlState {
    fn default() -> Self {
        Self {
            mode: HootlMode::Binding,
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum HootlMode {
    Binding,
    Configuring,
    Operating,
    Terminated,
}

// Mostly pass-through to wrapped peripheral
#[typetag::serde]
impl Peripheral for HootlPeripheral {
    fn id(&self) -> deimos_shared::peripherals::PeripheralId {
        self.inner.id()
    }

    fn input_names(&self) -> Vec<String> {
        self.inner.input_names()
    }

    fn output_names(&self) -> Vec<String> {
        self.inner.output_names()
    }

    fn operating_roundtrip_input_size(&self) -> usize {
        self.inner.operating_roundtrip_input_size()
    }

    fn operating_roundtrip_output_size(&self) -> usize {
        self.inner.operating_roundtrip_output_size()
    }

    fn emit_operating_roundtrip(
        &self,
        id: u64,
        period_delta_ns: i64,
        phase_delta_ns: i64,
        inputs: &[f64],
        bytes: &mut [u8],
    ) {
        self.inner
            .emit_operating_roundtrip(id, period_delta_ns, phase_delta_ns, inputs, bytes);
    }

    fn parse_operating_roundtrip(&self, bytes: &[u8], outputs: &mut [f64]) -> OperatingMetrics {
        let mut metrics = self.inner.parse_operating_roundtrip(bytes, outputs);

        let state = match self.state.lock() {
            Ok(state) => state,
            Err(_) => return metrics,
        };

        match state.mode {
            HootlMode::Operating => {
                // FUTURE: connect this to the driver to drive more interesting logic
                // instead of these placeholder values.
                let counter = metrics.id;
                for (idx, value) in outputs.iter_mut().enumerate() {
                    *value = counter as f64 + (idx as f64) * 0.01;
                }
            }
            HootlMode::Binding | HootlMode::Configuring | HootlMode::Terminated => {
                for value in outputs.iter_mut() {
                    *value = 0.0;
                }
                metrics = OperatingMetrics::default();
            }
        }

        metrics
    }

    fn standard_calcs(&self, name: String) -> BTreeMap<String, Box<dyn Calc>> {
        self.inner.standard_calcs(name)
    }
}

/// Choice of socket type to be used by the HOOTL driver.
#[cfg(unix)]
#[derive(Clone, Debug)]
#[cfg_attr(feature = "python", pyclass)]
pub enum HootlTransport {
    /// A thread channel with this name.
    ThreadChannel { name: String },

    /// A unix socket with this name.
    UnixSocket { name: String },

    /// UDP transport bound to PERIPHERAL_RX_PORT.
    /// Because the port can only be bound once, this can only
    /// be used by one hootl driver at a time.
    Udp(),
}

#[cfg(not(unix))] // Can't put this directive inside pyclass
#[derive(Clone, Debug)]
#[cfg_attr(feature = "python", pyclass)]
pub enum HootlTransport {
    /// A thread channel with this name.
    ThreadChannel { name: String },

    // No unix socket
    /// UDP transport bound to PERIPHERAL_RX_PORT.
    /// Because the port can only be bound once, this can only
    /// be used by one hootl driver at a time.
    Udp(),
}

impl HootlTransport {
    pub fn thread_channel(name: &str) -> Self {
        Self::ThreadChannel {
            name: name.to_owned(),
        }
    }

    #[cfg(unix)]
    pub fn unix_socket(name: &str) -> Self {
        Self::UnixSocket {
            name: name.to_owned(),
        }
    }

    pub fn udp() -> Self {
        Self::Udp()
    }
}

#[cfg(feature = "python")]
#[pymethods]
impl HootlTransport {
    #[staticmethod]
    #[pyo3(name = "thread_channel")]
    fn py_thread_channel(name: &str) -> Self {
        Self::thread_channel(name)
    }

    #[cfg(unix)]
    #[staticmethod]
    #[pyo3(name = "unix_socket")]
    fn py_unix_socket(name: &str) -> Self {
        Self::unix_socket(name)
    }

    #[staticmethod]
    #[pyo3(name = "udp")]
    fn py_udp() -> Self {
        Self::udp()
    }
}

#[derive(Clone, Debug)]
struct HootlConfig {
    peripheral_id: PeripheralId,
    input_size: usize,
    output_size: usize,
    end: Option<SystemTime>,
}

/// A handle to manipulate a Peripheral object
/// in order to imitate hardware in testing.
#[derive(Debug)]
#[cfg_attr(feature = "python", pyclass)]
pub struct HootlDriver {
    config: HootlConfig,
    transport: HootlTransport,
    state: Arc<Mutex<HootlState>>,
}

/// Thread handle with stop signal for HOOTL run threads.
#[cfg_attr(feature = "python", pyclass)]
pub struct HootlRunHandle {
    stop: Arc<AtomicBool>,
    join: Option<JoinHandle<()>>,
}

impl HootlRunHandle {
    /// Write to shared stop signal, indicating that the
    /// thread should exit.
    pub fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }

    /// Check if the thread is still running.
    pub fn is_running(&self) -> bool {
        self.join
            .as_ref()
            .map(|h| !h.is_finished())
            .unwrap_or(false)
    }

    /// Wait for the thread to finish running.
    pub fn join(&mut self) -> Result<(), String> {
        match self.join.take() {
            Some(h) => h
                .join()
                .map_err(|_| "HOOTL runner thread panicked".to_string()),
            None => Err("HOOTL runner thread already joined or not started".to_string()),
        }
    }
}

impl Drop for HootlRunHandle {
    /// Make sure to stop the thread when we exit to avoid leaking resources.
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

impl HootlDriver {
    /// Wrap a peripheral with a software imitation using some choice of
    /// communication medium.
    pub fn new(inner: &dyn Peripheral, transport: HootlTransport) -> Self {
        Self::new_with_state(
            inner,
            transport,
            Arc::new(Mutex::new(HootlState::default())),
        )
    }

    /// Wrap a peripheral with a pre-initialized run state
    /// in order to preserve the shared link.
    fn new_with_state(
        inner: &dyn Peripheral,
        transport: HootlTransport,
        state: Arc<Mutex<HootlState>>,
    ) -> Self {
        Self {
            config: HootlConfig {
                peripheral_id: inner.id(),
                input_size: inner.operating_roundtrip_input_size(),
                output_size: inner.operating_roundtrip_output_size(),
                end: None,
            },
            transport,
            state,
        }
    }

    /// Set scheduled end time.
    pub fn with_end(mut self, end: Option<SystemTime>) -> Self {
        self.config.end = end;
        self
    }

    /// Spawn a new runner and return its handle.
    pub fn run(&self, ctx: &ControllerCtx) -> Result<HootlRunHandle, String> {
        let stop = Arc::new(AtomicBool::new(false));
        let mut runner = HootlRunner::new(self, ctx, stop.clone())?;
        let join = std::thread::Builder::new()
            .name("hootl-runner".to_string())
            .spawn(move || runner.run_loop())
            .expect("HOOTL runner failed to spawn hootl runner thread");
        Ok(HootlRunHandle {
            stop,
            join: Some(join),
        })
    }
}

#[cfg(feature = "python")]
#[pymethods]
impl HootlDriver {
    #[new]
    #[pyo3(signature=(inner, transport, end_epoch_ns=None))]
    fn py_new(
        inner: Box<dyn Peripheral>,
        transport: HootlTransport,
        end_epoch_ns: Option<u64>,
    ) -> PyResult<Self> {
        let mut driver = Self::new(inner.as_ref(), transport);
        let end = match end_epoch_ns {
            Some(ns) => Some(
                SystemTime::UNIX_EPOCH
                    .checked_add(Duration::from_nanos(ns))
                    .ok_or_else(|| {
                        pyo3::exceptions::PyValueError::new_err("Invalid end_epoch_ns")
                    })?,
            ),
            None => None,
        };
        driver = driver.with_end(end);
        Ok(driver)
    }

    fn run_with(&self, controller: &PyController) -> PyResult<HootlRunHandle> {
        let ctx = controller.ctx()?;
        self.run(ctx)
            .map_err(|e| BackendErr::InvalidPeripheralErr { msg: e }.into())
    }
}

#[cfg(feature = "python")]
#[pymethods]
impl HootlRunHandle {
    #[pyo3(name = "stop")]
    fn py_stop(&self) {
        self.stop();
    }

    #[pyo3(name = "is_running")]
    fn py_is_running(&self) -> bool {
        self.is_running()
    }

    #[pyo3(name = "join")]
    fn py_join(&mut self) -> PyResult<()> {
        self.join()
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
        if self.join.is_none() {
            return Ok(false);
        }
        self.stop();
        self.join()
            .map_err(|e| PyErr::from(BackendErr::RunErr { msg: e }))?;
        Ok(false)
    }
}

/// The HOOTL state machine.
struct HootlRunner {
    config: HootlConfig,
    loss_of_contact_timeout: Duration,
    transport: TransportState,
    stop: Arc<AtomicBool>,
    state: Arc<Mutex<HootlState>>,
}

impl HootlRunner {
    /// Prep a driver to run with some stop signal and operation context.
    fn new(
        driver: &HootlDriver,
        ctx: &ControllerCtx,
        stop: Arc<AtomicBool>,
    ) -> Result<Self, String> {
        let mut transport = TransportState::new(driver.transport.clone());
        transport.open(ctx)?;
        let loss_of_contact_timeout =
            Duration::from_nanos(ctx.dt_ns as u64 * ctx.peripheral_loss_of_contact_limit as u64);
        Ok(Self {
            config: driver.config.clone(),
            loss_of_contact_timeout,
            transport,
            stop,
            state: driver.state.clone(),
        })
    }

    /// Change modeled hardware state.
    fn set_mode(&self, mode: HootlMode) {
        if let Ok(mut state) = self.state.lock() {
            state.mode = mode;
        } else {
            warn!("HOOTL runner shared state lock poisoned; unable to update mode.");
        }
    }

    /// Run the driver state machine to mimic hardware.
    fn run_loop(&mut self) {
        let mut buf = vec![0u8; 1522];
        let mut state = DriverState::Binding;
        let mut controller_addr: Option<TransportAddr> = None;
        let loss_of_contact_timeout = self.loss_of_contact_timeout;

        self.set_mode(HootlMode::Binding);

        loop {
            // Check exit criteria
            if self.stop.load(Ordering::Relaxed) {
                break;
            }
            if self.config.end.map_or(false, |end| SystemTime::now() > end) {
                break;
            }

            match state {
                DriverState::Binding => {
                    if let Some((size, addr)) = self.transport.recv_packet(&mut buf) {
                        // Parse incoming packet
                        if size != BindingInput::BYTE_LEN {
                            continue;
                        }
                        let msg = BindingInput::read_bytes(&buf[..size]);
                        let timeout = Duration::from_millis(msg.configuring_timeout_ms as u64);

                        // Build response packet
                        let resp = BindingOutput {
                            peripheral_id: self.config.peripheral_id,
                        };
                        let mut out = vec![0u8; BindingOutput::BYTE_LEN];
                        resp.write_bytes(&mut out);

                        // Send response
                        let send_status = self.transport.send_packet(
                            &out[..],
                            addr.as_ref(),
                            self.config.peripheral_id,
                        );
                        if send_status.is_err() {
                            error!("HOOTL runner failed to send binding response: {send_status:?}");
                            break;
                        }

                        controller_addr = addr;
                        let start = Instant::now();
                        self.set_mode(HootlMode::Configuring);
                        state = DriverState::Configuring { start, timeout };
                        info!("HOOTL runner received binding request; Binding -> Configuring.")
                    } else {
                        thread::sleep(Duration::from_millis(1));
                    }
                }
                DriverState::Configuring { start, timeout } => {
                    if start.elapsed() > timeout {
                        state = DriverState::Binding;
                        controller_addr = None;
                        self.set_mode(HootlMode::Binding);
                        info!(
                            "HOOTL runner did not receive configuration; Configuring -> Binding."
                        );
                        continue;
                    }

                    if let Some((size, _addr)) = self.transport.recv_packet(&mut buf) {
                        // FUTURE: validate configuration.
                        if size != ConfiguringInput::BYTE_LEN {
                            continue;
                        }

                        // Send response to acknowledge configuration.
                        let resp = ConfiguringOutput {
                            acknowledge: deimos_shared::states::AcknowledgeConfiguration::Ack,
                        };
                        let mut out = vec![0u8; ConfiguringOutput::BYTE_LEN];
                        resp.write_bytes(&mut out);

                        let send_status = self.transport.send_packet(
                            &out,
                            controller_addr.as_ref(),
                            self.config.peripheral_id,
                        );
                        if send_status.is_err() {
                            error!(
                                "HOOTL runner failed to send configuring response: {send_status:?}"
                            );
                            break;
                        }

                        // Transition to operating
                        self.set_mode(HootlMode::Operating);
                        state = DriverState::Operating {
                            counter: 0,
                            last_contact: Instant::now(),
                        };
                        info!("HOOTL driver acknowledged config; Configuring -> Operating.");
                    } else {
                        thread::sleep(Duration::from_millis(1));
                    }
                }
                DriverState::Operating {
                    ref mut counter,
                    ref mut last_contact,
                } => {
                    if let Some((size, _addr)) = self.transport.recv_packet(&mut buf) {
                        // FUTURE: parse operating packet & respond to actual content
                        if size != self.config.input_size {
                            continue;
                        }

                        // Reset loss-of-contact counter
                        *last_contact = Instant::now();
                        let last_input_id = if size >= 8 {
                            let mut bytes = [0u8; 8];
                            bytes.copy_from_slice(&buf[..8]);
                            u64::from_le_bytes(bytes)
                        } else {
                            0
                        };

                        // Send operating response
                        // FUTURE: use peripheral object to write output
                        let mut out = vec![0u8; self.config.output_size];
                        let mut metrics = OperatingMetrics::default();
                        metrics.id = *counter;
                        metrics.last_input_id = last_input_id;
                        metrics.write_bytes(&mut out[..OperatingMetrics::BYTE_LEN]);

                        let send_status = self.transport.send_packet(
                            &out,
                            controller_addr.as_ref(),
                            self.config.peripheral_id,
                        );
                        if send_status.is_err() {
                            // Return to Binding on error
                            state = DriverState::Binding;
                            controller_addr = None;
                            self.set_mode(HootlMode::Binding);
                            info!("HOOTL runner failed to send packet; Operating -> Binding.");
                            continue;
                        }

                        *counter = counter.wrapping_add(1);
                    } else {
                        // Check for loss of contact
                        if last_contact.elapsed() >= loss_of_contact_timeout {
                            state = DriverState::Binding;
                            controller_addr = None;
                            self.set_mode(HootlMode::Binding);
                            info!(
                                "HOOTL runner lost contact with controller; Operating -> Binding."
                            );
                            continue;
                        }
                        thread::sleep(Duration::from_millis(1));
                    }
                }
            }
        }

        self.set_mode(HootlMode::Terminated);
        self.transport.close();
    }
}

/// Build a linked peripheral wrapper and driver.
pub(crate) fn build_hootl_pair(
    inner: Box<dyn Peripheral>,
    transport: HootlTransport,
    end: Option<SystemTime>,
) -> (HootlPeripheral, HootlDriver) {
    let state = Arc::new(Mutex::new(HootlState::default()));
    let driver =
        HootlDriver::new_with_state(inner.as_ref(), transport, state.clone()).with_end(end);
    let peripheral = HootlPeripheral::new_driver_owned(inner, state);
    (peripheral, driver)
}

/// State machine info for HOOTL driver.
#[derive(Debug)]
enum DriverState {
    Binding,
    Configuring { start: Instant, timeout: Duration },
    Operating { counter: u64, last_contact: Instant },
}

/// Different kinds of addresses that may be used for different transport layers.
#[derive(Debug)]
enum TransportAddr {
    #[cfg(unix)]
    Unix(UnixSocketAddr),
    Udp(UdpSocketAddr),
}

/// State machine info for HOOTL transport layer.
#[derive(Debug)]
enum TransportState {
    ThreadChannel {
        name: String,
        endpoint: Option<Endpoint>,
    },
    #[cfg(unix)]
    UnixSocket {
        name: String,
        socket: Option<UnixDatagram>,
        path: Option<PathBuf>,
    },
    UdpSocket {
        socket: Option<UdpSocket>,
    },
}

impl TransportState {
    /// Set up a fresh state for this transport layer kind.
    fn new(transport: HootlTransport) -> Self {
        match transport {
            HootlTransport::ThreadChannel { name } => Self::ThreadChannel {
                name,
                endpoint: None,
            },
            #[cfg(unix)]
            HootlTransport::UnixSocket { name } => Self::UnixSocket {
                name,
                socket: None,
                path: None,
            },
            HootlTransport::Udp() => Self::UdpSocket { socket: None },
        }
    }

    /// Open sockets.
    fn open(&mut self, ctx: &ControllerCtx) -> Result<(), String> {
        match self {
            TransportState::ThreadChannel { name, endpoint } => {
                *endpoint = Some(ctx.sink_endpoint(name));
                info!("HOOTL driver opened thread channel socket on user channel `{name}`");
                Ok(())
            }
            #[cfg(unix)]
            TransportState::UnixSocket { name, socket, path } => {
                let socket_path = socket_path(&ctx.op_dir, name);
                if let Some(parent) = socket_path.parent() {
                    std::fs::create_dir_all(parent).map_err(|e| {
                        format!("HOOTL driver unable to create socket folders: {e}")
                    })?;
                }
                if socket_path.exists() {
                    let _ = std::fs::remove_file(&socket_path);
                }
                let sock = UnixDatagram::bind(&socket_path)
                    .map_err(|e| format!("HOOTL driver unable to bind unix socket: {e}"))?;
                sock.set_nonblocking(true).map_err(|e| {
                    format!("HOOTL driver unable to set unix socket to nonblocking mode: {e}")
                })?;
                *socket = Some(sock);
                *path = Some(socket_path.clone());
                info!("HOOTL driver opened unix socket at {socket_path:?}");
                Ok(())
            }
            TransportState::UdpSocket { socket } => {
                let sock = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, PERIPHERAL_RX_PORT))
                    .map_err(|e| format!("HOOTL driver unable to bind UDP socket: {e}"))?;
                sock.set_nonblocking(true).map_err(|e| {
                    format!("HOOTL driver unable to set UDP socket to nonblocking mode: {e}")
                })?;
                *socket = Some(sock);
                info!("HOOTL driver opened UDP socket on 0.0.0.0:{PERIPHERAL_RX_PORT}");
                Ok(())
            }
        }
    }

    /// Close sockets.
    fn close(&mut self) {
        match self {
            TransportState::ThreadChannel { endpoint, .. } => {
                *endpoint = None;
            }
            #[cfg(unix)]
            TransportState::UnixSocket { socket, path, .. } => {
                *socket = None;
                if let Some(path) = path.take() {
                    if let Err(err) = std::fs::remove_file(&path) {
                        warn!("HOOTL driver failed to remove unix socket file {path:?}: {err}");
                    }
                    info!("HOOTL driver closed unix socket at {path:?}");
                }
            }
            TransportState::UdpSocket { socket } => {
                *socket = None;
                info!("HOOTL driver closed UDP socket on 0.0.0.0:{PERIPHERAL_RX_PORT}");
            }
        }
    }

    /// Receive bytes on the socket.
    fn recv_packet(&mut self, buf: &mut [u8]) -> Option<(usize, Option<TransportAddr>)> {
        match self {
            TransportState::ThreadChannel { endpoint, .. } => {
                let endpoint = endpoint.as_ref()?;
                let msg = endpoint.rx().try_recv().ok()?;
                match msg {
                    Msg::Packet(bytes) => {
                        if bytes.len() < PeripheralId::BYTE_LEN {
                            return None;
                        }
                        let payload = &bytes[PeripheralId::BYTE_LEN..];
                        let size = payload.len().min(buf.len());
                        buf[..size].copy_from_slice(&payload[..size]);
                        Some((size, None))
                    }
                    _ => None,
                }
            }
            #[cfg(unix)]
            TransportState::UnixSocket { socket, .. } => {
                let sock = socket.as_mut()?;
                match sock.recv_from(buf).ok() {
                    Some((size, addr)) => Some((size, Some(TransportAddr::Unix(addr)))),
                    None => None,
                }
            }
            TransportState::UdpSocket { socket } => {
                let sock = socket.as_mut()?;
                match sock.recv_from(buf).ok() {
                    Some((size, addr)) => Some((size, Some(TransportAddr::Udp(addr)))),
                    None => None,
                }
            }
        }
    }

    /// Send bytes on the socket.
    fn send_packet(
        &mut self,
        payload: &[u8],
        addr: Option<&TransportAddr>,
        peripheral_id: PeripheralId,
    ) -> Result<(), String> {
        match self {
            TransportState::ThreadChannel { endpoint, .. } => {
                let endpoint = endpoint.as_ref().ok_or_else(|| {
                    "HOOTL driver thread channel endpoint not initialized".to_string()
                })?;
                let mut bytes = vec![0u8; PeripheralId::BYTE_LEN + payload.len()];
                peripheral_id.write_bytes(&mut bytes[..PeripheralId::BYTE_LEN]);
                bytes[PeripheralId::BYTE_LEN..].copy_from_slice(payload);
                endpoint
                    .tx()
                    .send(Msg::Packet(bytes))
                    .map_err(|e| format!("HOOTL driver failed to send thread channel packet: {e}"))
            }
            #[cfg(unix)]
            TransportState::UnixSocket { socket, .. } => {
                let sock = socket
                    .as_ref()
                    .ok_or_else(|| "HOOTL driver unix socket not initialized".to_string())?;
                let addr = match addr {
                    Some(TransportAddr::Unix(addr)) => addr,
                    Some(_) => {
                        return Err("HOOTL driver unexpected controller address type".to_string());
                    }
                    None => return Err("HOOTL driver missing controller address".to_string()),
                };
                sock.send_to_addr(payload, addr)
                    .map_err(|e| format!("HOOTL driver failed to send unix socket packet: {e}"))?;
                Ok(())
            }
            TransportState::UdpSocket { socket } => {
                let sock = socket
                    .as_ref()
                    .ok_or_else(|| "HOOTL driver uDP socket not initialized".to_string())?;
                let addr = match addr {
                    Some(TransportAddr::Udp(addr)) => addr,
                    Some(_) => {
                        return Err("HOOTL driver unexpected controller address type".to_string());
                    }
                    None => return Err("HOOTL driver missing controller address".to_string()),
                };
                sock.send_to(payload, addr)
                    .map_err(|e| format!("HOOTL driver failed to send UDP packet: {e}"))?;
                Ok(())
            }
        }
    }
}

/// Get the expected path to a unix socket with this name.
#[cfg(unix)]
fn socket_path(op_dir: &PathBuf, name: &str) -> PathBuf {
    op_dir.join("sock").join("per").join(name)
}
