use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use deimos_shared::peripherals::PeripheralId;
use deimos_shared::states::{
    AcknowledgeConfiguration, BindingInput, BindingOutput, ByteStruct, ByteStructLen,
    ConfiguringInput, ConfiguringOutput, OperatingMetrics,
};

use crate::controller::channel::{Endpoint, Msg};
use crate::controller::context::ControllerCtx;

pub(crate) trait InstrumentProxy: Send + Sync + 'static {
    fn id(&self) -> PeripheralId;
    fn input_size(&self) -> usize;
    fn output_size(&self) -> usize;
    fn process_request(&self, bytes: &[u8]) -> Result<u64, String>;
    fn write_response(&self, metrics: OperatingMetrics, bytes: &mut [u8]) -> Result<(), String>;
    fn on_loss_of_contact(&self);
    fn error(&self) -> Option<String>;
}

/// Stop and join handle for an instrument's protocol and I/O threads.
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

    /// Signal both instrument threads to stop.
    pub fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }

    /// Whether either instrument thread remains active.
    pub fn is_running(&self) -> bool {
        self.protocol
            .as_ref()
            .is_some_and(|join| !join.is_finished())
            || self.worker.as_ref().is_some_and(|join| !join.is_finished())
    }

    /// Stop and join both threads, reporting panics and latched errors.
    pub fn join(&mut self) -> Result<(), String> {
        self.stop();
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
        self.stop();
    }
}

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

pub(crate) fn spawn_protocol(
    ctx: &ControllerCtx,
    channel_name: &str,
    proxy: Arc<dyn InstrumentProxy>,
    stop: Arc<AtomicBool>,
) -> Result<JoinHandle<Result<(), String>>, String> {
    let endpoint = ctx.sink_endpoint(channel_name);
    let thread_name = format!("instrument-proxy-{channel_name}");
    thread::Builder::new()
        .name(thread_name)
        .spawn(move || run_protocol(endpoint, proxy, stop))
        .map_err(|err| format!("failed to spawn instrument protocol responder: {err}"))
}

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

fn run_protocol(
    endpoint: Endpoint,
    proxy: Arc<dyn InstrumentProxy>,
    stop: Arc<AtomicBool>,
) -> Result<(), String> {
    let mut state = State::Binding;
    while !stop.load(Ordering::Relaxed) {
        if proxy.error().is_some() {
            thread::sleep(Duration::from_millis(1));
            continue;
        }

        match &mut state {
            State::Binding => {
                let Some(payload) = receive_payload(&endpoint, Duration::from_millis(2)) else {
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
                send_payload(&endpoint, proxy.id(), bytes)?;
                state = State::Configuring {
                    deadline: Instant::now()
                        + Duration::from_millis(request.configuring_timeout_ms.into()),
                };
            }
            State::Configuring { deadline } => {
                if Instant::now() >= *deadline {
                    state = State::Binding;
                    continue;
                }
                let timeout = (*deadline)
                    .saturating_duration_since(Instant::now())
                    .min(Duration::from_millis(2));
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
                send_payload(&endpoint, proxy.id(), bytes)?;
                let timeout_ns = u64::from(request.dt_ns)
                    .saturating_mul(u64::from(request.loss_of_contact_limit))
                    .max(1_000_000);
                state = State::Operating {
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
                    .min(Duration::from_millis(2));
                if let Some(payload) = receive_payload(&endpoint, timeout) {
                    if payload.len() != proxy.input_size() {
                        continue;
                    }
                    let last_input_id = match proxy.process_request(&payload) {
                        Ok(id) => id,
                        Err(_) => continue,
                    };
                    *last_contact = Instant::now();
                    let metrics = OperatingMetrics {
                        id: *response_id,
                        last_input_id,
                        ..OperatingMetrics::default()
                    };
                    let mut bytes = vec![0; proxy.output_size()];
                    proxy.write_response(metrics, &mut bytes)?;
                    send_payload(&endpoint, proxy.id(), bytes)?;
                    *response_id = response_id.wrapping_add(1);
                } else if last_contact.elapsed() >= *loss_of_contact_timeout {
                    proxy.on_loss_of_contact();
                    state = State::Binding;
                }
            }
        }
    }
    proxy.on_loss_of_contact();
    Ok(())
}

fn receive_payload(endpoint: &Endpoint, timeout: Duration) -> Option<Vec<u8>> {
    let message = endpoint.rx().recv_timeout(timeout).ok()?;
    let Msg::Packet(bytes) = message else {
        return None;
    };
    if bytes.len() < PeripheralId::BYTE_LEN {
        return None;
    }
    Some(bytes[PeripheralId::BYTE_LEN..].to_vec())
}

fn send_payload(endpoint: &Endpoint, id: PeripheralId, payload: Vec<u8>) -> Result<(), String> {
    let mut bytes = vec![0; PeripheralId::BYTE_LEN + payload.len()];
    id.write_bytes(&mut bytes[..PeripheralId::BYTE_LEN]);
    bytes[PeripheralId::BYTE_LEN..].copy_from_slice(&payload);
    endpoint
        .tx()
        .send(Msg::Packet(bytes))
        .map_err(|err| format!("failed to send instrument proxy packet: {err}"))
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

        fn process_request(&self, bytes: &[u8]) -> Result<u64, String> {
            Ok(u64::from_le_bytes(bytes.try_into().unwrap()))
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

    fn send(endpoint: &Endpoint, payload: &[u8]) {
        let mut framed = vec![0; PeripheralId::BYTE_LEN + payload.len()];
        framed[PeripheralId::BYTE_LEN..].copy_from_slice(payload);
        endpoint.tx().send(Msg::Packet(framed)).unwrap();
    }

    fn receive(endpoint: &Endpoint) -> Vec<u8> {
        let Msg::Packet(bytes) = endpoint.rx().recv_timeout(Duration::from_secs(1)).unwrap() else {
            panic!("expected packet")
        };
        assert_eq!(
            PeripheralId::read_bytes(&bytes[..PeripheralId::BYTE_LEN]),
            TEST_ID
        );
        bytes[PeripheralId::BYTE_LEN..].to_vec()
    }

    #[test]
    fn responder_completes_lifecycle_and_increments_response_ids() {
        let ctx = ControllerCtx::default();
        let endpoint = ctx.source_endpoint("instrument-protocol-test");
        let proxy = Arc::new(TestProxy {
            losses: AtomicUsize::new(0),
        });
        let stop = Arc::new(AtomicBool::new(false));
        let thread = spawn_protocol(
            &ctx,
            "instrument-protocol-test",
            proxy.clone(),
            stop.clone(),
        )
        .unwrap();

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
}
