//! Keithley DMM6500 DC-voltage acquisition integration.
//!
//! A blocking worker owns the SCPI-over-TCP connection and continuously issues
//! `:READ?`. The controller-facing responder repeats the latest completed
//! finite sample, sequence number, and sample age without blocking the control
//! loop. Sample timestamps are host-observed completion bounds rather than
//! cycle-synchronous measurement times.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use byte_struct::ByteStructUnspecifiedByteOrder;
use serde::{Deserialize, Serialize};

use deimos_shared::peripherals::PeripheralId;
use deimos_shared::states::{ByteStruct, ByteStructLen, OperatingMetrics};

use super::SOFTWARE_MODEL_NUMBER_BASE;
use super::protocol::{
    InstrumentProxy, InstrumentRunHandle, WorkerStatus, attach_instrument, start_driver,
};
use super::scpi::{ScpiClient, ScpiTcpConfig};
use super::wire::{instrument_fields, operating_packets};
use crate::calc::Calc;
use crate::controller::Controller;
use crate::controller::context::ControllerCtx;
use crate::peripheral::Peripheral;

const INPUT_COUNT: usize = 0;
instrument_fields!(OUTPUT_FIELDS = [voltage_v, sample_sequence, sample_age_s,]);
const OUTPUT_COUNT: usize = OUTPUT_FIELDS.len();
operating_packets!(OperatingInput, OperatingOutput, INPUT_COUNT, OUTPUT_COUNT);
// Request: the controller's little-endian u64 packet ID.
// Response: OperatingMetrics followed by voltage, sample sequence, and sample
// age as little-endian f64 values.
const INPUT_SIZE: usize = OperatingInput::BYTE_LEN;
const OUTPUT_SIZE: usize = OperatingOutput::BYTE_LEN;
const MAX_EXACT_F64_INTEGER: u64 = (1_u64 << 53) - 1;

/// Software model number for the Keithley DMM6500 integration.
pub const MODEL_NUMBER: u64 = SOFTWARE_MODEL_NUMBER_BASE + 2;

/// Connection and DC-voltage acquisition configuration for a DMM6500.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct KeithleyDmm6500Config {
    /// Shared SCPI/TCP connection, identity, and timeout settings.
    pub connection: ScpiTcpConfig,
    /// `None` enables autorange; `Some(volts)` selects a fixed voltage range.
    pub range_v: Option<f64>,
    /// Integration aperture in power-line cycles.
    pub nplc: f64,
    /// Whether automatic zero correction is enabled.
    pub autozero: bool,
}

impl KeithleyDmm6500Config {
    /// Build a configuration from a host name or address, adding SCPI port 5025.
    ///
    /// Args:
    ///   host: Host name, IP address, or address with an explicit port.
    ///   serial_number: Logical software serial used in the peripheral ID.
    ///
    /// Returns:
    ///   An autoranging DC-voltage configuration with one NPLC, autozero
    ///   enabled, and bounded connection and I/O timeouts.
    pub fn new(host: impl Into<String>, serial_number: u64) -> Self {
        let mut connection = ScpiTcpConfig::new(host, serial_number, "KEITHLEY", "DMM6500");
        connection.read_timeout = Duration::from_secs(5);
        Self {
            connection,
            range_v: None,
            nplc: 1.0,
            autozero: true,
        }
    }

    fn validate(&self) -> Result<(), String> {
        self.connection.validate()?;
        if !self.nplc.is_finite() || self.nplc <= 0.0 {
            return Err("nplc must be finite and positive".to_owned());
        }
        if self
            .range_v
            .is_some_and(|range| !range.is_finite() || range <= 0.0)
        {
            return Err("range_v must be finite and positive when supplied".to_owned());
        }
        Ok(())
    }
}

/// Pure controller-side representation of a Keithley DMM6500.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct KeithleyDmm6500 {
    /// Logical software serial number used in the Deimos peripheral ID.
    pub serial_number: u64,
}

impl KeithleyDmm6500 {
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
impl Peripheral for KeithleyDmm6500 {
    fn id(&self) -> PeripheralId {
        PeripheralId {
            model_number: MODEL_NUMBER,
            serial_number: self.serial_number,
        }
    }

    fn input_names(&self) -> Vec<String> {
        Vec::new()
    }

    fn output_names(&self) -> Vec<String> {
        OUTPUT_FIELDS.iter().map(ToString::to_string).collect()
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
        _inputs: &[f64],
        bytes: &mut [u8],
    ) {
        OperatingInput { id, values: [] }.write_bytes(bytes);
    }

    fn parse_operating_roundtrip(&self, bytes: &[u8], outputs: &mut [f64]) -> OperatingMetrics {
        let response = OperatingOutput::read_bytes(bytes);
        outputs[..OUTPUT_COUNT].copy_from_slice(&response.values);
        response.metrics
    }

    fn validate_operating_roundtrip(&self, bytes: &[u8]) -> bool {
        bytes.len() == OUTPUT_SIZE
            && OperatingOutput::read_bytes(bytes)
                .values
                .iter()
                .all(|value| value.is_finite())
    }

    fn standard_calcs(&self, _name: &str) -> BTreeMap<String, Box<dyn Calc>> {
        BTreeMap::new()
    }
}

/// Latest complete measurement published by the blocking SCPI worker.
struct State {
    voltage_v: f64,
    sample_sequence: u64,
    sampled_at: Instant,
    status: WorkerStatus,
}

impl State {
    fn publish_sample(&mut self, voltage_v: f64) {
        self.voltage_v = voltage_v;
        self.sample_sequence = self.sample_sequence.wrapping_add(1);
        self.sampled_at = Instant::now();
    }
}

/// Validated configuration plus synchronized measurement state.
struct Shared {
    config: KeithleyDmm6500Config,
    channel_name: String,
    state: Mutex<State>,
}

impl Shared {
    fn new(config: KeithleyDmm6500Config) -> Self {
        let channel_name = format!(
            "instrument-dmm6500-{:016x}",
            config.connection.serial_number
        );
        Self {
            config,
            channel_name,
            state: Mutex::new(State {
                voltage_v: 0.0,
                sample_sequence: 0,
                sampled_at: Instant::now(),
                status: WorkerStatus::default(),
            }),
        }
    }
}

impl InstrumentProxy for Shared {
    fn id(&self) -> PeripheralId {
        KeithleyDmm6500::new(self.config.connection.serial_number).id()
    }

    fn input_size(&self) -> usize {
        INPUT_SIZE
    }

    fn output_size(&self) -> usize {
        OUTPUT_SIZE
    }

    fn process_request(&self, bytes: &[u8]) -> Result<u64, String> {
        if bytes.len() == INPUT_SIZE {
            Ok(OperatingInput::read_bytes(bytes).id)
        } else {
            Err(format!(
                "expected {INPUT_SIZE} request bytes, got {}",
                bytes.len()
            ))
        }
    }

    fn write_response(&self, metrics: OperatingMetrics, bytes: &mut [u8]) -> Result<(), String> {
        let state = self.state.lock().map_err(|_| "DMM6500 state poisoned")?;
        if let Some(error) = state.status.error() {
            return Err(error);
        }
        OperatingOutput {
            metrics,
            values: [
                state.voltage_v,
                state.sample_sequence.min(MAX_EXACT_F64_INTEGER) as f64,
                state.sampled_at.elapsed().as_secs_f64(),
            ],
        }
        .write_bytes(bytes);
        Ok(())
    }

    fn on_loss_of_contact(&self) {}

    fn error(&self) -> Option<String> {
        self.state.lock().ok()?.status.error()
    }
}

/// Owns the live DMM6500 connection and software-peripheral responder.
pub struct KeithleyDmm6500Driver {
    shared: Arc<Shared>,
}

impl KeithleyDmm6500Driver {
    /// Construct a validated DMM6500 driver without connecting it.
    ///
    /// Args:
    ///   config: Connection, DC measurement, and timeout settings.
    ///
    /// Returns:
    ///   A driver ready to be started with [`Self::run`].
    ///
    /// Errors:
    ///   Returns an error when configuration fields are invalid.
    pub fn new(config: KeithleyDmm6500Config) -> Result<Self, String> {
        config.validate()?;
        Ok(Self {
            shared: Arc::new(Shared::new(config)),
        })
    }

    /// Return the pure peripheral paired with this driver.
    ///
    /// Returns:
    ///   A serializable peripheral carrying the driver's logical identity.
    pub fn peripheral(&self) -> KeithleyDmm6500 {
        KeithleyDmm6500::new(self.shared.config.connection.serial_number)
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

    /// Connect, configure DC acquisition, obtain one sample, and start both threads.
    ///
    /// Args:
    ///   ctx: Controller context containing the matching thread channel.
    ///
    /// Returns:
    ///   A handle that owns shutdown and joining for the responder and worker.
    ///
    /// Errors:
    ///   Returns an error for connection, identity, configuration, initial
    ///   reading, parse, or thread startup failures. The protocol responder is
    ///   not started until one finite voltage reading has completed.
    pub fn run(&self, ctx: &ControllerCtx) -> Result<InstrumentRunHandle, String> {
        let shared = self.shared.clone();
        start_driver(
            ctx,
            &self.shared.channel_name,
            format!("dmm6500-{}", self.shared.config.connection.serial_number),
            "DMM6500",
            self.shared.config.connection.startup_timeout(),
            self.shared.clone(),
            move |stop, startup| dmm_worker(shared, stop, startup),
        )
    }
}

/// Attach one configured DMM6500 to a controller.
///
/// This connects, configures, and obtains the first valid reading before
/// registering its software peripheral and automatically named thread-channel
/// socket with `controller`.
///
/// Args:
///   peripheral_name: Unique name used for controller fields such as
///   `peripheral_name.voltage_v`.
///   config: Complete connection, identity, measurement, and timeout
///   configuration.
///   controller: Controller to receive the peripheral and generated socket.
///
/// Returns:
///   A running instrument handle that must outlive the controller run.
///
/// Errors:
///   Returns an error for duplicate peripheral names, invalid configuration,
///   connection or identity failure, initial reading failure, thread startup
///   failure, or controller registration failure.
pub fn attach(
    peripheral_name: &str,
    config: KeithleyDmm6500Config,
    controller: &mut Controller,
) -> Result<InstrumentRunHandle, String> {
    let driver = KeithleyDmm6500Driver::new(config)?;
    let channel_name = driver.channel_name().to_owned();
    attach_instrument(
        peripheral_name,
        &channel_name,
        driver.peripheral(),
        "DMM6500",
        controller,
        |ctx| driver.run(ctx),
    )
}

fn dmm_worker(
    shared: Arc<Shared>,
    stop: Arc<AtomicBool>,
    startup: mpsc::SyncSender<Result<(), String>>,
) -> Result<(), String> {
    // Preserve the first worker failure for the protocol responder. Suppressing
    // later replies lets the controller's ordinary loss-of-contact policy act.
    let result = dmm_worker_inner(&shared, &stop, &startup);
    if let Err(error) = &result
        && let Ok(mut state) = shared.state.lock()
    {
        state.status.latch_error(format!("DMM6500: {error}"));
    }
    result
}

/// Own the SCPI connection and continuously publish complete voltage samples.
///
/// Startup is not reported until identity validation, configuration, and the
/// first finite reading succeed, so the controller never observes placeholder
/// acquisition data from an unverified instrument.
fn dmm_worker_inner(
    shared: &Arc<Shared>,
    stop: &Arc<AtomicBool>,
    startup: &mpsc::SyncSender<Result<(), String>>,
) -> Result<(), String> {
    let config = &shared.config;
    let mut client = match ScpiClient::connect(&config.connection) {
        Ok(client) => client,
        Err(err) => {
            let _ = startup.send(Err(format!("DMM6500 connection failed: {err}")));
            return Err(err);
        }
    };

    let identity = match setup_dmm(&mut client, config) {
        Ok(identity) => identity,
        Err(err) => {
            let _ = startup.send(Err(format!("DMM6500 setup failed: {err}")));
            return Err(err);
        }
    };
    let voltage_v = match read_voltage(&mut client) {
        Ok(sample) => sample,
        Err(err) => {
            let _ = startup.send(Err(format!("DMM6500 initial read failed: {err}")));
            return Err(err);
        }
    };
    {
        let mut state = shared.state.lock().unwrap();
        state.status.set_identity(identity);
        state.publish_sample(voltage_v);
    }
    let _ = startup.send(Ok(()));

    while !stop.load(Ordering::Relaxed) {
        let voltage_v = read_voltage(&mut client)?;
        let mut state = shared.state.lock().unwrap();
        // Timestamp after the complete response arrives. This bounds sample
        // freshness but does not claim cycle-synchronous acquisition timing.
        state.publish_sample(voltage_v);
    }
    Ok(())
}

/// Verify the model and configure single-sample ASCII DC-voltage acquisition.
fn setup_dmm(client: &mut ScpiClient, config: &KeithleyDmm6500Config) -> Result<String, String> {
    let identity = client.identify()?;
    config.connection.validate_identity(&identity)?;
    client.command(":SENSe:FUNCtion \"VOLTage\"")?;
    client.command(":FORMat:DATA ASCii")?;
    client.command(":SENSe:COUNt 1")?;
    match config.range_v {
        Some(range) => client.command(&format!(":SENSe:VOLTage:RANGe {}", scpi_number(range)))?,
        None => client.command(":SENSe:VOLTage:RANGe:AUTO ON")?,
    }
    client.command(&format!(
        ":SENSe:VOLTage:NPLCycles {}",
        scpi_number(config.nplc)
    ))?;
    client.command(if config.autozero {
        ":SENSe:VOLTage:AZERo ON"
    } else {
        ":SENSe:VOLTage:AZERo OFF"
    })?;
    Ok(identity)
}

/// Read one finite voltage.
///
/// Returns:
///   The voltage in volts.
///
/// Errors:
///   Returns transport errors or a nonnumeric/non-finite instrument response.
fn read_voltage(client: &mut ScpiClient) -> Result<f64, String> {
    let response = client.query(":READ?")?;
    let value = response
        .trim()
        .parse::<f64>()
        .map_err(|err| format!("invalid voltage response `{response}`: {err}"))?;
    if !value.is_finite() {
        return Err(format!("non-finite voltage response `{response}`"));
    }
    Ok(value)
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
        let peripheral = KeithleyDmm6500::new(9);
        assert!(peripheral.input_names().is_empty());
        assert_eq!(peripheral.output_names().len(), OUTPUT_COUNT);
        assert_eq!(peripheral.operating_roundtrip_input_size(), INPUT_SIZE);
        assert_eq!(peripheral.operating_roundtrip_output_size(), OUTPUT_SIZE);
    }

    #[test]
    fn thread_channel_name_is_derived_from_model_and_serial() {
        let config = KeithleyDmm6500Config::new("localhost", 0x2a);
        let driver = KeithleyDmm6500Driver::new(config).unwrap();
        assert_eq!(driver.channel_name(), "instrument-dmm6500-000000000000002a");
    }

    #[test]
    fn configuration_rejects_invalid_measurement_settings() {
        let mut config = KeithleyDmm6500Config::new("localhost", 1);
        config.nplc = f64::NAN;
        assert!(config.validate().is_err());
        config.nplc = 1.0;
        config.range_v = Some(0.0);
        assert!(config.validate().is_err());
    }

    #[test]
    fn worker_configures_dc_voltage_and_publishes_fresh_samples() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut writer = stream;
            let mut commands = Vec::new();
            let mut sample = 1.0;
            loop {
                let mut command = String::new();
                if reader.read_line(&mut command).unwrap() == 0 {
                    break;
                }
                let command = command.trim_end().to_owned();
                match command.as_str() {
                    "*IDN?" => writer
                        .write_all(b"KEITHLEY INSTRUMENTS,DMM6500,TEST,1.0\n")
                        .unwrap(),
                    ":READ?" => {
                        thread::sleep(Duration::from_millis(1));
                        writeln!(writer, "{sample}").unwrap();
                        sample += 0.25;
                    }
                    _ => {}
                }
                commands.push(command);
            }
            commands
        });

        let config = KeithleyDmm6500Config::new(address.to_string(), 2);
        let driver = KeithleyDmm6500Driver::new(config).unwrap();
        let ctx = ControllerCtx::default();
        let mut handle = driver.run(&ctx).unwrap();
        let deadline = Instant::now() + Duration::from_secs(1);
        loop {
            if driver.shared.state.lock().unwrap().sample_sequence >= 3 {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "fresh samples were not published"
            );
            thread::sleep(Duration::from_millis(1));
        }
        {
            let state = driver.shared.state.lock().unwrap();
            assert!(state.voltage_v >= 1.5);
            assert!(state.sampled_at.elapsed() < Duration::from_secs(1));
        }
        handle.join().unwrap();
        let commands = server.join().unwrap();

        assert!(
            commands
                .iter()
                .any(|command| command == ":SENSe:FUNCtion \"VOLTage\"")
        );
        assert!(
            commands
                .iter()
                .any(|command| command == ":SENSe:VOLTage:RANGe:AUTO ON")
        );
        assert!(
            commands
                .iter()
                .filter(|command| *command == ":READ?")
                .count()
                >= 3
        );
    }
}
