//! Peripheral wrapper that provides software-defined behavior with an internal state machine.

use std::collections::BTreeMap;
use std::os::unix::net::SocketAddr as UnixSocketAddr;
use std::os::unix::net::UnixDatagram;
use std::path::PathBuf;
use std::sync::Mutex;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime};

use serde::{Deserialize, Serialize};

use deimos_shared::OperatingMetrics;
use deimos_shared::peripherals::PeripheralId;
use deimos_shared::states::{
    BindingInput, BindingOutput, ByteStruct, ByteStructLen, ConfiguringInput, ConfiguringOutput,
};

#[cfg(feature = "python")]
use pyo3::prelude::*;
use tracing::error;
use tracing::info;
use tracing::warn;

use crate::calc::Calc;
use crate::controller::channel::{Endpoint, Msg};
use crate::controller::context::ControllerCtx;
use crate::py_json_methods;

use super::Peripheral;

#[cfg(feature = "python")]
use crate::python::{BackendErr, Controller as PyController};

/// Peripheral wrapper that emits mock outputs using the ipc_mockup-style state machine.
#[derive(Serialize, Deserialize, Debug)]
#[cfg_attr(feature = "python", pyclass)]
pub struct HootlMockupPeripheral {
    inner: Box<dyn Peripheral>,
    configuring_timeout: Duration,
    end: Option<SystemTime>,
    #[serde(skip)]
    state: Mutex<MockState>,
}

impl HootlMockupPeripheral {
    pub fn new(
        inner: Box<dyn Peripheral>,
        configuring_timeout: Duration,
        end: Option<SystemTime>,
    ) -> Self {
        Self {
            inner,
            configuring_timeout,
            end,
            state: Mutex::new(MockState::default()),
        }
    }
}

py_json_methods!(
    HootlMockupPeripheral,
    Peripheral,
    #[new]
    #[pyo3(signature=(inner, configuring_timeout_ms=250, end_epoch_ns=None))]
    fn py_new(
        inner: Box<dyn Peripheral>,
        configuring_timeout_ms: u64,
        end_epoch_ns: Option<u64>,
    ) -> PyResult<Self> {
        let configuring_timeout = Duration::from_millis(configuring_timeout_ms);
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
        Ok(Self::new(inner, configuring_timeout, end))
    },
    #[getter]
    fn serial_number(&self) -> u64 {
        self.inner.id().serial_number
    }
);

#[derive(Debug)]
struct MockState {
    mode: MockMode,
    counter: u64,
}

impl Default for MockState {
    fn default() -> Self {
        Self {
            mode: MockMode::Binding,
            counter: 0,
        }
    }
}

#[derive(Debug)]
enum MockMode {
    Binding,
    Configuring { start: Instant },
    Operating,
    Terminated,
}

impl MockState {
    fn advance(&mut self, now: SystemTime, timeout: Duration, end: Option<SystemTime>) {
        if end.map_or(false, |end| now > end) {
            self.mode = MockMode::Terminated;
            return;
        }

        let next = match std::mem::replace(&mut self.mode, MockMode::Terminated) {
            MockMode::Binding => MockMode::Configuring {
                start: Instant::now(),
            },
            MockMode::Configuring { start } => {
                if start.elapsed() > timeout {
                    MockMode::Binding
                } else {
                    self.counter = 0;
                    MockMode::Operating
                }
            }
            MockMode::Operating => MockMode::Operating,
            MockMode::Terminated => MockMode::Terminated,
        };

        self.mode = next;
    }
}

#[typetag::serde]
impl Peripheral for HootlMockupPeripheral {
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

        let mut state = match self.state.lock() {
            Ok(state) => state,
            Err(_) => return metrics,
        };
        state.advance(SystemTime::now(), self.configuring_timeout, self.end);

        match state.mode {
            MockMode::Operating => {
                for (idx, value) in outputs.iter_mut().enumerate() {
                    *value = state.counter as f64 + (idx as f64) * 0.01;
                }
                metrics.id = state.counter;
                state.counter = state.counter.wrapping_add(1);
            }
            MockMode::Binding | MockMode::Configuring { .. } | MockMode::Terminated => {
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

#[derive(Clone, Debug)]
#[cfg_attr(feature = "python", pyclass)]
pub enum MockupTransport {
    ThreadChannel { name: String },
    UnixSocket { name: String },
}

#[cfg_attr(feature = "python", pymethods)]
impl MockupTransport {
    #[staticmethod]
    pub fn thread_channel(name: &str) -> Self {
        Self::ThreadChannel {
            name: name.to_owned(),
        }
    }

    #[staticmethod]
    pub fn unix_socket(name: &str) -> Self {
        Self::UnixSocket {
            name: name.to_owned(),
        }
    }
}

#[derive(Clone, Debug)]
struct MockupConfig {
    peripheral_id: PeripheralId,
    input_size: usize,
    output_size: usize,
    end: Option<SystemTime>,
}

#[derive(Debug)]
#[cfg_attr(feature = "python", pyclass)]
pub struct MockupDriver {
    config: MockupConfig,
    transport: MockupTransport,
}

impl MockupDriver {
    pub fn new(inner: &dyn Peripheral, transport: MockupTransport) -> Self {
        Self {
            config: MockupConfig {
                peripheral_id: inner.id(),
                input_size: inner.operating_roundtrip_input_size(),
                output_size: inner.operating_roundtrip_output_size(),
                end: None,
            },
            transport,
        }
    }

    pub fn with_end(mut self, end: Option<SystemTime>) -> Self {
        self.config.end = end;
        self
    }

    pub fn run(&self, ctx: &ControllerCtx) -> Result<JoinHandle<()>, String> {
        let mut runner = MockupRunner::new(self, ctx)?;
        Ok(thread::spawn(move || runner.run_loop()))
    }
}

#[cfg(feature = "python")]
#[pymethods]
impl MockupDriver {
    #[new]
    #[pyo3(signature=(inner, transport, end_epoch_ns=None))]
    fn py_new(
        inner: Box<dyn Peripheral>,
        transport: MockupTransport,
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

    fn run_with(&self, controller: &PyController) -> PyResult<()> {
        let ctx = controller.ctx()?;
        self.run(ctx)
            .map(|_| ())
            .map_err(|e| BackendErr::InvalidPeripheralErr { msg: e }.into())
    }
}

struct MockupRunner {
    config: MockupConfig,
    transport: TransportState,
}

impl MockupRunner {
    fn new(driver: &MockupDriver, ctx: &ControllerCtx) -> Result<Self, String> {
        let mut transport = TransportState::new(driver.transport.clone());
        transport.open(ctx)?;
        Ok(Self {
            config: driver.config.clone(),
            transport,
        })
    }

    fn run_loop(&mut self) {
        let mut buf = vec![0u8; 1522];
        let mut state = DriverState::Binding;
        let mut controller_addr: Option<UnixSocketAddr> = None;

        loop {
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
                            error!("Mockup runner failed to send packet: {send_status:?}");
                            break;
                        }

                        controller_addr = addr;
                        state = DriverState::Configuring {
                            start: Instant::now(),
                            timeout,
                        };
                    } else {
                        thread::sleep(Duration::from_millis(1));
                    }
                }
                DriverState::Configuring { start, timeout } => {
                    if start.elapsed() > timeout {
                        state = DriverState::Binding;
                        controller_addr = None;
                        continue;
                    }

                    if let Some((size, _addr)) = self.transport.recv_packet(&mut buf) {
                        if size != ConfiguringInput::BYTE_LEN {
                            continue;
                        }
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
                            error!("Mockup runner failed to send packet: {send_status:?}");
                            break;
                        }

                        state = DriverState::Operating { counter: 0 };
                    } else {
                        thread::sleep(Duration::from_millis(1));
                    }
                }
                DriverState::Operating { ref mut counter } => {
                    if let Some((size, _addr)) = self.transport.recv_packet(&mut buf) {
                        if size != self.config.input_size {
                            continue;
                        }

                        let last_input_id = if size >= 8 {
                            let mut bytes = [0u8; 8];
                            bytes.copy_from_slice(&buf[..8]);
                            u64::from_le_bytes(bytes)
                        } else {
                            0
                        };

                        let mut out = vec![0u8; self.config.output_size];
                        if self.config.output_size >= OperatingMetrics::BYTE_LEN {
                            let mut metrics = OperatingMetrics::default();
                            metrics.id = *counter;
                            metrics.last_input_id = last_input_id;
                            metrics.write_bytes(&mut out[..OperatingMetrics::BYTE_LEN]);
                        }

                        let send_status = self.transport.send_packet(
                            &out,
                            controller_addr.as_ref(),
                            self.config.peripheral_id,
                        );
                        if send_status.is_err() {
                            error!("Mockup runner failed to send packet: {send_status:?}");
                            break;
                        }

                        *counter = counter.wrapping_add(1);
                    } else {
                        thread::sleep(Duration::from_millis(1));
                    }
                }
            }
        }

        self.transport.close();
    }
}

#[derive(Debug)]
enum DriverState {
    Binding,
    Configuring { start: Instant, timeout: Duration },
    Operating { counter: u64 },
}

#[derive(Debug)]
enum TransportState {
    ThreadChannel {
        name: String,
        endpoint: Option<Endpoint>,
    },
    UnixSocket {
        name: String,
        socket: Option<UnixDatagram>,
        path: Option<PathBuf>,
    },
}

impl TransportState {
    fn new(transport: MockupTransport) -> Self {
        match transport {
            MockupTransport::ThreadChannel { name } => Self::ThreadChannel {
                name,
                endpoint: None,
            },
            MockupTransport::UnixSocket { name } => Self::UnixSocket {
                name,
                socket: None,
                path: None,
            },
        }
    }

    fn open(&mut self, ctx: &ControllerCtx) -> Result<(), String> {
        match self {
            TransportState::ThreadChannel { name, endpoint } => {
                *endpoint = Some(ctx.sink_endpoint(name));
                info!("Opened thread channel socket on user channel `{name}`");
                Ok(())
            }
            TransportState::UnixSocket { name, socket, path } => {
                let socket_path = socket_path(&ctx.op_dir, name);
                if let Some(parent) = socket_path.parent() {
                    std::fs::create_dir_all(parent)
                        .map_err(|e| format!("Unable to create socket folders: {e}"))?;
                }
                if socket_path.exists() {
                    let _ = std::fs::remove_file(&socket_path);
                }
                let sock = UnixDatagram::bind(&socket_path)
                    .map_err(|e| format!("Unable to bind unix socket: {e}"))?;
                sock.set_nonblocking(true)
                    .map_err(|e| format!("Unable to set unix socket to nonblocking mode: {e}"))?;
                *socket = Some(sock);
                *path = Some(socket_path.clone());
                info!("Opened unix socket at {socket_path:?}");
                Ok(())
            }
        }
    }

    fn close(&mut self) {
        match self {
            TransportState::ThreadChannel { endpoint, .. } => {
                *endpoint = None;
            }
            TransportState::UnixSocket { socket, path, .. } => {
                *socket = None;
                if let Some(path) = path.take() {
                    if let Err(err) = std::fs::remove_file(&path) {
                        warn!("Failed to remove unix socket file {path:?}: {err}");
                    }
                }
                info!("Closed unix socket at {path:?}");
            }
        }
    }

    fn recv_packet(&mut self, buf: &mut [u8]) -> Option<(usize, Option<UnixSocketAddr>)> {
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
            TransportState::UnixSocket { socket, .. } => {
                let sock = socket.as_mut()?;
                match sock.recv_from(buf).ok() {
                    Some((size, addr)) => Some((size, Some(addr))),
                    None => None,
                }
            }
        }
    }

    fn send_packet(
        &mut self,
        payload: &[u8],
        addr: Option<&UnixSocketAddr>,
        peripheral_id: PeripheralId,
    ) -> Result<(), String> {
        match self {
            TransportState::ThreadChannel { endpoint, .. } => {
                let endpoint = endpoint
                    .as_ref()
                    .ok_or_else(|| "Thread channel endpoint not initialized".to_string())?;
                let mut bytes = vec![0u8; PeripheralId::BYTE_LEN + payload.len()];
                peripheral_id.write_bytes(&mut bytes[..PeripheralId::BYTE_LEN]);
                bytes[PeripheralId::BYTE_LEN..].copy_from_slice(payload);
                endpoint
                    .tx()
                    .send(Msg::Packet(bytes))
                    .map_err(|e| format!("Failed to send thread channel packet: {e}"))
            }
            TransportState::UnixSocket { socket, .. } => {
                let sock = socket
                    .as_ref()
                    .ok_or_else(|| "Unix socket not initialized".to_string())?;
                let addr = addr.ok_or_else(|| "Missing controller address".to_string())?;
                sock.send_to_addr(payload, addr)
                    .map_err(|e| format!("Failed to send unix socket packet: {e}"))?;
                Ok(())
            }
        }
    }
}

fn socket_path(op_dir: &PathBuf, name: &str) -> PathBuf {
    op_dir.join("sock").join("per").join(name)
}
