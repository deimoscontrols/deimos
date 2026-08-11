//! Controller-facing lifecycle responder shared by instrument integrations.
//!
//! The responder implements the standard Deimos binding, configuring, and
//! operating packet exchange over a one-to-one `ThreadChannelSocket`. It only copies
//! requested and completed state through [`InstrumentProxy`]; all blocking
//! external I/O remains on the instrument worker.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crossbeam::channel::SendTimeoutError;
use deimos_shared::peripherals::PeripheralId;
use deimos_shared::states::{
    AcknowledgeConfiguration, BindingInput, BindingOutput, ByteStruct, ByteStructLen,
    ConfiguringInput, ConfiguringOutput, OperatingMetrics,
};

use crate::controller::Controller;
use crate::controller::context::ControllerCtx;
use crate::controller::socket_channel::SocketEndpoint;
use crate::peripheral::Peripheral;
use crate::socket::thread_channel::ThreadChannelSocket;

const CHANNEL_POLL_INTERVAL: Duration = Duration::from_millis(2);

/// Validated identity and first failure shared with the protocol responder.
#[derive(Debug, Default)]
pub(crate) struct WorkerStatus {
    // Identity is published only after the worker has completed physical setup.
    identity: Option<String>,
    // Retaining the first error preserves the failure that made later state
    // and responses untrustworthy.
    error: Option<String>,
}

impl WorkerStatus {
    pub(crate) fn identity(&self) -> Option<String> {
        self.identity.clone()
    }

    pub(crate) fn set_identity(&mut self, identity: String) {
        self.identity = Some(identity);
    }

    pub(crate) fn error(&self) -> Option<String> {
        self.error.clone()
    }

    pub(crate) fn latch_error(&mut self, error: String) {
        if self.error.is_none() {
            self.error = Some(error);
        }
    }
}

/// Instrument-specific state exchange used by the nonblocking responder.
pub(crate) trait InstrumentProxy: Send + Sync + 'static {
    /// Return the software peripheral identity advertised during binding.
    fn id(&self) -> PeripheralId;
    /// Return the exact operating request size in bytes.
    fn input_size(&self) -> usize;
    /// Return the exact operating response size in bytes.
    fn output_size(&self) -> usize;
    /// Enqueue one correctly sized controller request and return its packet ID.
    fn process_request(&self, bytes: &[u8]) -> u64;
    /// Encode the latest completed state and supplied protocol metrics.
    fn write_response(&self, metrics: OperatingMetrics, bytes: &mut [u8]) -> Result<(), String>;
    /// Request the integration's safe behavior after controller contact is lost.
    fn on_loss_of_contact(&self);
    /// Return a latched worker or validation failure, if present.
    fn error(&self) -> Option<String>;
}

/// Stop and join handle for an instrument's protocol and I/O threads.
///
/// Dropping the handle requests shutdown but does not block. Call [`Self::join`]
/// after stopping the controller to wait for bounded physical cleanup and to
/// observe worker failures.
pub struct InstrumentRunHandle {
    stop: Arc<AtomicBool>,
    protocol: Option<JoinHandle<Result<(), String>>>,
    worker: Option<JoinHandle<Result<(), String>>>,
}

impl InstrumentRunHandle {
    pub(crate) fn new(
        stop: Arc<AtomicBool>,
        protocol: JoinHandle<Result<(), String>>,
        worker: JoinHandle<Result<(), String>>,
    ) -> Self {
        Self {
            stop,
            protocol: Some(protocol),
            worker: Some(worker),
        }
    }

    /// Signal both instrument threads to stop without waiting for them.
    fn request_stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }

    /// Stop and join both threads, reporting panics and latched errors.
    ///
    /// Errors:
    ///   Returns all responder errors, worker errors, and thread panics joined
    ///   into one message.
    pub fn join(&mut self) -> Result<(), String> {
        self.request_stop();
        let mut errors = Vec::new();
        join_one("protocol responder", &mut self.protocol, &mut errors);
        join_one("instrument worker", &mut self.worker, &mut errors);
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors.join("; "))
        }
    }
}

impl Drop for InstrumentRunHandle {
    fn drop(&mut self) {
        self.request_stop();
    }
}

/// Start a worker, wait for validated startup, then start its protocol responder.
pub(crate) fn start_driver<P, W>(
    ctx: &ControllerCtx,
    worker_name: String,
    instrument_name: &'static str,
    startup_timeout: Duration,
    proxy: Arc<P>,
    worker_fn: W,
) -> Result<InstrumentRunHandle, String>
where
    P: InstrumentProxy,
    W: FnOnce(Arc<AtomicBool>, mpsc::SyncSender<Result<(), String>>) -> Result<(), String>
        + Send
        + 'static,
{
    // Reserve the one peripheral endpoint before touching the instrument. A
    // second live responder for this identity therefore fails immediately.
    let endpoint = ctx.peripheral_socket_endpoint(proxy.id())?;
    let stop = Arc::new(AtomicBool::new(false));
    let (startup_tx, startup_rx) = mpsc::sync_channel(1);
    let worker_stop = stop.clone();
    let worker = thread::Builder::new()
        .name(worker_name)
        .spawn(move || worker_fn(worker_stop, startup_tx))
        .map_err(|err| format!("failed to spawn {instrument_name} worker: {err}"))?;

    // Do not advertise the software peripheral until its worker has connected,
    // validated the model, and established the instrument-specific baseline.
    match startup_rx.recv_timeout(startup_timeout) {
        Ok(Ok(())) => {}
        Ok(Err(err)) => {
            let _ = worker.join();
            return Err(err);
        }
        Err(err) => {
            stop.store(true, Ordering::Relaxed);
            let _ = worker.join();
            return Err(format!("{instrument_name} startup did not complete: {err}"));
        }
    }

    let protocol = match spawn_protocol(endpoint, proxy, stop.clone()) {
        Ok(protocol) => protocol,
        Err(err) => {
            stop.store(true, Ordering::Relaxed);
            let _ = worker.join();
            return Err(err);
        }
    };
    Ok(InstrumentRunHandle::new(stop, protocol, worker))
}

/// Start and register a configured instrument with its identity-keyed socket.
pub(crate) fn attach_instrument<P, S>(
    peripheral_name: &str,
    peripheral: P,
    instrument_name: &'static str,
    controller: &mut Controller,
    start: S,
) -> Result<InstrumentRunHandle, String>
where
    P: Peripheral + 'static,
    S: FnOnce(&ControllerCtx) -> Result<InstrumentRunHandle, String>,
{
    if controller.peripherals().contains_key(peripheral_name) {
        return Err(format!("Peripheral name `{peripheral_name}` is duplicated"));
    }
    let id = peripheral.id();
    if controller
        .peripherals()
        .values()
        .any(|existing| existing.id() == id)
    {
        return Err(format!("Peripheral ID `{id:?}` is duplicated"));
    }
    // Start first so a connection/setup failure cannot leave a controller with
    // a registered peripheral that has no functioning instrument behind it.
    let mut handle = start(&controller.ctx)?;
    if let Err(err) = controller.add_peripheral(peripheral_name, Box::new(peripheral)) {
        return match handle.join() {
            Ok(()) => Err(err),
            Err(cleanup) => Err(format!(
                "{err}; additionally failed to stop {instrument_name}: {cleanup}"
            )),
        };
    }
    let socket_name = ThreadChannelSocket::socket_name(id);
    // Replacing an inactive socket drops and releases its controller endpoint.
    // The peripheral endpoint was claimed before worker startup, so an active
    // responder with the same identity has already caused `start` to fail.
    controller.add_socket(&socket_name, Box::new(ThreadChannelSocket::new(id)));
    Ok(handle)
}

/// Consume one thread handle and append any failure to a shared error list.
fn join_one(
    name: &str,
    handle: &mut Option<JoinHandle<Result<(), String>>>,
    errors: &mut Vec<String>,
) {
    let Some(handle) = handle.take() else {
        return;
    };
    match handle.join() {
        Ok(Ok(())) => {}
        Ok(Err(err)) => errors.push(format!("{name}: {err}")),
        Err(_) => errors.push(format!("{name} panicked")),
    }
}

/// Start the controller-facing protocol responder.
///
/// Errors:
///   Returns an error when the operating-system thread cannot be created.
pub(crate) fn spawn_protocol(
    endpoint: SocketEndpoint,
    proxy: Arc<dyn InstrumentProxy>,
    stop: Arc<AtomicBool>,
) -> Result<JoinHandle<Result<(), String>>, String> {
    let id = proxy.id();
    let thread_name = format!(
        "instrument-proxy-{:016x}-{:016x}",
        id.model_number, id.serial_number
    );
    thread::Builder::new()
        .name(thread_name)
        .spawn(move || run_protocol(endpoint, proxy, stop))
        .map_err(|err| format!("failed to spawn instrument protocol responder: {err}"))
}

/// Lifecycle state owned exclusively by the responder thread.
enum State {
    Binding,
    Configuring {
        deadline: Instant,
    },
    Operating {
        response_id: u64,
        last_contact: Instant,
        loss_of_contact_timeout: Duration,
    },
}

/// Serve the Deimos binding, configuration, and operating state machine.
fn run_protocol(
    endpoint: SocketEndpoint,
    proxy: Arc<dyn InstrumentProxy>,
    stop: Arc<AtomicBool>,
) -> Result<(), String> {
    let mut state = State::Binding;
    while !stop.load(Ordering::Relaxed) {
        // A latched physical-I/O error deliberately suppresses valid responses.
        // Existing controller loss-of-contact policy then terminates or reconnects.
        if proxy.error().is_some() {
            thread::sleep(Duration::from_millis(1));
            continue;
        }

        match &mut state {
            State::Binding => {
                let Some(payload) = receive_payload(&endpoint, CHANNEL_POLL_INTERVAL) else {
                    continue;
                };
                if payload.len() != BindingInput::BYTE_LEN {
                    continue;
                }
                let request = BindingInput::read_bytes(&payload);
                let response = BindingOutput {
                    peripheral_id: proxy.id(),
                };
                let mut bytes = vec![0; BindingOutput::BYTE_LEN];
                response.write_bytes(&mut bytes);
                if !send_payload(&endpoint, bytes, &stop) {
                    proxy.on_loss_of_contact();
                    return Ok(());
                }
                state = State::Configuring {
                    deadline: Instant::now()
                        + Duration::from_millis(request.configuring_timeout_ms.into()),
                };
            }
            State::Configuring { deadline } => {
                if Instant::now() >= *deadline {
                    return_to_binding(&mut state, proxy.as_ref());
                    continue;
                }
                let timeout = (*deadline)
                    .saturating_duration_since(Instant::now())
                    .min(CHANNEL_POLL_INTERVAL);
                let Some(payload) = receive_payload(&endpoint, timeout) else {
                    continue;
                };
                if payload.len() != ConfiguringInput::BYTE_LEN {
                    continue;
                }
                let request = ConfiguringInput::read_bytes(&payload);
                let response = ConfiguringOutput {
                    acknowledge: AcknowledgeConfiguration::Ack,
                };
                let mut bytes = vec![0; ConfiguringOutput::BYTE_LEN];
                response.write_bytes(&mut bytes);
                if !send_payload(&endpoint, bytes, &stop) {
                    proxy.on_loss_of_contact();
                    return Ok(());
                }
                let timeout_ns = u64::from(request.dt_ns)
                    .saturating_mul(u64::from(request.loss_of_contact_limit))
                    .max(1_000_000);
                state = State::Operating {
                    // Response IDs are scoped to one Operating session, just as
                    // they are for hardware peripheral protocol responders.
                    response_id: 1,
                    last_contact: Instant::now(),
                    loss_of_contact_timeout: Duration::from_nanos(timeout_ns),
                };
            }
            State::Operating {
                response_id,
                last_contact,
                loss_of_contact_timeout,
            } => {
                let timeout = (*loss_of_contact_timeout)
                    .saturating_sub(last_contact.elapsed())
                    .min(CHANNEL_POLL_INTERVAL);
                if let Some(payload) = receive_payload(&endpoint, timeout) {
                    if payload.len() != proxy.input_size() {
                        continue;
                    }
                    let last_input_id = proxy.process_request(&payload);
                    *last_contact = Instant::now();
                    let metrics = OperatingMetrics {
                        id: *response_id,
                        last_input_id,
                        ..OperatingMetrics::default()
                    };
                    let mut bytes = vec![0; proxy.output_size()];
                    proxy.write_response(metrics, &mut bytes)?;
                    if !send_payload(&endpoint, bytes, &stop) {
                        proxy.on_loss_of_contact();
                        return Ok(());
                    }
                    *response_id = response_id.wrapping_add(1);
                } else if last_contact.elapsed() >= *loss_of_contact_timeout {
                    return_to_binding(&mut state, proxy.as_ref());
                }
            }
        }
    }
    proxy.on_loss_of_contact();
    Ok(())
}

/// Reenter Binding only after requesting the integration's safe behavior.
fn return_to_binding(state: &mut State, proxy: &dyn InstrumentProxy) {
    // The hook is intentionally issued on every transition, even if Binding
    // traffic follows immediately. Output drivers treat it as a priority
    // request, so a rapid rebind cannot overwrite the safety transition.
    proxy.on_loss_of_contact();
    *state = State::Binding;
}

/// Receive one complete packet from the dedicated controller endpoint.
fn receive_payload(endpoint: &SocketEndpoint, timeout: Duration) -> Option<Vec<u8>> {
    endpoint.recv_timeout(timeout).ok()
}

/// Send one complete response, returning `false` on shutdown or controller
/// disconnection.
fn send_payload(endpoint: &SocketEndpoint, payload: Vec<u8>, stop: &AtomicBool) -> bool {
    let mut message = payload;
    loop {
        if stop.load(Ordering::Relaxed) {
            return false;
        }
        match endpoint.send_timeout(message, CHANNEL_POLL_INTERVAL) {
            Ok(()) => return true,
            Err(SendTimeoutError::Timeout(unsent)) => message = unsent,
            Err(SendTimeoutError::Disconnected(_)) => return false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    use deimos_shared::states::{Mode, OperatingMetrics};

    const TEST_ID: PeripheralId = PeripheralId {
        model_number: 99,
        serial_number: 7,
    };

    #[test]
    fn worker_status_preserves_the_first_error() {
        let mut status = WorkerStatus::default();
        status.set_identity("vendor,model,serial,version".to_owned());
        status.latch_error("first".to_owned());
        status.latch_error("second".to_owned());
        assert_eq!(
            status.identity().as_deref(),
            Some("vendor,model,serial,version")
        );
        assert_eq!(status.error().as_deref(), Some("first"));
    }

    struct TestProxy {
        losses: AtomicUsize,
    }

    impl InstrumentProxy for TestProxy {
        fn id(&self) -> PeripheralId {
            TEST_ID
        }

        fn input_size(&self) -> usize {
            8
        }

        fn output_size(&self) -> usize {
            OperatingMetrics::BYTE_LEN
        }

        fn process_request(&self, bytes: &[u8]) -> u64 {
            u64::from_le_bytes(bytes.try_into().unwrap())
        }

        fn write_response(
            &self,
            metrics: OperatingMetrics,
            bytes: &mut [u8],
        ) -> Result<(), String> {
            metrics.write_bytes(bytes);
            Ok(())
        }

        fn on_loss_of_contact(&self) {
            self.losses.fetch_add(1, Ordering::Relaxed);
        }

        fn error(&self) -> Option<String> {
            None
        }
    }

    fn send(endpoint: &SocketEndpoint, payload: &[u8]) {
        endpoint.send(payload.to_vec()).unwrap();
    }

    fn receive(endpoint: &SocketEndpoint) -> Vec<u8> {
        endpoint.recv_timeout(Duration::from_secs(1)).unwrap()
    }

    #[test]
    fn responder_completes_lifecycle_and_increments_response_ids() {
        let ctx = ControllerCtx::default();
        let endpoint = ctx.controller_socket_endpoint(TEST_ID).unwrap();
        let responder_endpoint = ctx.peripheral_socket_endpoint(TEST_ID).unwrap();
        let proxy = Arc::new(TestProxy {
            losses: AtomicUsize::new(0),
        });
        let stop = Arc::new(AtomicBool::new(false));
        let thread = spawn_protocol(responder_endpoint, proxy.clone(), stop.clone()).unwrap();

        let binding = BindingInput {
            configuring_timeout_ms: 100,
        };
        let mut bytes = vec![0; BindingInput::BYTE_LEN];
        binding.write_bytes(&mut bytes);
        send(&endpoint, &bytes);
        assert_eq!(
            BindingOutput::read_bytes(&receive(&endpoint)).peripheral_id,
            TEST_ID
        );

        let configuring = ConfiguringInput {
            dt_ns: 10_000_000,
            mode: Mode::Roundtrip,
            timeout_to_operating_ns: 0,
            loss_of_contact_limit: 10,
        };
        bytes.resize(ConfiguringInput::BYTE_LEN, 0);
        configuring.write_bytes(&mut bytes);
        send(&endpoint, &bytes);
        assert!(matches!(
            ConfiguringOutput::read_bytes(&receive(&endpoint)).acknowledge,
            AcknowledgeConfiguration::Ack
        ));

        for request_id in [41_u64, 42] {
            send(&endpoint, &request_id.to_le_bytes());
            let metrics = OperatingMetrics::read_bytes(&receive(&endpoint));
            assert_eq!(metrics.id, request_id - 40);
            assert_eq!(metrics.last_input_id, request_id);
        }

        stop.store(true, Ordering::Relaxed);
        thread.join().unwrap().unwrap();
        assert_eq!(proxy.losses.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn configuring_timeout_requests_safe_state_before_returning_to_binding() {
        let ctx = ControllerCtx::default();
        let endpoint = ctx.controller_socket_endpoint(TEST_ID).unwrap();
        let responder_endpoint = ctx.peripheral_socket_endpoint(TEST_ID).unwrap();
        let proxy = Arc::new(TestProxy {
            losses: AtomicUsize::new(0),
        });
        let stop = Arc::new(AtomicBool::new(false));
        let thread = spawn_protocol(responder_endpoint, proxy.clone(), stop.clone()).unwrap();

        let binding = BindingInput {
            configuring_timeout_ms: 1,
        };
        let mut bytes = vec![0; BindingInput::BYTE_LEN];
        binding.write_bytes(&mut bytes);
        send(&endpoint, &bytes);
        let _ = receive(&endpoint);

        let deadline = Instant::now() + Duration::from_secs(1);
        while proxy.losses.load(Ordering::Relaxed) == 0 {
            assert!(
                Instant::now() < deadline,
                "responder did not request safe state"
            );
            thread::sleep(Duration::from_millis(1));
        }

        send(&endpoint, &bytes);
        assert_eq!(
            BindingOutput::read_bytes(&receive(&endpoint)).peripheral_id,
            TEST_ID
        );
        stop.store(true, Ordering::Relaxed);
        thread.join().unwrap().unwrap();
    }
}
