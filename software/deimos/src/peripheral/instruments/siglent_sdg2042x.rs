use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use deimos_shared::peripherals::PeripheralId;
use deimos_shared::states::{ByteStruct, ByteStructLen, OperatingMetrics};

use super::protocol::{InstrumentProxy, InstrumentRunHandle, spawn_protocol};
use super::scpi::{ScpiClient, address_with_default_port};
use super::{SOFTWARE_MODEL_NUMBER_BASE, serial_from_address};
use crate::calc::Calc;
use crate::controller::Controller;
use crate::controller::context::ControllerCtx;
use crate::peripheral::Peripheral;
use crate::socket::thread_channel::ThreadChannelSocket;

const CHANNEL_COUNT: usize = 2;
const VALUES_PER_CHANNEL: usize = 6;
const INPUT_COUNT: usize = CHANNEL_COUNT * VALUES_PER_CHANNEL;
const OUTPUT_COUNT: usize = INPUT_COUNT + 2;
const INPUT_SIZE: usize = 8 + INPUT_COUNT * 8;
const OUTPUT_SIZE: usize = OperatingMetrics::BYTE_LEN + OUTPUT_COUNT * 8;
const MAX_EXACT_F64_INTEGER: u64 = (1_u64 << 53) - 1;
const SAFE_LOAD: &str = "100000";

/// Software model number for the Siglent SDG2042X integration.
pub const MODEL_NUMBER: u64 = SOFTWARE_MODEL_NUMBER_BASE + 1;

/// Supported SDG2042X basic waveforms.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum SiglentWaveform {
    Sine,
    Square,
    Ramp,
    Pulse,
    Noise,
    Dc,
}

impl SiglentWaveform {
    fn scpi(self) -> &'static str {
        match self {
            Self::Sine => "SINE",
            Self::Square => "SQUARE",
            Self::Ramp => "RAMP",
            Self::Pulse => "PULSE",
            Self::Noise => "NOISE",
            Self::Dc => "DC",
        }
    }

    fn uses_frequency(self) -> bool {
        !matches!(self, Self::Noise | Self::Dc)
    }

    fn uses_amplitude(self) -> bool {
        !matches!(self, Self::Noise | Self::Dc)
    }

    fn uses_offset(self) -> bool {
        !matches!(self, Self::Noise)
    }

    fn uses_duty(self) -> bool {
        matches!(self, Self::Square | Self::Pulse)
    }

    fn uses_phase(self) -> bool {
        matches!(self, Self::Sine | Self::Square | Self::Ramp)
    }
}

/// Load presented to an SDG2042X output channel.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum SiglentLoad {
    HighImpedance,
    Ohms(u32),
}

impl SiglentLoad {
    fn validate(self) -> Result<(), String> {
        match self {
            Self::HighImpedance => Ok(()),
            Self::Ohms(ohms) if (50..=100_000).contains(&ohms) => Ok(()),
            Self::Ohms(ohms) => Err(format!("load {ohms} ohms is outside 50..=100000")),
        }
    }

    fn scpi(self) -> String {
        match self {
            // SDG2042X firmware 2.01.01.37R2 silently retains 50-ohm
            // compensation for LOAD,HZ. Its maximum numeric load applies and
            // reads back reliably, while differing from an open load by only
            // the generator's 50-ohm source impedance.
            Self::HighImpedance => "100000".to_owned(),
            Self::Ohms(ohms) => ohms.to_string(),
        }
    }
}

/// Fixed channel settings and accepted dynamic-command ranges.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct SiglentChannelConfig {
    pub waveform: SiglentWaveform,
    pub amplitude_vpp: f64,
    pub load: SiglentLoad,
    pub frequency_hz: (f64, f64),
    pub offset_voltage_v: (f64, f64),
    pub pulse_duty_cycle: (f64, f64),
    pub phase_deg: (f64, f64),
    pub stdev: (f64, f64),
}

impl Default for SiglentChannelConfig {
    fn default() -> Self {
        Self {
            waveform: SiglentWaveform::Dc,
            amplitude_vpp: 1.0,
            load: SiglentLoad::HighImpedance,
            frequency_hz: (1.0e-6, 40.0e6),
            offset_voltage_v: (-10.0, 10.0),
            pulse_duty_cycle: (0.0, 1.0),
            phase_deg: (0.0, 360.0),
            stdev: (0.0, 10.0),
        }
    }
}

impl SiglentChannelConfig {
    fn validate(&self, channel: usize) -> Result<(), String> {
        self.load
            .validate()
            .map_err(|err| format!("channel {channel}: {err}"))?;
        if self.waveform.uses_amplitude()
            && (!self.amplitude_vpp.is_finite() || self.amplitude_vpp <= 0.0)
        {
            return Err(format!(
                "channel {channel}: amplitude_vpp must be finite and positive"
            ));
        }
        validate_range("frequency_hz", self.frequency_hz, channel)?;
        validate_range("offset_voltage_v", self.offset_voltage_v, channel)?;
        validate_range("pulse_duty_cycle", self.pulse_duty_cycle, channel)?;
        validate_range("phase_deg", self.phase_deg, channel)?;
        validate_range("stdev", self.stdev, channel)?;
        if self.pulse_duty_cycle.0 < 0.0 || self.pulse_duty_cycle.1 > 1.0 {
            return Err(format!(
                "channel {channel}: pulse_duty_cycle range must be within 0..=1"
            ));
        }
        Ok(())
    }
}

fn validate_range(name: &str, range: (f64, f64), channel: usize) -> Result<(), String> {
    if range.0.is_finite() && range.1.is_finite() && range.0 <= range.1 {
        Ok(())
    } else {
        Err(format!(
            "channel {channel}: {name} range must contain finite ascending bounds"
        ))
    }
}

/// Connection and safety configuration for an SDG2042X driver.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct SiglentSdg2042XConfig {
    pub address: String,
    pub channel_name: String,
    pub serial_number: u64,
    pub expected_model: String,
    pub connect_timeout: Duration,
    pub read_timeout: Duration,
    pub write_timeout: Duration,
    pub channels: [SiglentChannelConfig; CHANNEL_COUNT],
}

impl SiglentSdg2042XConfig {
    /// Build a configuration from a host name or address, adding SCPI port 5025.
    pub fn new(
        host: impl Into<String>,
        channel_name: impl Into<String>,
        serial_number: u64,
    ) -> Self {
        let address = address_with_default_port(host.into());
        Self {
            address,
            channel_name: channel_name.into(),
            serial_number,
            expected_model: "SDG2042X".to_owned(),
            connect_timeout: Duration::from_secs(2),
            read_timeout: Duration::from_secs(2),
            write_timeout: Duration::from_secs(2),
            channels: std::array::from_fn(|_| SiglentChannelConfig::default()),
        }
    }

    fn validate(&self) -> Result<(), String> {
        if self.address.trim().is_empty() || self.channel_name.trim().is_empty() {
            return Err("address and channel_name cannot be empty".to_owned());
        }
        if self.expected_model.trim().is_empty() {
            return Err("expected_model cannot be empty".to_owned());
        }
        for (index, channel) in self.channels.iter().enumerate() {
            channel.validate(index + 1)?;
        }
        Ok(())
    }
}

/// Pure controller-side representation of an SDG2042X.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SiglentSdg2042X {
    pub serial_number: u64,
}

impl SiglentSdg2042X {
    pub fn new(serial_number: u64) -> Self {
        Self { serial_number }
    }
}

#[typetag::serde]
impl Peripheral for SiglentSdg2042X {
    fn id(&self) -> PeripheralId {
        PeripheralId {
            model_number: MODEL_NUMBER,
            serial_number: self.serial_number,
        }
    }

    fn input_names(&self) -> Vec<String> {
        channel_names("")
    }

    fn output_names(&self) -> Vec<String> {
        let mut names = channel_names("applied_");
        names.extend(["command_sequence".to_owned(), "command_age_s".to_owned()]);
        names
    }

    fn operating_roundtrip_input_size(&self) -> usize {
        INPUT_SIZE
    }

    fn operating_roundtrip_output_size(&self) -> usize {
        OUTPUT_SIZE
    }

    fn emit_operating_roundtrip(
        &self,
        id: u64,
        _period_delta_ns: i64,
        _phase_delta_ns: i64,
        inputs: &[f64],
        bytes: &mut [u8],
    ) {
        write_u64(bytes, 0, id);
        for (index, value) in inputs.iter().copied().take(INPUT_COUNT).enumerate() {
            write_f64(bytes, 8 + index * 8, value);
        }
    }

    fn parse_operating_roundtrip(&self, bytes: &[u8], outputs: &mut [f64]) -> OperatingMetrics {
        let metrics = OperatingMetrics::read_bytes(&bytes[..OperatingMetrics::BYTE_LEN]);
        for (index, output) in outputs.iter_mut().take(OUTPUT_COUNT).enumerate() {
            *output = read_f64(bytes, OperatingMetrics::BYTE_LEN + index * 8);
        }
        metrics
    }

    fn validate_operating_roundtrip(&self, bytes: &[u8]) -> bool {
        bytes.len() == OUTPUT_SIZE
            && (0..OUTPUT_COUNT)
                .all(|index| read_f64(bytes, OperatingMetrics::BYTE_LEN + index * 8).is_finite())
    }

    fn standard_calcs(&self, _name: &str) -> BTreeMap<String, Box<dyn Calc>> {
        BTreeMap::new()
    }
}

fn channel_names(infix: &str) -> Vec<String> {
    let mut names = Vec::with_capacity(INPUT_COUNT);
    for channel in 1..=CHANNEL_COUNT {
        for field in [
            "enabled",
            "frequency_hz",
            "offset_voltage_v",
            "pulse_duty_cycle",
            "phase_deg",
            "stdev",
        ] {
            names.push(format!("ch{channel}_{infix}{field}"));
        }
    }
    names
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct ChannelRequest {
    enabled: bool,
    frequency_hz: f64,
    offset_voltage_v: f64,
    pulse_duty_cycle: f64,
    phase_deg: f64,
    stdev: f64,
}

impl ChannelRequest {
    fn values(self) -> [f64; VALUES_PER_CHANNEL] {
        [
            f64::from(self.enabled),
            self.frequency_hz,
            self.offset_voltage_v,
            self.pulse_duty_cycle,
            self.phase_deg,
            self.stdev,
        ]
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Request {
    channels: [ChannelRequest; CHANNEL_COUNT],
}

struct State {
    pending: Option<(u64, Request)>,
    next_generation: u64,
    applied: Request,
    command_sequence: u64,
    completed_at: Instant,
    identity: Option<String>,
    error: Option<String>,
}

struct Shared {
    config: SiglentSdg2042XConfig,
    state: Mutex<State>,
    changed: Condvar,
}

impl Shared {
    fn new(config: SiglentSdg2042XConfig) -> Self {
        Self {
            config,
            state: Mutex::new(State {
                pending: None,
                next_generation: 1,
                applied: Request::default(),
                command_sequence: 0,
                completed_at: Instant::now(),
                identity: None,
                error: None,
            }),
            changed: Condvar::new(),
        }
    }

    fn reject_request(&self, message: String) {
        let mut state = self.state.lock().unwrap();
        let latest = state.pending.map_or(state.applied, |(_, request)| request);
        if latest.channels.iter().any(|channel| channel.enabled) {
            let mut disabled = latest;
            for channel in &mut disabled.channels {
                channel.enabled = false;
            }
            let generation = state.next_generation;
            state.next_generation = state.next_generation.wrapping_add(1).max(1);
            state.pending = Some((generation, disabled));
            self.changed.notify_one();
        }
        if state.error.is_none() {
            state.error = Some(message);
        }
    }
}

impl InstrumentProxy for Shared {
    fn id(&self) -> PeripheralId {
        SiglentSdg2042X::new(self.config.serial_number).id()
    }

    fn input_size(&self) -> usize {
        INPUT_SIZE
    }

    fn output_size(&self) -> usize {
        OUTPUT_SIZE
    }

    fn process_request(&self, bytes: &[u8]) -> Result<u64, String> {
        let id = read_u64(bytes, 0);
        let request = match parse_request(bytes) {
            Ok(request) => request,
            Err(err) => {
                self.reject_request(format!("SDG2042X rejected operating request: {err}"));
                return Err(err);
            }
        };
        if let Err(err) = validate_request(request, &self.config.channels) {
            let message = format!("SDG2042X rejected operating request: {err}");
            self.reject_request(message.clone());
            return Err(message);
        }

        let mut state = self.state.lock().unwrap();
        let latest = state.pending.map_or(state.applied, |(_, request)| request);
        if request != latest {
            let generation = state.next_generation;
            state.next_generation = state.next_generation.wrapping_add(1).max(1);
            state.pending = Some((generation, request));
            self.changed.notify_one();
        }
        Ok(id)
    }

    fn write_response(&self, metrics: OperatingMetrics, bytes: &mut [u8]) -> Result<(), String> {
        let state = self.state.lock().map_err(|_| "SDG2042X state poisoned")?;
        if let Some(error) = &state.error {
            return Err(error.clone());
        }
        metrics.write_bytes(&mut bytes[..OperatingMetrics::BYTE_LEN]);
        let mut index = 0;
        for channel in state.applied.channels {
            for value in channel.values() {
                write_f64(bytes, OperatingMetrics::BYTE_LEN + index * 8, value);
                index += 1;
            }
        }
        write_f64(
            bytes,
            OperatingMetrics::BYTE_LEN + index * 8,
            state.command_sequence.min(MAX_EXACT_F64_INTEGER) as f64,
        );
        write_f64(
            bytes,
            OperatingMetrics::BYTE_LEN + (index + 1) * 8,
            state.completed_at.elapsed().as_secs_f64(),
        );
        Ok(())
    }

    fn on_loss_of_contact(&self) {
        let mut state = self.state.lock().unwrap();
        let latest = state.pending.map_or(state.applied, |(_, request)| request);
        if latest.channels.iter().any(|channel| channel.enabled) {
            let mut disabled = latest;
            for channel in &mut disabled.channels {
                channel.enabled = false;
            }
            let generation = state.next_generation;
            state.next_generation = state.next_generation.wrapping_add(1).max(1);
            state.pending = Some((generation, disabled));
            self.changed.notify_one();
        }
    }

    fn error(&self) -> Option<String> {
        self.state.lock().ok()?.error.clone()
    }
}

/// Owns the live SDG2042X connection and software-peripheral responder.
pub struct SiglentSdg2042XDriver {
    shared: Arc<Shared>,
}

impl SiglentSdg2042XDriver {
    pub fn new(config: SiglentSdg2042XConfig) -> Result<Self, String> {
        config.validate()?;
        Ok(Self {
            shared: Arc::new(Shared::new(config)),
        })
    }

    pub fn peripheral(&self) -> SiglentSdg2042X {
        SiglentSdg2042X::new(self.shared.config.serial_number)
    }

    pub fn channel_name(&self) -> &str {
        &self.shared.config.channel_name
    }

    pub fn identity(&self) -> Option<String> {
        self.shared.state.lock().ok()?.identity.clone()
    }

    pub fn run(&self, ctx: &ControllerCtx) -> Result<InstrumentRunHandle, String> {
        let stop = Arc::new(AtomicBool::new(false));
        let (startup_tx, startup_rx) = mpsc::sync_channel(1);
        let worker_shared = self.shared.clone();
        let worker_stop = stop.clone();
        let worker = thread::Builder::new()
            .name(format!("sdg2042x-{}", self.shared.config.serial_number))
            .spawn(move || siglent_worker(worker_shared, worker_stop, startup_tx))
            .map_err(|err| format!("failed to spawn SDG2042X worker: {err}"))?;

        let startup_wait = self.shared.config.connect_timeout
            + self.shared.config.read_timeout
            + self.shared.config.write_timeout
            + Duration::from_secs(1);
        match startup_rx.recv_timeout(startup_wait) {
            Ok(Ok(())) => {}
            Ok(Err(err)) => {
                let _ = worker.join();
                return Err(err);
            }
            Err(err) => {
                stop.store(true, Ordering::Relaxed);
                let _ = worker.join();
                return Err(format!("SDG2042X startup did not complete: {err}"));
            }
        }

        let protocol = match spawn_protocol(
            ctx,
            &self.shared.config.channel_name,
            self.shared.clone(),
            stop.clone(),
        ) {
            Ok(protocol) => protocol,
            Err(err) => {
                stop.store(true, Ordering::Relaxed);
                self.shared.changed.notify_all();
                let _ = worker.join();
                return Err(err);
            }
        };
        Ok(InstrumentRunHandle::new(stop, protocol, worker))
    }
}

/// Attach one SDG2042X using the default two-channel configuration.
///
/// This connects and validates the instrument before registering its software
/// peripheral and generated thread-channel socket with `controller`. Construct
/// [`SiglentSdg2042XConfig`] and [`SiglentSdg2042XDriver`] directly when channel
/// waveforms, limits, loads, or timeouts need customization.
pub fn attach(
    peripheral_name: &str,
    address: impl Into<String>,
    controller: &mut Controller,
) -> Result<InstrumentRunHandle, String> {
    if controller.peripherals().contains_key(peripheral_name) {
        return Err(format!("Peripheral name `{peripheral_name}` is duplicated"));
    }
    let address = address_with_default_port(address.into());
    let serial_number = serial_from_address(&address);
    let channel_name = format!("instrument-sdg2042x-{peripheral_name}-{serial_number:016x}");
    let driver = SiglentSdg2042XDriver::new(SiglentSdg2042XConfig::new(
        address,
        channel_name.clone(),
        serial_number,
    ))?;
    let mut handle = driver.run(&controller.ctx)?;
    if let Err(err) = controller.add_peripheral(peripheral_name, Box::new(driver.peripheral())) {
        return match handle.join() {
            Ok(()) => Err(err),
            Err(cleanup) => Err(format!(
                "{err}; additionally failed to stop SDG2042X: {cleanup}"
            )),
        };
    }
    controller.add_socket(
        &channel_name,
        Box::new(ThreadChannelSocket::new(&channel_name)),
    );
    Ok(handle)
}

fn siglent_worker(
    shared: Arc<Shared>,
    stop: Arc<AtomicBool>,
    startup: mpsc::SyncSender<Result<(), String>>,
) -> Result<(), String> {
    let result = siglent_worker_inner(&shared, &stop, &startup);
    if let Err(error) = &result
        && let Ok(mut state) = shared.state.lock()
        && state.error.is_none()
    {
        state.error = Some(format!("SDG2042X: {error}"));
    }
    result
}

fn siglent_worker_inner(
    shared: &Arc<Shared>,
    stop: &Arc<AtomicBool>,
    startup: &mpsc::SyncSender<Result<(), String>>,
) -> Result<(), String> {
    let config = &shared.config;
    let mut client = match ScpiClient::connect(
        &config.address,
        config.connect_timeout,
        config.read_timeout,
        config.write_timeout,
    ) {
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
    shared.state.lock().unwrap().identity = Some(identity);
    let _ = startup.send(Ok(()));

    let run_result = loop {
        let mut state = shared.state.lock().unwrap();
        while state.pending.is_none() && !stop.load(Ordering::Relaxed) {
            state = shared
                .changed
                .wait_timeout(state, Duration::from_millis(20))
                .unwrap()
                .0;
        }
        if stop.load(Ordering::Relaxed) {
            break Ok(());
        }
        let (generation, request) = state.pending.take().unwrap();
        let previous = state.applied;
        drop(state);

        if let Err(err) = apply_request(&mut client, config, previous, request) {
            break Err(format!("failed to apply generation {generation}: {err}"));
        }
        let mut state = shared.state.lock().unwrap();
        state.applied = request;
        state.command_sequence = generation;
        state.completed_at = Instant::now();
    };

    let shutdown_result = safe_outputs(&mut client);
    match (run_result, shutdown_result) {
        (Err(err), _) => Err(err),
        (Ok(()), Err(err)) => Err(format!("failed to apply safe state during shutdown: {err}")),
        (Ok(()), Ok(())) => Ok(()),
    }
}

fn setup_siglent(
    client: &mut ScpiClient,
    config: &SiglentSdg2042XConfig,
) -> Result<String, String> {
    let identity = client.identify()?;
    let uppercase = identity.to_ascii_uppercase();
    if !uppercase.contains("SIGLENT")
        || !uppercase.contains(&config.expected_model.to_ascii_uppercase())
    {
        return Err(format!(
            "identity `{identity}` did not match SIGLENT {}",
            config.expected_model
        ));
    }
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

fn safe_channel(client: &mut ScpiClient, number: usize) -> Result<(), String> {
    client.command(&safe_waveform_command(number))?;
    expect_operation_complete(client)?;
    client.command(&format!("C{number}:OUTP OFF,LOAD,{SAFE_LOAD}"))?;
    expect_operation_complete(client)?;
    verify_output_state(client, number, false, SAFE_LOAD)
}

fn safe_waveform_command(channel_number: usize) -> String {
    format!("C{channel_number}:BSWV WVTP,DC,OFST,0")
}

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

fn apply_request(
    client: &mut ScpiClient,
    config: &SiglentSdg2042XConfig,
    previous: Request,
    request: Request,
) -> Result<(), String> {
    for index in 0..CHANNEL_COUNT {
        let number = index + 1;
        let desired = request.channels[index];
        let was = previous.channels[index];
        if !desired.enabled {
            if was.enabled {
                safe_channel(client, number)?;
            }
            continue;
        }

        let channel = &config.channels[index];
        let command = basic_wave_command(number, channel, desired);
        client.command(&command)?;
        expect_operation_complete(client)?;
        if !was.enabled {
            let load = channel.load.scpi();
            client.command(&format!("C{number}:OUTP ON,LOAD,{load}"))?;
            verify_output_state(client, number, true, &load)?;
        }
    }
    Ok(())
}

fn basic_wave_command(
    channel_number: usize,
    config: &SiglentChannelConfig,
    request: ChannelRequest,
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

fn expect_operation_complete(client: &mut ScpiClient) -> Result<(), String> {
    let response = client.query("*OPC?")?;
    if response.trim() == "1" {
        Ok(())
    } else {
        Err(format!("unexpected *OPC? response `{response}`"))
    }
}

fn parse_request(bytes: &[u8]) -> Result<Request, String> {
    if bytes.len() != INPUT_SIZE {
        return Err(format!(
            "expected {INPUT_SIZE} request bytes, got {}",
            bytes.len()
        ));
    }
    let mut request = Request::default();
    for (index, channel) in request.channels.iter_mut().enumerate() {
        let start = 8 + index * VALUES_PER_CHANNEL * 8;
        let enabled = read_f64(bytes, start);
        if enabled != 0.0 && enabled != 1.0 {
            return Err(format!(
                "channel {} enabled must be exactly 0 or 1",
                index + 1
            ));
        }
        *channel = ChannelRequest {
            enabled: enabled == 1.0,
            frequency_hz: read_f64(bytes, start + 8),
            offset_voltage_v: read_f64(bytes, start + 16),
            pulse_duty_cycle: read_f64(bytes, start + 24),
            phase_deg: read_f64(bytes, start + 32),
            stdev: read_f64(bytes, start + 40),
        };
    }
    Ok(request)
}

fn validate_request(
    request: Request,
    configs: &[SiglentChannelConfig; CHANNEL_COUNT],
) -> Result<(), String> {
    for (index, (request, config)) in request.channels.iter().zip(configs).enumerate() {
        for (name, value) in [
            ("frequency_hz", request.frequency_hz),
            ("offset_voltage_v", request.offset_voltage_v),
            ("pulse_duty_cycle", request.pulse_duty_cycle),
            ("phase_deg", request.phase_deg),
            ("stdev", request.stdev),
        ] {
            if !value.is_finite() {
                return Err(format!("channel {} {name} must be finite", index + 1));
            }
        }
        if !request.enabled {
            continue;
        }
        let waveform = config.waveform;
        if waveform.uses_frequency() {
            in_range(
                "frequency_hz",
                request.frequency_hz,
                config.frequency_hz,
                index + 1,
            )?;
        }
        if waveform.uses_offset() || waveform == SiglentWaveform::Noise {
            in_range(
                "offset_voltage_v",
                request.offset_voltage_v,
                config.offset_voltage_v,
                index + 1,
            )?;
        }
        if waveform.uses_duty() {
            in_range(
                "pulse_duty_cycle",
                request.pulse_duty_cycle,
                config.pulse_duty_cycle,
                index + 1,
            )?;
        }
        if waveform.uses_phase() {
            in_range("phase_deg", request.phase_deg, config.phase_deg, index + 1)?;
        }
        if waveform == SiglentWaveform::Noise {
            in_range("stdev", request.stdev, config.stdev, index + 1)?;
        }
    }
    Ok(())
}

fn in_range(name: &str, value: f64, range: (f64, f64), channel: usize) -> Result<(), String> {
    if (range.0..=range.1).contains(&value) {
        Ok(())
    } else {
        Err(format!(
            "channel {channel} {name} {value} is outside {}..={}",
            range.0, range.1
        ))
    }
}

fn scpi_number(value: f64) -> String {
    format!("{value:.17e}")
}

fn read_u64(bytes: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap())
}

fn write_u64(bytes: &mut [u8], offset: usize, value: u64) {
    bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}

fn read_f64(bytes: &[u8], offset: usize) -> f64 {
    f64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap())
}

fn write_f64(bytes: &mut [u8], offset: usize, value: f64) {
    bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;

    #[test]
    fn packet_shape_and_names_are_stable() {
        let peripheral = SiglentSdg2042X::new(7);
        assert_eq!(peripheral.input_names().len(), INPUT_COUNT);
        assert_eq!(peripheral.output_names().len(), OUTPUT_COUNT);
        assert_eq!(peripheral.operating_roundtrip_input_size(), INPUT_SIZE);
        assert_eq!(peripheral.operating_roundtrip_output_size(), OUTPUT_SIZE);
        assert_eq!(peripheral.input_names()[6], "ch2_enabled");
        assert_eq!(peripheral.output_names()[6], "ch2_applied_enabled");
    }

    #[test]
    fn active_parameters_are_validated_by_waveform() {
        let mut configs = std::array::from_fn(|_| SiglentChannelConfig::default());
        configs[0].waveform = SiglentWaveform::Sine;
        let mut request = Request::default();
        request.channels[0] = ChannelRequest {
            enabled: true,
            frequency_hz: 1_000.0,
            offset_voltage_v: 0.0,
            pulse_duty_cycle: 99.0,
            phase_deg: 10.0,
            stdev: 99.0,
        };
        assert!(validate_request(request, &configs).is_ok());
        request.channels[0].frequency_hz = 50.0e6;
        assert!(validate_request(request, &configs).is_err());
    }

    #[test]
    fn duty_fraction_is_rendered_as_percent() {
        assert_eq!(scpi_number(0.25 * 100.0), "2.50000000000000000e1");
    }

    #[test]
    fn every_waveform_emits_only_its_applicable_fields() {
        let request = ChannelRequest {
            enabled: true,
            frequency_hz: 1_000.0,
            offset_voltage_v: 0.25,
            pulse_duty_cycle: 0.4,
            phase_deg: 30.0,
            stdev: 0.1,
        };
        for waveform in [
            SiglentWaveform::Sine,
            SiglentWaveform::Square,
            SiglentWaveform::Ramp,
            SiglentWaveform::Pulse,
            SiglentWaveform::Noise,
            SiglentWaveform::Dc,
        ] {
            let config = SiglentChannelConfig {
                waveform,
                ..SiglentChannelConfig::default()
            };
            let command = basic_wave_command(1, &config, request);
            assert_eq!(command.contains(",FRQ,"), waveform.uses_frequency());
            assert_eq!(command.contains(",AMP,"), waveform.uses_amplitude());
            assert_eq!(command.contains(",OFST,"), waveform.uses_offset());
            assert_eq!(command.contains(",DUTY,"), waveform.uses_duty());
            assert_eq!(command.contains(",PHSE,"), waveform.uses_phase());
            assert_eq!(
                command.contains(",MEAN,"),
                waveform == SiglentWaveform::Noise
            );
            assert_eq!(
                command.contains(",STDEV,"),
                waveform == SiglentWaveform::Noise
            );
        }
    }

    #[test]
    fn worker_applies_complete_two_channel_generation_and_shuts_down_safe() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut writer = stream;
            let mut commands = Vec::new();
            let mut output_enabled = [false; CHANNEL_COUNT];
            loop {
                let mut command = String::new();
                if reader.read_line(&mut command).unwrap() == 0 {
                    break;
                }
                let command = command.trim_end().to_owned();
                for number in 1..=CHANNEL_COUNT {
                    if command == format!("C{number}:OUTP ON,LOAD,100000") {
                        output_enabled[number - 1] = true;
                    } else if command.starts_with(&format!("C{number}:OUTP OFF")) {
                        output_enabled[number - 1] = false;
                    }
                }
                match command.as_str() {
                    "*IDN?" => writer
                        .write_all(b"Siglent Technologies,SDG2042X,TEST,1.0\n")
                        .unwrap(),
                    "C1:OUTP?" => writeln!(
                        writer,
                        "C1:OUTP {},LOAD,100000,PLRT,NOR",
                        if output_enabled[0] { "ON" } else { "OFF" }
                    )
                    .unwrap(),
                    "C2:OUTP?" => writeln!(
                        writer,
                        "C2:OUTP {},LOAD,100000,PLRT,NOR",
                        if output_enabled[1] { "ON" } else { "OFF" }
                    )
                    .unwrap(),
                    "C1:BSWV?" => writer.write_all(b"C1:BSWV WVTP,DC,OFST,0V\n").unwrap(),
                    "C2:BSWV?" => writer.write_all(b"C2:BSWV WVTP,DC,OFST,0V\n").unwrap(),
                    "*OPC?" => writer.write_all(b"1\n").unwrap(),
                    _ => {}
                }
                commands.push(command);
            }
            commands
        });

        let mut config = SiglentSdg2042XConfig::new(address.to_string(), "siglent-test", 1);
        config.channels[0].waveform = SiglentWaveform::Sine;
        config.channels[1].waveform = SiglentWaveform::Noise;
        let driver = SiglentSdg2042XDriver::new(config).unwrap();
        let ctx = ControllerCtx::default();
        let mut handle = driver.run(&ctx).unwrap();

        let inputs = [
            1.0, 1_000.0, 0.25, 0.5, 30.0, 0.0, // ch1 sine
            1.0, 0.0, -0.1, 0.0, 0.0, 0.1, // ch2 noise
        ];
        let mut bytes = vec![0; INPUT_SIZE];
        driver
            .peripheral()
            .emit_operating_roundtrip(8, 0, 0, &inputs, &mut bytes);
        assert_eq!(driver.shared.process_request(&bytes).unwrap(), 8);

        let deadline = Instant::now() + Duration::from_secs(1);
        loop {
            if driver.shared.state.lock().unwrap().command_sequence == 1 {
                break;
            }
            assert!(Instant::now() < deadline, "generation was not applied");
            thread::sleep(Duration::from_millis(1));
        }
        handle.join().unwrap();
        let commands = server.join().unwrap();

        assert!(commands.iter().any(|command| {
            command.starts_with("C1:BSWV WVTP,SINE,FRQ,")
                && command.contains(",OFST,2.50000000000000000e-1")
                && command.contains(",PHSE,3.00000000000000000e1")
        }));
        assert!(commands.iter().any(|command| {
            command.starts_with("C2:BSWV WVTP,NOISE,MEAN,-1.00000000000000006e-1")
                && command.contains(",STDEV,1.00000000000000006e-1")
        }));
        assert!(
            commands
                .iter()
                .any(|command| command == "C1:OUTP ON,LOAD,100000")
        );
        assert!(
            commands
                .iter()
                .any(|command| command == "C2:OUTP ON,LOAD,100000")
        );
        assert_eq!(
            &commands[commands.len() - 8..],
            [
                "C1:BSWV WVTP,DC,OFST,0",
                "C2:BSWV WVTP,DC,OFST,0",
                "*OPC?",
                "C1:OUTP OFF,LOAD,100000",
                "C2:OUTP OFF,LOAD,100000",
                "*OPC?",
                "C1:OUTP?",
                "C2:OUTP?",
            ]
        );
    }
}
