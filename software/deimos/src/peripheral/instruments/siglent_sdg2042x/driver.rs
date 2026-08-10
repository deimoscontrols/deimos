//! Live SDG2042X state and blocking SCPI worker.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, mpsc};
use std::time::Duration;

use super::super::responder::WorkerStatus;
use super::super::scpi::ScpiClient;
use super::config::{CHANNEL_COUNT, ChannelConfig, Config};
use super::peripheral::{ChannelState, InstrumentState, SiglentSdg2042X};

const SAFE_LOAD: &str = "100000";

/// State shared between the real-time responder and blocking SCPI worker.
///
/// Every valid controller packet replaces `next` without comparison. A safe
/// state requested while returning to Binding takes priority over `next`, and
/// `applied` changes only after every SCPI command for it is transmitted.
struct State {
    // Separate from `next` so safety cannot be overwritten by a command that
    // arrives during a rapid Binding/configuration cycle.
    safe_state_pending: bool,
    // Latest coherent controller state not yet owned by the worker. Replacing
    // this value implements the full-state reassertion contract without a queue.
    next: Option<InstrumentState>,
    // Published only after all SCPI writes for both channels succeed. Physical
    // readback is intentionally limited to startup and shutdown.
    applied: InstrumentState,
    status: WorkerStatus,
}

/// Validated configuration plus synchronized instrument state.
struct Inner {
    config: Config,
    channel_name: String,
    state: Mutex<State>,
    changed: Condvar,
}

/// Owns the live SDG2042X state and blocking SCPI worker.
pub struct SiglentSdg2042XDriver {
    inner: Arc<Inner>,
}

impl SiglentSdg2042XDriver {
    /// Construct a validated SDG2042X driver without connecting it.
    ///
    /// Args:
    ///   config: Connection, waveform, load, timeout, and safety limits.
    ///
    /// Returns:
    ///   A driver ready to be started with [`Self::run`].
    ///
    /// Errors:
    ///   Returns an error when configuration fields or channel ranges are invalid.
    pub fn new(config: Config) -> Result<Self, String> {
        config.validate()?;
        let channel_name = format!(
            "instrument-sdg2042x-{:016x}",
            config.connection.serial_number
        );
        Ok(Self {
            inner: Arc::new(Inner {
                config,
                channel_name,
                state: Mutex::new(State {
                    safe_state_pending: false,
                    next: None,
                    applied: InstrumentState::default(),
                    status: WorkerStatus::default(),
                }),
                changed: Condvar::new(),
            }),
        })
    }

    /// Return the pure peripheral paired with this driver.
    ///
    /// Returns:
    ///   A serializable peripheral carrying the driver's logical identity.
    pub fn peripheral(&self) -> SiglentSdg2042X {
        SiglentSdg2042X::new(self.inner.config.connection.serial_number)
    }

    /// Return the internal thread-channel name expected by the driver.
    ///
    /// Returns:
    ///   A deterministic name derived from the model and logical serial number.
    pub fn channel_name(&self) -> &str {
        &self.inner.channel_name
    }

    /// Return the validated SCPI identity after successful startup.
    ///
    /// Returns:
    ///   The complete `*IDN?` response, or `None` before startup succeeds.
    pub fn identity(&self) -> Option<String> {
        self.inner.state.lock().ok()?.status.identity()
    }

    pub(super) fn shared_handle(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }

    pub(super) fn startup_timeout(&self) -> Duration {
        self.inner.config.connection.startup_timeout()
    }

    pub(super) fn run_worker(
        self,
        stop: Arc<AtomicBool>,
        startup: mpsc::SyncSender<Result<(), String>>,
    ) -> Result<(), String> {
        siglent_worker(self, stop, startup)
    }

    /// Replace the queued command with one normalized complete state.
    pub(super) fn submit(&self, request: InstrumentState) {
        let request = request.normalized(&self.inner.config.channels);
        let mut state = self.inner.state.lock().unwrap();
        state.next = Some(request);
        self.inner.changed.notify_one();
    }

    /// Queue the disabled safe state without waiting for the SCPI worker.
    pub(super) fn request_safe_state(&self) {
        let mut state = self.inner.state.lock().unwrap();
        state.safe_state_pending = true;
        // A command from before contact was lost is stale. A command received
        // after this point may populate `next`, but the safety latch stays set.
        state.next = None;
        self.inner.changed.notify_one();
    }

    /// Return the last completely applied state or a latched worker error.
    pub(super) fn applied(&self) -> Result<InstrumentState, String> {
        let state = self
            .inner
            .state
            .lock()
            .map_err(|_| "SDG2042X state poisoned")?;
        if let Some(error) = state.status.error() {
            Err(error)
        } else {
            Ok(state.applied)
        }
    }

    /// Return a latched worker error, if present.
    pub(super) fn latched_error(&self) -> Option<String> {
        self.inner.state.lock().ok()?.status.error()
    }

    #[cfg(test)]
    pub(super) fn take_queued(&self) -> Option<InstrumentState> {
        take_pending(&mut self.inner.state.lock().unwrap())
    }

    #[cfg(test)]
    pub(super) fn queued(&self) -> Option<InstrumentState> {
        self.inner.state.lock().unwrap().next
    }
}

fn take_pending(state: &mut State) -> Option<InstrumentState> {
    // Consume the safety latch first while preserving any newer command in
    // `next`; the worker will pick that command up on its following iteration.
    if state.safe_state_pending {
        state.safe_state_pending = false;
        Some(InstrumentState::default())
    } else {
        state.next.take()
    }
}

fn siglent_worker(
    driver: SiglentSdg2042XDriver,
    stop: Arc<AtomicBool>,
    startup: mpsc::SyncSender<Result<(), String>>,
) -> Result<(), String> {
    // Keep the first failure visible to the protocol responder. Once latched,
    // it stops emitting apparently healthy responses from stale applied data.
    let result = siglent_worker_inner(&driver, &stop, &startup);
    if let Err(error) = &result
        && let Ok(mut state) = driver.inner.state.lock()
    {
        state.status.latch_error(format!("SDG2042X: {error}"));
    }
    result
}

/// Own the SCPI connection and apply complete two-channel states.
///
/// The worker reports startup only after identity validation and physical safe
/// state verification. Every exit after startup attempts the same safe state.
fn siglent_worker_inner(
    driver: &SiglentSdg2042XDriver,
    stop: &Arc<AtomicBool>,
    startup: &mpsc::SyncSender<Result<(), String>>,
) -> Result<(), String> {
    let config = &driver.inner.config;
    let mut client = match ScpiClient::connect(&config.connection) {
        Ok(client) => client,
        Err(err) => {
            let _ = startup.send(Err(format!("SDG2042X connection failed: {err}")));
            return Err(err);
        }
    };

    let setup = setup_siglent(&mut client, config);
    let identity = match setup {
        Ok(identity) => identity,
        Err(err) => {
            let _ = startup.send(Err(format!("SDG2042X setup failed: {err}")));
            return Err(err);
        }
    };
    driver
        .inner
        .state
        .lock()
        .unwrap()
        .status
        .set_identity(identity);
    let _ = startup.send(Ok(()));

    let run_result = loop {
        let mut state = driver.inner.state.lock().unwrap();
        while !state.safe_state_pending && state.next.is_none() && !stop.load(Ordering::Relaxed) {
            state = driver
                .inner
                .changed
                .wait_timeout(state, Duration::from_millis(20))
                .unwrap()
                .0;
        }
        if stop.load(Ordering::Relaxed) {
            break Ok(());
        }
        // Removing the request while holding the mutex makes it the worker's
        // in-flight state. The responder can now replace `next` independently.
        let request = take_pending(&mut state).unwrap();
        drop(state);

        if let Err(err) = apply_request(&mut client, config, request) {
            break Err(format!("failed to apply commanded state: {err}"));
        }
        // Never report a partially transmitted two-channel request as applied.
        driver.inner.state.lock().unwrap().applied = request;
    };

    let shutdown_result = safe_outputs(&mut client);
    combine_worker_results(run_result, shutdown_result)
}

/// Preserve an operating failure without concealing a later safing failure.
pub(super) fn combine_worker_results(
    run_result: Result<(), String>,
    shutdown_result: Result<(), String>,
) -> Result<(), String> {
    match (run_result, shutdown_result) {
        (Err(err), Err(shutdown_err)) => Err(format!(
            "{err}; additionally failed to apply safe state during shutdown: {shutdown_err}"
        )),
        (Err(err), Ok(())) => Err(err),
        (Ok(()), Err(err)) => Err(format!("failed to apply safe state during shutdown: {err}")),
        (Ok(()), Ok(())) => Ok(()),
    }
}

/// Verify the model and establish a read-back-verified safe baseline.
fn setup_siglent(client: &mut ScpiClient, config: &Config) -> Result<String, String> {
    let identity = client.identify()?;
    config.connection.validate_identity(&identity)?;
    safe_outputs(client)?;
    for (index, channel) in config.channels.iter().enumerate() {
        let number = index + 1;
        let load = channel.load.scpi();
        client.command(&format!("C{number}:OUTP LOAD,{load}"))?;
        verify_output_state(client, number, false, &load)?;
        verify_safe_waveform(client, number)?;
    }
    Ok(identity)
}

/// Drive both channels to 0 V DC, then open both output relays.
///
/// Waveform commands precede relay commands so a slow physical relay cannot
/// expose the previous waveform during shutdown. All steps are best-effort;
/// errors are accumulated so failure on one channel does not skip the other.
fn safe_outputs(client: &mut ScpiClient) -> Result<(), String> {
    let mut errors = Vec::new();
    for number in 1..=CHANNEL_COUNT {
        if let Err(err) = client.command(&safe_waveform_command(number)) {
            errors.push(format!("channel {number} zero command: {err}"));
        }
    }
    if let Err(err) = expect_operation_complete(client) {
        errors.push(format!("zero completion: {err}"));
    }
    for number in 1..=CHANNEL_COUNT {
        if let Err(err) = verify_safe_waveform(client, number) {
            errors.push(format!("channel {number} zero readback: {err}"));
        }
    }
    for number in 1..=CHANNEL_COUNT {
        if let Err(err) = client.command(&format!("C{number}:OUTP OFF,LOAD,{SAFE_LOAD}")) {
            errors.push(format!("channel {number} output-off command: {err}"));
        }
    }
    if let Err(err) = expect_operation_complete(client) {
        errors.push(format!("output-off completion: {err}"));
    }
    for number in 1..=CHANNEL_COUNT {
        if let Err(err) = verify_output_state(client, number, false, SAFE_LOAD) {
            errors.push(format!("channel {number} output-off readback: {err}"));
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("; "))
    }
}

/// Transmit the safe state without adding readback traffic to Operating.
fn command_safe_channel(client: &mut ScpiClient, number: usize) -> Result<(), String> {
    // The SDG processes commands in order, so the waveform becomes 0 V DC
    // before the following command opens the physical output relay.
    client.command(&safe_waveform_command(number))?;
    client.command(&format!("C{number}:OUTP OFF,LOAD,{SAFE_LOAD}"))
}

fn safe_waveform_command(channel_number: usize) -> String {
    format!("C{channel_number}:BSWV WVTP,DC,OFST,0")
}

/// Confirm that a channel reports a zero-offset DC basic waveform.
fn verify_safe_waveform(client: &mut ScpiClient, channel_number: usize) -> Result<(), String> {
    let readback = client.query(&format!("C{channel_number}:BSWV?"))?;
    let waveform = parameter_value(&readback, "WVTP");
    let offset = parameter_value(&readback, "OFST").and_then(|value| {
        value
            .trim_end_matches(|c: char| c.is_ascii_alphabetic())
            .parse()
            .ok()
    });
    if waveform.is_some_and(|value| value.eq_ignore_ascii_case("DC")) && offset == Some(0.0) {
        Ok(())
    } else {
        Err(format!(
            "channel {channel_number} waveform readback `{readback}` was not 0 V DC"
        ))
    }
}

/// Confirm both relay state and load compensation from `OUTP?` readback.
fn verify_output_state(
    client: &mut ScpiClient,
    channel_number: usize,
    enabled: bool,
    load: &str,
) -> Result<(), String> {
    let readback = client.query(&format!("C{channel_number}:OUTP?"))?;
    let state = if enabled { "ON" } else { "OFF" };
    let expected = format!("C{channel_number}:OUTP {state},LOAD,{load}");
    if readback.to_ascii_uppercase().starts_with(&expected) {
        Ok(())
    } else {
        Err(format!(
            "channel {channel_number} output readback `{readback}` did not start with `{expected}`"
        ))
    }
}

/// Find the value following a named comma-delimited SCPI response field.
fn parameter_value<'a>(response: &'a str, name: &str) -> Option<&'a str> {
    let mut tokens = response.split(',').map(str::trim);
    while let Some(token) = tokens.next() {
        if token
            .split_ascii_whitespace()
            .next_back()
            .is_some_and(|token| token.eq_ignore_ascii_case(name))
        {
            return tokens.next();
        }
    }
    None
}

/// Reassert all channel values and output-relay states.
///
/// Operating deliberately performs no readback queries. A channel is
/// configured before its relay closes, and disabling transmits 0 V DC before
/// opening its relay.
fn apply_request(
    client: &mut ScpiClient,
    config: &Config,
    request: InstrumentState,
) -> Result<(), String> {
    // Always reassert the complete state. Besides keeping the protocol simple,
    // this preserves the desired behavior for transports where commands may be
    // dropped and periodic state reassertion is useful.
    for (index, desired) in request.channels().into_iter().enumerate() {
        let number = index + 1;
        if desired.enabled == 0.0 {
            command_safe_channel(client, number)?;
            continue;
        }

        let channel = &config.channels[index];
        let command = basic_wave_command(number, channel, desired);
        client.command(&command)?;
        let load = channel.load.scpi();
        client.command(&format!("C{number}:OUTP ON,LOAD,{load}"))?;
    }
    Ok(())
}

/// Render the subset of `BSWV` fields applicable to the configured waveform.
pub(super) fn basic_wave_command(
    channel_number: usize,
    config: &ChannelConfig,
    request: ChannelState,
) -> String {
    let waveform = config.waveform;
    let mut command = format!("C{channel_number}:BSWV WVTP,{}", waveform.scpi());
    if waveform.uses_frequency() {
        command.push_str(&format!(",FRQ,{}", scpi_number(request.frequency_hz)));
    }
    if waveform.uses_amplitude() {
        command.push_str(&format!(",AMP,{}", scpi_number(config.amplitude_vpp)));
    }
    if waveform.uses_offset() {
        command.push_str(&format!(",OFST,{}", scpi_number(request.offset_voltage_v)));
    } else {
        command.push_str(&format!(",MEAN,{}", scpi_number(request.offset_voltage_v)));
        command.push_str(&format!(",STDEV,{}", scpi_number(request.stdev)));
    }
    if waveform.uses_duty() {
        command.push_str(&format!(
            ",DUTY,{}",
            scpi_number(request.pulse_duty_cycle * 100.0)
        ));
    }
    if waveform.uses_phase() {
        command.push_str(&format!(",PHSE,{}", scpi_number(request.phase_deg)));
    }
    command
}

/// Wait for previously issued operations using the standard `*OPC?` query.
fn expect_operation_complete(client: &mut ScpiClient) -> Result<(), String> {
    let response = client.query("*OPC?")?;
    if response.trim() == "1" {
        Ok(())
    } else {
        Err(format!("unexpected *OPC? response `{response}`"))
    }
}

pub(super) fn scpi_number(value: f64) -> String {
    format!("{value:.17e}")
}
