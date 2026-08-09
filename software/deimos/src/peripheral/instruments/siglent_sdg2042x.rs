//! Siglent SDG2042X basic-wave generator integration.
//!
//! The peripheral exposes requested settings for both output channels while a
//! dedicated worker serializes SCPI commands over TCP. The protocol responder
//! publishes only the most recently completed two-channel state, so the
//! controller never waits for network or relay latency.
//!
//! A disabled channel is physically driven to 0 V DC before its output relay is
//! opened. Startup, controller loss, explicit disable, worker failure, and
//! shutdown all use this safe state. Arbitrary waveforms, modulation, burst,
//! and the generator's internal sweep subsystem are intentionally unsupported.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, mpsc};
use std::time::Duration;

use byte_struct::ByteStructUnspecifiedByteOrder;
use serde::{Deserialize, Serialize};

use deimos_shared::peripherals::PeripheralId;
use deimos_shared::states::{ByteStruct, ByteStructLen, OperatingMetrics};

use super::SOFTWARE_MODEL_NUMBER_BASE;
use super::protocol::{
    InstrumentProxy, InstrumentRunHandle, WorkerStatus, attach_instrument, start_driver,
};
use super::scpi::{ScpiClient, ScpiTcpConfig};
use crate::calc::Calc;
use crate::controller::Controller;
use crate::controller::context::ControllerCtx;
use crate::peripheral::Peripheral;

const CHANNEL_COUNT: usize = 2;
const CHANNEL_FIELDS: &[&str] = &[
    "enabled",
    "frequency_hz",
    "offset_voltage_v",
    "pulse_duty_cycle",
    "phase_deg",
    "stdev",
];
const VALUES_PER_CHANNEL: usize = CHANNEL_FIELDS.len();
const INPUT_COUNT: usize = CHANNEL_COUNT * VALUES_PER_CHANNEL;
const OUTPUT_COUNT: usize = INPUT_COUNT;

/// Complete dynamic state for one output channel.
#[derive(ByteStruct, Clone, Copy, Debug, Default, PartialEq)]
#[byte_struct_le]
struct ChannelState {
    enabled: f64,
    frequency_hz: f64,
    offset_voltage_v: f64,
    pulse_duty_cycle: f64,
    phase_deg: f64,
    stdev: f64,
}

impl ChannelState {
    fn from_values(values: &[f64]) -> Self {
        Self {
            enabled: values[0],
            frequency_hz: values[1],
            offset_voltage_v: values[2],
            pulse_duty_cycle: values[3],
            phase_deg: values[4],
            stdev: values[5],
        }
    }

    fn write_values(self, values: &mut [f64]) {
        values[0] = self.enabled;
        values[1] = self.frequency_hz;
        values[2] = self.offset_voltage_v;
        values[3] = self.pulse_duty_cycle;
        values[4] = self.phase_deg;
        values[5] = self.stdev;
    }

    fn contains_nan(self) -> bool {
        self.enabled.is_nan()
            || self.frequency_hz.is_nan()
            || self.offset_voltage_v.is_nan()
            || self.pulse_duty_cycle.is_nan()
            || self.phase_deg.is_nan()
            || self.stdev.is_nan()
    }

    /// Clamp one controller command to the configured channel envelope.
    ///
    /// A NaN in any field disables the channel rather than allowing a partial
    /// command to reach the instrument.
    fn normalized(self, config: &SiglentChannelConfig) -> Self {
        if self.contains_nan() {
            return Self::default();
        }
        Self {
            enabled: f64::from(self.enabled >= 0.5),
            frequency_hz: self
                .frequency_hz
                .clamp(config.frequency_hz.0, config.frequency_hz.1),
            offset_voltage_v: self
                .offset_voltage_v
                .clamp(config.offset_voltage_v.0, config.offset_voltage_v.1),
            pulse_duty_cycle: self
                .pulse_duty_cycle
                .clamp(config.pulse_duty_cycle.0, config.pulse_duty_cycle.1),
            phase_deg: self.phase_deg.clamp(config.phase_deg.0, config.phase_deg.1),
            stdev: self.stdev.clamp(config.stdev.0, config.stdev.1),
        }
    }
}

/// Complete dynamic state for both output channels.
#[derive(ByteStruct, Clone, Copy, Debug, Default, PartialEq)]
#[byte_struct_le]
struct InstrumentState {
    ch1: ChannelState,
    ch2: ChannelState,
}

impl InstrumentState {
    fn from_values(values: &[f64]) -> Self {
        Self {
            ch1: ChannelState::from_values(&values[..VALUES_PER_CHANNEL]),
            ch2: ChannelState::from_values(&values[VALUES_PER_CHANNEL..INPUT_COUNT]),
        }
    }

    fn channels(self) -> [ChannelState; CHANNEL_COUNT] {
        [self.ch1, self.ch2]
    }

    fn write_values(self, values: &mut [f64]) {
        self.ch1.write_values(&mut values[..VALUES_PER_CHANNEL]);
        self.ch2
            .write_values(&mut values[VALUES_PER_CHANNEL..OUTPUT_COUNT]);
    }

    fn contains_nan(self) -> bool {
        self.ch1.contains_nan() || self.ch2.contains_nan()
    }

    fn normalized(self, configs: &[SiglentChannelConfig; CHANNEL_COUNT]) -> Self {
        Self {
            ch1: self.ch1.normalized(&configs[0]),
            ch2: self.ch2.normalized(&configs[1]),
        }
    }
}

/// Controller packet containing one coherent two-channel state.
#[derive(ByteStruct, Clone, Copy, Debug, Default)]
#[byte_struct_le]
struct OperatingInput {
    id: u64,
    state: InstrumentState,
}

/// Last completely applied two-channel state returned to the controller.
#[derive(ByteStruct, Clone, Copy, Debug, Default)]
#[byte_struct_le]
struct OperatingOutput {
    metrics: OperatingMetrics,
    state: InstrumentState,
}

// Request: u64 packet ID, then six little-endian f64 values per channel.
// Response: OperatingMetrics followed by both applied channel vectors.
const INPUT_SIZE: usize = OperatingInput::BYTE_LEN;
const OUTPUT_SIZE: usize = OperatingOutput::BYTE_LEN;
const SAFE_LOAD: &str = "100000";

/// Software model number for the Siglent SDG2042X integration.
pub const MODEL_NUMBER: u64 = SOFTWARE_MODEL_NUMBER_BASE + 1;

/// Supported SDG2042X basic waveforms.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum SiglentWaveform {
    /// Sinusoidal waveform.
    Sine,
    /// Square waveform.
    Square,
    /// Ramp or sawtooth waveform.
    Ramp,
    /// Pulse waveform.
    Pulse,
    /// Gaussian noise waveform.
    Noise,
    /// Constant DC waveform.
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
    /// Highest supported numeric load, used to approximate a high-impedance sink.
    HighImpedance,
    /// Explicit load impedance in ohms, inclusive from 50 through 100,000.
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

/// Fixed channel settings and dynamic-command clamp ranges.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct SiglentChannelConfig {
    /// Basic waveform selected whenever this channel is enabled.
    pub waveform: SiglentWaveform,
    /// Fixed peak-to-peak amplitude for sine, square, ramp, and pulse waveforms.
    pub amplitude_vpp: f64,
    /// Load used for output-level compensation.
    pub load: SiglentLoad,
    /// Inclusive frequency clamp range in hertz.
    pub frequency_hz: (f64, f64),
    /// Inclusive DC offset or noise-mean clamp range in volts.
    pub offset_voltage_v: (f64, f64),
    /// Inclusive square/pulse duty-cycle clamp range as a fraction.
    pub pulse_duty_cycle: (f64, f64),
    /// Inclusive phase clamp range in degrees.
    pub phase_deg: (f64, f64),
    /// Inclusive noise standard-deviation clamp range in volts.
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
            && (self.amplitude_vpp.is_nan()
                || self.amplitude_vpp <= 0.0
                || self.amplitude_vpp == f64::INFINITY)
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
    if !range.0.is_nan()
        && !range.1.is_nan()
        && range.0 != f64::NEG_INFINITY
        && range.1 != f64::INFINITY
        && range.0 <= range.1
    {
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
    /// Shared SCPI/TCP connection, identity, and timeout settings.
    pub connection: ScpiTcpConfig,
    /// Fixed settings and accepted dynamic ranges for channels 1 and 2.
    pub channels: [SiglentChannelConfig; CHANNEL_COUNT],
}

impl SiglentSdg2042XConfig {
    /// Build a configuration from a host name or address, adding SCPI port 5025.
    ///
    /// Args:
    ///   host: Host name, IP address, or address with an explicit port.
    ///   serial_number: Logical software serial used in the peripheral ID.
    ///
    /// Returns:
    ///   A configuration with both channels set to DC, high-impedance load
    ///   compensation, conservative timeouts, and output disabled at startup.
    pub fn new(host: impl Into<String>, serial_number: u64) -> Self {
        Self {
            connection: ScpiTcpConfig::new(host, serial_number, "SIGLENT", "SDG2042X"),
            channels: std::array::from_fn(|_| SiglentChannelConfig::default()),
        }
    }

    fn validate(&self) -> Result<(), String> {
        self.connection.validate()?;
        for (index, channel) in self.channels.iter().enumerate() {
            channel.validate(index + 1)?;
        }
        Ok(())
    }
}

/// Pure controller-side representation of an SDG2042X.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SiglentSdg2042X {
    /// Logical software serial number used in the Deimos peripheral ID.
    pub serial_number: u64,
}

impl SiglentSdg2042X {
    /// Construct the pure controller-side peripheral representation.
    ///
    /// Args:
    ///   serial_number: Logical software serial used in the peripheral ID.
    ///
    /// Returns:
    ///   A serializable peripheral with no connection or worker state.
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
        channel_names("applied_")
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
        OperatingInput {
            id,
            state: InstrumentState::from_values(&inputs[..INPUT_COUNT]),
        }
        .write_bytes(bytes);
    }

    fn parse_operating_roundtrip(&self, bytes: &[u8], outputs: &mut [f64]) -> OperatingMetrics {
        let response = OperatingOutput::read_bytes(bytes);
        response.state.write_values(&mut outputs[..OUTPUT_COUNT]);
        response.metrics
    }

    fn validate_operating_roundtrip(&self, bytes: &[u8]) -> bool {
        bytes.len() == OUTPUT_SIZE && !OperatingOutput::read_bytes(bytes).state.contains_nan()
    }

    fn standard_calcs(&self, _name: &str) -> BTreeMap<String, Box<dyn Calc>> {
        BTreeMap::new()
    }
}

fn channel_names(infix: &str) -> Vec<String> {
    let mut names = Vec::with_capacity(INPUT_COUNT);
    for channel in 1..=CHANNEL_COUNT {
        for field in CHANNEL_FIELDS {
            names.push(format!("ch{channel}_{infix}{field}"));
        }
    }
    names
}

/// State shared between the real-time responder and blocking SCPI worker.
///
/// Every valid controller packet replaces `next` without comparison. The worker
/// owns the in-flight request, and `applied` changes only after every SCPI
/// operation for it completes.
struct State {
    next: Option<InstrumentState>,
    applied: InstrumentState,
    status: WorkerStatus,
}

/// Validated configuration plus synchronized instrument state.
struct Shared {
    config: SiglentSdg2042XConfig,
    channel_name: String,
    state: Mutex<State>,
    changed: Condvar,
}

impl Shared {
    fn new(config: SiglentSdg2042XConfig) -> Self {
        let channel_name = format!(
            "instrument-sdg2042x-{:016x}",
            config.connection.serial_number
        );
        Self {
            config,
            channel_name,
            state: Mutex::new(State {
                next: None,
                applied: InstrumentState::default(),
                status: WorkerStatus::default(),
            }),
            changed: Condvar::new(),
        }
    }

    /// Latch an invalid-request error and enqueue a disabled safe state.
    fn reject_request(&self, message: String) {
        let mut state = self.state.lock().unwrap();
        state.next = Some(InstrumentState::default());
        self.changed.notify_one();
        state.status.latch_error(message);
    }
}

impl InstrumentProxy for Shared {
    fn id(&self) -> PeripheralId {
        SiglentSdg2042X::new(self.config.connection.serial_number).id()
    }

    fn input_size(&self) -> usize {
        INPUT_SIZE
    }

    fn output_size(&self) -> usize {
        OUTPUT_SIZE
    }

    fn process_request(&self, bytes: &[u8]) -> Result<u64, String> {
        let (id, request) = match parse_request(bytes) {
            Ok(packet) => packet,
            Err(err) => {
                self.reject_request(format!("SDG2042X rejected operating request: {err}"));
                return Err(err);
            }
        };
        let request = request.normalized(&self.config.channels);

        let mut state = self.state.lock().unwrap();
        state.next = Some(request);
        self.changed.notify_one();
        Ok(id)
    }

    fn write_response(&self, metrics: OperatingMetrics, bytes: &mut [u8]) -> Result<(), String> {
        let state = self.state.lock().map_err(|_| "SDG2042X state poisoned")?;
        if let Some(error) = state.status.error() {
            return Err(error);
        }
        let response = OperatingOutput {
            metrics,
            state: state.applied,
        };
        response.write_bytes(bytes);
        Ok(())
    }

    fn on_loss_of_contact(&self) {
        let mut state = self.state.lock().unwrap();
        state.next = Some(InstrumentState::default());
        self.changed.notify_one();
    }

    fn error(&self) -> Option<String> {
        self.state.lock().ok()?.status.error()
    }
}

/// Owns the live SDG2042X connection and software-peripheral responder.
pub struct SiglentSdg2042XDriver {
    shared: Arc<Shared>,
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
    pub fn new(config: SiglentSdg2042XConfig) -> Result<Self, String> {
        config.validate()?;
        Ok(Self {
            shared: Arc::new(Shared::new(config)),
        })
    }

    /// Return the pure peripheral paired with this driver.
    ///
    /// Returns:
    ///   A serializable peripheral carrying the driver's logical identity.
    pub fn peripheral(&self) -> SiglentSdg2042X {
        SiglentSdg2042X::new(self.shared.config.connection.serial_number)
    }

    /// Return the internal thread-channel name expected by the driver.
    ///
    /// Returns:
    ///   A deterministic name derived from the model and logical serial number.
    pub fn channel_name(&self) -> &str {
        &self.shared.channel_name
    }

    /// Return the validated SCPI identity after successful startup.
    ///
    /// Returns:
    ///   The complete `*IDN?` response, or `None` before startup succeeds.
    pub fn identity(&self) -> Option<String> {
        self.shared.state.lock().ok()?.status.identity()
    }

    /// Connect, validate identity, apply the safe state, and start both threads.
    ///
    /// Args:
    ///   ctx: Controller context containing the matching thread channel.
    ///
    /// Returns:
    ///   A handle that owns shutdown and joining for the responder and worker.
    ///
    /// Errors:
    ///   Returns an error for connection, identity, setup, readback, or thread
    ///   startup failures. The protocol responder is not started until physical
    ///   instrument setup has succeeded.
    pub fn run(&self, ctx: &ControllerCtx) -> Result<InstrumentRunHandle, String> {
        let shared = self.shared.clone();
        start_driver(
            ctx,
            &self.shared.channel_name,
            format!("sdg2042x-{}", self.shared.config.connection.serial_number),
            "SDG2042X",
            self.shared.config.connection.startup_timeout(),
            self.shared.clone(),
            move |stop, startup| siglent_worker(shared, stop, startup),
        )
    }
}

/// Attach one configured SDG2042X to a controller.
///
/// This connects and validates the instrument before registering its software
/// peripheral and automatically named thread-channel socket with `controller`.
/// Both channel configurations and all six dynamic inputs per channel remain
/// independent, while applied outputs are published as one completed
/// two-channel state.
///
/// Args:
///   peripheral_name: Unique name used for controller fields such as
///   `peripheral_name.ch1_enabled`.
///   config: Complete connection, identity, waveform, channel, and timeout
///   configuration.
///   controller: Controller to receive the peripheral and generated socket.
///
/// Returns:
///   A running instrument handle that must outlive the controller run.
///
/// Errors:
///   Returns an error for duplicate peripheral names, invalid configuration,
///   connection or identity failure, safe-state readback failure, thread
///   startup failure, or controller registration failure.
pub fn attach(
    peripheral_name: &str,
    config: SiglentSdg2042XConfig,
    controller: &mut Controller,
) -> Result<InstrumentRunHandle, String> {
    let driver = SiglentSdg2042XDriver::new(config)?;
    let channel_name = driver.channel_name().to_owned();
    attach_instrument(
        peripheral_name,
        &channel_name,
        driver.peripheral(),
        "SDG2042X",
        controller,
        |ctx| driver.run(ctx),
    )
}

fn siglent_worker(
    shared: Arc<Shared>,
    stop: Arc<AtomicBool>,
    startup: mpsc::SyncSender<Result<(), String>>,
) -> Result<(), String> {
    // Keep the first failure visible to the protocol responder. Once latched,
    // it stops emitting apparently healthy responses from stale applied data.
    let result = siglent_worker_inner(&shared, &stop, &startup);
    if let Err(error) = &result
        && let Ok(mut state) = shared.state.lock()
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
    shared: &Arc<Shared>,
    stop: &Arc<AtomicBool>,
    startup: &mpsc::SyncSender<Result<(), String>>,
) -> Result<(), String> {
    let config = &shared.config;
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
    shared.state.lock().unwrap().status.set_identity(identity);
    let _ = startup.send(Ok(()));

    let run_result = loop {
        let mut state = shared.state.lock().unwrap();
        while state.next.is_none() && !stop.load(Ordering::Relaxed) {
            state = shared
                .changed
                .wait_timeout(state, Duration::from_millis(20))
                .unwrap()
                .0;
        }
        if stop.load(Ordering::Relaxed) {
            break Ok(());
        }
        let request = state.next.take().unwrap();
        drop(state);

        if let Err(err) = apply_request(&mut client, config, request) {
            break Err(format!("failed to apply commanded state: {err}"));
        }
        let mut state = shared.state.lock().unwrap();
        state.applied = request;
    };

    let shutdown_result = safe_outputs(&mut client);
    match (run_result, shutdown_result) {
        (Err(err), _) => Err(err),
        (Ok(()), Err(err)) => Err(format!("failed to apply safe state during shutdown: {err}")),
        (Ok(()), Ok(())) => Ok(()),
    }
}

/// Verify the model and establish a read-back-verified safe baseline.
fn setup_siglent(
    client: &mut ScpiClient,
    config: &SiglentSdg2042XConfig,
) -> Result<String, String> {
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

/// Apply and verify the safe state on one channel during an explicit disable.
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
/// A channel is configured before its relay closes. Disabling uses
/// [`safe_channel`] so it is driven to 0 V DC before its relay opens.
fn apply_request(
    client: &mut ScpiClient,
    config: &SiglentSdg2042XConfig,
    request: InstrumentState,
) -> Result<(), String> {
    for (index, desired) in request.channels().into_iter().enumerate() {
        let number = index + 1;
        if desired.enabled == 0.0 {
            safe_channel(client, number)?;
            continue;
        }

        let channel = &config.channels[index];
        let command = basic_wave_command(number, channel, desired);
        client.command(&command)?;
        expect_operation_complete(client)?;
        let load = channel.load.scpi();
        client.command(&format!("C{number}:OUTP ON,LOAD,{load}"))?;
        verify_output_state(client, number, true, &load)?;
    }
    Ok(())
}

/// Render the subset of `BSWV` fields applicable to the configured waveform.
fn basic_wave_command(
    channel_number: usize,
    config: &SiglentChannelConfig,
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

/// Decode the fixed-width operating packet into a two-channel request.
fn parse_request(bytes: &[u8]) -> Result<(u64, InstrumentState), String> {
    if bytes.len() != INPUT_SIZE {
        return Err(format!(
            "expected {INPUT_SIZE} request bytes, got {}",
            bytes.len()
        ));
    }
    let packet = OperatingInput::read_bytes(bytes);
    Ok((packet.id, packet.state))
}

fn scpi_number(value: f64) -> String {
    format!("{value:.17e}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;
    use std::thread;
    use std::time::Instant;

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
    fn repeated_controller_state_is_queued_for_reassertion() {
        let driver =
            SiglentSdg2042XDriver::new(SiglentSdg2042XConfig::new("localhost", 1)).unwrap();
        let request = InstrumentState {
            ch1: ChannelState {
                enabled: 1.0,
                offset_voltage_v: 1.0,
                ..ChannelState::default()
            },
            ch2: ChannelState::default(),
        };
        let mut bytes = vec![0; INPUT_SIZE];
        OperatingInput {
            id: 1,
            state: request,
        }
        .write_bytes(&mut bytes);
        let expected = request.normalized(&driver.shared.config.channels);
        assert_eq!(driver.shared.process_request(&bytes).unwrap(), 1);
        assert_eq!(
            driver.shared.state.lock().unwrap().next.take(),
            Some(expected)
        );
        assert_eq!(driver.shared.process_request(&bytes).unwrap(), 1);
        assert_eq!(driver.shared.state.lock().unwrap().next, Some(expected));
    }

    #[test]
    fn thread_channel_name_is_derived_from_model_and_serial() {
        let config = SiglentSdg2042XConfig::new("localhost", 0x2a);
        let driver = SiglentSdg2042XDriver::new(config).unwrap();
        assert_eq!(
            driver.channel_name(),
            "instrument-sdg2042x-000000000000002a"
        );
    }

    #[test]
    fn controller_values_are_clamped_to_configured_ranges() {
        let configs = std::array::from_fn(|_| SiglentChannelConfig::default());
        let request = InstrumentState {
            ch1: ChannelState {
                enabled: f64::INFINITY,
                frequency_hz: f64::INFINITY,
                offset_voltage_v: f64::NEG_INFINITY,
                pulse_duty_cycle: 99.0,
                phase_deg: -10.0,
                stdev: f64::INFINITY,
            },
            ..InstrumentState::default()
        }
        .normalized(&configs);
        assert_eq!(request.ch1.enabled, 1.0);
        assert_eq!(request.ch1.frequency_hz, configs[0].frequency_hz.1);
        assert_eq!(request.ch1.offset_voltage_v, configs[0].offset_voltage_v.0);
        assert_eq!(request.ch1.pulse_duty_cycle, configs[0].pulse_duty_cycle.1);
        assert_eq!(request.ch1.phase_deg, configs[0].phase_deg.0);
        assert_eq!(request.ch1.stdev, configs[0].stdev.1);
    }

    #[test]
    fn nan_safe_states_only_the_affected_channel() {
        let driver =
            SiglentSdg2042XDriver::new(SiglentSdg2042XConfig::new("localhost", 1)).unwrap();
        let request = InstrumentState {
            ch1: ChannelState {
                enabled: 1.0,
                offset_voltage_v: f64::NAN,
                ..ChannelState::default()
            },
            ch2: ChannelState {
                enabled: 1.0,
                frequency_hz: 1_000.0,
                offset_voltage_v: 2.0,
                ..ChannelState::default()
            },
        };
        let mut bytes = vec![0; INPUT_SIZE];
        OperatingInput {
            id: 1,
            state: request,
        }
        .write_bytes(&mut bytes);
        assert_eq!(driver.shared.process_request(&bytes).unwrap(), 1);
        let request = driver.shared.state.lock().unwrap().next.unwrap();
        assert_eq!(request.ch1, ChannelState::default());
        assert_eq!(request.ch2.enabled, 1.0);
        assert_eq!(request.ch2.offset_voltage_v, 2.0);
        assert!(driver.shared.error().is_none());
    }

    #[test]
    fn duty_fraction_is_rendered_as_percent() {
        assert_eq!(scpi_number(0.25 * 100.0), "2.50000000000000000e1");
    }

    #[test]
    fn every_waveform_emits_only_its_applicable_fields() {
        let request = ChannelState {
            enabled: 1.0,
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
    fn worker_applies_complete_two_channel_state_and_shuts_down_safe() {
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

        let mut config = SiglentSdg2042XConfig::new(address.to_string(), 1);
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
        let expected = parse_request(&bytes)
            .unwrap()
            .1
            .normalized(&driver.shared.config.channels);
        assert_eq!(driver.shared.process_request(&bytes).unwrap(), 8);

        let deadline = Instant::now() + Duration::from_secs(1);
        loop {
            if driver.shared.state.lock().unwrap().applied == expected {
                break;
            }
            assert!(Instant::now() < deadline, "commanded state was not applied");
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
