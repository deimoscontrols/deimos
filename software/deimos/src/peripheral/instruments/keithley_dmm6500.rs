//! Keithley DMM6500 DC-voltage and four-wire-resistance integration.
//!
//! A blocking worker owns the SCPI-over-TCP connection and continuously issues
//! `:READ?`. The controller-facing responder repeats the latest completed
//! numeric sample, sequence number, and sample age without blocking the control
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
use super::responder::{
    InstrumentProxy, InstrumentRunHandle, WorkerStatus, attach_instrument, start_driver,
};
use super::scpi::{ScpiClient, ScpiTcpConfig};
use crate::calc::Calc;
use crate::controller::Controller;
use crate::controller::context::ControllerCtx;
use crate::peripheral::Peripheral;

const SAMPLE_SEQUENCE_OUTPUT: &str = "sample_sequence";
const SAMPLE_AGE_OUTPUT: &str = "sample_age_s";
const MINIMUM_LINE_FREQUENCY_HZ: f64 = 50.0;
const MEASUREMENT_PROCESSING_MARGIN: Duration = Duration::from_millis(250);
const STARTUP_PROCESSING_MARGIN: Duration = Duration::from_millis(250);

/// Controller request for the latest completed DMM reading.
#[derive(ByteStruct, Clone, Copy, Debug, Default)]
#[byte_struct_le]
struct OperatingInput {
    id: u64,
}

/// Latest completed DMM reading returned to the controller.
#[derive(ByteStruct, Clone, Copy, Debug, Default)]
#[byte_struct_le]
struct OperatingOutput {
    metrics: OperatingMetrics,
    value: f64,
    sample_sequence: f64,
    sample_age_s: f64,
}

// Request: the controller's little-endian u64 packet ID.
// Response: OperatingMetrics followed by value, sample sequence, and sample
// age as little-endian f64 values.
const INPUT_SIZE: usize = OperatingInput::BYTE_LEN;
const OUTPUT_SIZE: usize = OperatingOutput::BYTE_LEN;
const MAX_EXACT_F64_INTEGER: u64 = (1_u64 << 53) - 1;
const OVERFLOW_READING_ABS: f64 = 9.9e37;

/// Software model number for the Keithley DMM6500 integration.
pub const MODEL_NUMBER: u64 = SOFTWARE_MODEL_NUMBER_BASE + 2;

/// DMM6500 measurement function selected once during driver startup.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
pub enum Function {
    /// DC-voltage measurement with an optional fixed range in volts.
    DcVoltage {
        /// `None` enables autorange; `Some(volts)` selects a fixed range.
        range_v: Option<f64>,
    },
    /// Four-wire resistance measurement with an optional fixed range in ohms.
    FourWireResistance {
        /// `None` enables autorange; `Some(ohms)` selects a fixed range.
        range_ohm: Option<f64>,
        /// Whether to enable offset-compensated resistance measurements.
        offset_compensation: bool,
    },
}

impl Default for Function {
    fn default() -> Self {
        Self::DcVoltage { range_v: None }
    }
}

impl Function {
    fn kind(self) -> FunctionKind {
        match self {
            Self::DcVoltage { .. } => FunctionKind::DcVoltage,
            Self::FourWireResistance { .. } => FunctionKind::FourWireResistance,
        }
    }

    fn validate(self) -> Result<(), String> {
        let (name, range) = match self {
            Self::DcVoltage { range_v } => ("range_v", range_v),
            Self::FourWireResistance { range_ohm, .. } => ("range_ohm", range_ohm),
        };
        if range.is_some_and(|value| value.is_nan() || value <= 0.0 || value == f64::INFINITY) {
            Err(format!("{name} must be finite and positive when supplied"))
        } else {
            Ok(())
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
enum FunctionKind {
    #[default]
    DcVoltage,
    FourWireResistance,
}

impl FunctionKind {
    fn output_name(self) -> &'static str {
        match self {
            Self::DcVoltage => "voltage_v",
            Self::FourWireResistance => "resistance_ohm",
        }
    }
}

/// Connection and measurement configuration for a DMM6500.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct Config {
    /// Shared SCPI/TCP connection, identity, and timeout settings.
    ///
    /// `read_timeout` is a lower bound; the driver raises its socket read
    /// timeout when the configured NPLC and correction modes require longer.
    pub connection: ScpiTcpConfig,
    /// Measurement function and function-specific settings fixed for the run.
    #[serde(default)]
    pub function: Function,
    /// Integration aperture in power-line cycles, also used to budget reads.
    pub nplc: f64,
    /// Whether automatic zero correction is enabled.
    pub autozero: bool,
}

impl Config {
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
        Self {
            connection: ScpiTcpConfig::new(host, serial_number, "KEITHLEY", "DMM6500"),
            function: Function::default(),
            nplc: 1.0,
            autozero: true,
        }
    }

    fn validate(&self) -> Result<(), String> {
        self.connection.validate()?;
        if self.nplc.is_nan() || self.nplc <= 0.0 || self.nplc == f64::INFINITY {
            return Err("nplc must be finite and positive".to_owned());
        }
        self.function.validate()
    }

    /// Estimate a complete reading using worst-case 50 Hz line timing.
    fn measurement_read_timeout(&self) -> Duration {
        let mut aperture_count = if self.autozero { 3 } else { 1 };
        if matches!(
            self.function,
            Function::FourWireResistance {
                offset_compensation: true,
                ..
            }
        ) {
            aperture_count *= 2;
        }
        let aperture_s = self.nplc * f64::from(aperture_count) / MINIMUM_LINE_FREQUENCY_HZ;
        let aperture = if aperture_s >= Duration::MAX.as_secs_f64() {
            Duration::MAX
        } else {
            Duration::from_secs_f64(aperture_s)
        };
        let estimated = aperture.saturating_add(MEASUREMENT_PROCESSING_MARGIN);
        self.connection.read_timeout.max(estimated)
    }

    /// Build socket settings with enough read time for the configured aperture.
    fn effective_connection(&self) -> ScpiTcpConfig {
        let mut connection = self.connection.clone();
        connection.read_timeout = self.measurement_read_timeout();
        connection
    }

    /// Budget identity, configuration, and the first complete measurement.
    fn startup_timeout(&self) -> Duration {
        let configuration_command_count = match self.function {
            Function::DcVoltage { .. } => 6,
            Function::FourWireResistance { .. } => 7,
        };
        // `*IDN?` uses the ordinary query budget. `:READ?` contributes one
        // command write plus its independently calculated measurement time.
        self.connection.startup_timeout(
            1,
            configuration_command_count + 1,
            self.measurement_read_timeout()
                .saturating_add(STARTUP_PROCESSING_MARGIN),
        )
    }
}

/// Pure controller-side representation of a Keithley DMM6500.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct KeithleyDmm6500 {
    /// Logical software serial number used in the Deimos peripheral ID.
    pub serial_number: u64,
    // The pure peripheral needs only the function category to preserve its
    // output field name across serialization; ranges remain driver concerns.
    #[serde(default)]
    function: FunctionKind,
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
        Self {
            serial_number,
            function: FunctionKind::default(),
        }
    }

    fn with_function(serial_number: u64, function: Function) -> Self {
        Self {
            serial_number,
            function: function.kind(),
        }
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
        [
            self.function.output_name(),
            SAMPLE_SEQUENCE_OUTPUT,
            SAMPLE_AGE_OUTPUT,
        ]
        .into_iter()
        .map(ToOwned::to_owned)
        .collect()
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
        OperatingInput { id }.write_bytes(bytes);
    }

    fn parse_operating_roundtrip(&self, bytes: &[u8], outputs: &mut [f64]) -> OperatingMetrics {
        let response = OperatingOutput::read_bytes(bytes);
        outputs[0] = response.value;
        outputs[1] = response.sample_sequence;
        outputs[2] = response.sample_age_s;
        response.metrics
    }

    fn validate_operating_roundtrip(&self, bytes: &[u8]) -> bool {
        bytes.len() == OUTPUT_SIZE && {
            let response = OperatingOutput::read_bytes(bytes);
            !response.value.is_nan()
                && !response.sample_sequence.is_nan()
                && !response.sample_age_s.is_nan()
        }
    }

    fn standard_calcs(&self, _name: &str) -> BTreeMap<String, Box<dyn Calc>> {
        BTreeMap::new()
    }
}

/// Latest complete measurement published by the blocking SCPI worker.
struct State {
    value: f64,
    sample_sequence: u64,
    sampled_at: Instant,
    status: WorkerStatus,
}

impl State {
    fn publish_sample(&mut self, value: f64) {
        self.value = value;
        self.sample_sequence = self.sample_sequence.wrapping_add(1);
        self.sampled_at = Instant::now();
    }
}

/// Validated configuration plus synchronized measurement state.
struct Shared {
    config: Config,
    channel_name: String,
    state: Mutex<State>,
}

impl Shared {
    fn new(config: Config) -> Self {
        let channel_name = format!(
            "instrument-dmm6500-{:016x}",
            config.connection.serial_number
        );
        Self {
            config,
            channel_name,
            state: Mutex::new(State {
                value: 0.0,
                sample_sequence: 0,
                sampled_at: Instant::now(),
                status: WorkerStatus::default(),
            }),
        }
    }
}

impl InstrumentProxy for Shared {
    fn id(&self) -> PeripheralId {
        KeithleyDmm6500::with_function(self.config.connection.serial_number, self.config.function)
            .id()
    }

    fn input_size(&self) -> usize {
        INPUT_SIZE
    }

    fn output_size(&self) -> usize {
        OUTPUT_SIZE
    }

    fn process_request(&self, bytes: &[u8]) -> u64 {
        // The DMM has no dynamic controller inputs. Each request merely asks for
        // the latest sample and carries the ID needed for protocol metrics.
        OperatingInput::read_bytes(bytes).id
    }

    fn write_response(&self, metrics: OperatingMetrics, bytes: &mut [u8]) -> Result<(), String> {
        let state = self.state.lock().map_err(|_| "DMM6500 state poisoned")?;
        if let Some(error) = state.status.error() {
            return Err(error);
        }
        OperatingOutput {
            metrics,
            value: state.value,
            // f64 represents every integer exactly only through 2^53 - 1.
            sample_sequence: state.sample_sequence.min(MAX_EXACT_F64_INTEGER) as f64,
            // Age makes the loose worker timing explicit to controller calcs.
            sample_age_s: state.sampled_at.elapsed().as_secs_f64(),
        }
        .write_bytes(bytes);
        Ok(())
    }

    // This input-only instrument has no energized output to safe on contact loss.
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
    pub fn new(config: Config) -> Result<Self, String> {
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
        KeithleyDmm6500::with_function(
            self.shared.config.connection.serial_number,
            self.shared.config.function,
        )
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

    /// Connect, configure acquisition, obtain one sample, and start both threads.
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
    ///   not started until one valid configured-function reading has completed.
    pub fn run(&self, ctx: &ControllerCtx) -> Result<InstrumentRunHandle, String> {
        let shared = self.shared.clone();
        start_driver(
            ctx,
            &self.shared.channel_name,
            format!("dmm6500-{}", self.shared.config.connection.serial_number),
            "DMM6500",
            self.shared.config.startup_timeout(),
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
///   `peripheral_name.voltage_v` or `peripheral_name.resistance_ohm`.
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
    config: Config,
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

/// Own the SCPI connection and continuously publish complete measurement samples.
///
/// Startup is not reported until identity validation, configuration, and the
/// first non-NaN reading succeeds, so the controller never observes placeholder
/// acquisition data from an unverified instrument.
fn dmm_worker_inner(
    shared: &Arc<Shared>,
    stop: &Arc<AtomicBool>,
    startup: &mpsc::SyncSender<Result<(), String>>,
) -> Result<(), String> {
    let config = &shared.config;
    let connection = config.effective_connection();
    let mut client = match ScpiClient::connect(&connection) {
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
    let value = match read_measurement(&mut client) {
        Ok(sample) => sample,
        Err(err) => {
            let _ = startup.send(Err(format!("DMM6500 initial read failed: {err}")));
            return Err(err);
        }
    };
    {
        let mut state = shared.state.lock().unwrap();
        state.status.set_identity(identity);
        state.publish_sample(value);
    }
    let _ = startup.send(Ok(()));

    while !stop.load(Ordering::Relaxed) {
        // `:READ?` blocks only this worker. The responder continues returning
        // the previous complete sample, together with its increasing age.
        let value = read_measurement(&mut client)?;
        let mut state = shared.state.lock().unwrap();
        // Timestamp after the complete response arrives. This bounds sample
        // freshness but does not claim cycle-synchronous acquisition timing.
        state.publish_sample(value);
    }
    Ok(())
}

/// Verify the model and configure single-sample ASCII acquisition.
fn setup_dmm(client: &mut ScpiClient, config: &Config) -> Result<String, String> {
    let identity = client.identify()?;
    config.connection.validate_identity(&identity)?;
    client.command(":FORMat:DATA ASCii")?;
    client.command(":SENSe:COUNt 1")?;
    match config.function {
        Function::DcVoltage { range_v } => {
            client.command(":SENSe:FUNCtion \"VOLTage\"")?;
            match range_v {
                Some(range) => {
                    client.command(&format!(":SENSe:VOLTage:RANGe {}", scpi_number(range)))?
                }
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
        }
        Function::FourWireResistance {
            range_ohm,
            offset_compensation,
        } => {
            client.command(":SENS:FUNC \"FRES\"")?;
            match range_ohm {
                Some(range) => {
                    client.command(&format!(":SENS:FRES:RANG {}", scpi_number(range)))?
                }
                None => client.command(":SENS:FRES:RANG:AUTO ON")?,
            }
            client.command(if offset_compensation {
                ":SENS:FRES:OCOM ON"
            } else {
                ":SENS:FRES:OCOM OFF"
            })?;
            client.command(if config.autozero {
                ":SENS:FRES:AZER ON"
            } else {
                ":SENS:FRES:AZER OFF"
            })?;
            client.command(&format!(":SENS:FRES:NPLC {}", scpi_number(config.nplc)))?;
        }
    }
    Ok(identity)
}

/// Read one numeric measurement.
///
/// Returns:
///   The configured measurement in volts or ohms.
///
/// Errors:
///   Returns transport errors or a nonnumeric, NaN, infinite, or overrange
///   instrument response.
fn read_measurement(client: &mut ScpiClient) -> Result<f64, String> {
    let response = client.query(":READ?")?;
    parse_measurement(&response)
}

fn parse_measurement(response: &str) -> Result<f64, String> {
    let value = response
        .trim()
        .parse::<f64>()
        .map_err(|err| format!("invalid measurement response `{response}`: {err}"))?;
    if value.is_nan() {
        return Err(format!("NaN measurement response `{response}`"));
    }
    if value.abs() >= OVERFLOW_READING_ABS {
        return Err(format!("overrange measurement response `{response}`"));
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
        let voltage = KeithleyDmm6500::new(9);
        let resistance = KeithleyDmm6500::with_function(
            9,
            Function::FourWireResistance {
                range_ohm: None,
                offset_compensation: true,
            },
        );
        assert!(voltage.input_names().is_empty());
        assert_eq!(
            voltage.output_names(),
            ["voltage_v", "sample_sequence", "sample_age_s"]
        );
        assert_eq!(
            resistance.output_names(),
            ["resistance_ohm", "sample_sequence", "sample_age_s"]
        );
        assert_eq!(voltage.operating_roundtrip_input_size(), INPUT_SIZE);
        assert_eq!(voltage.operating_roundtrip_output_size(), OUTPUT_SIZE);
        assert_eq!(
            voltage.operating_roundtrip_output_size(),
            resistance.operating_roundtrip_output_size()
        );
        let serialized = serde_json::to_string(&resistance).unwrap();
        let restored: KeithleyDmm6500 = serde_json::from_str(&serialized).unwrap();
        assert_eq!(restored.output_names()[0], "resistance_ohm");
    }

    #[test]
    fn thread_channel_name_is_derived_from_model_and_serial() {
        let config = Config::new("localhost", 0x2a);
        let driver = KeithleyDmm6500Driver::new(config).unwrap();
        assert_eq!(driver.channel_name(), "instrument-dmm6500-000000000000002a");
    }

    #[test]
    fn configuration_rejects_invalid_measurement_settings() {
        let mut config = Config::new("localhost", 1);
        config.nplc = f64::NAN;
        assert!(config.validate().is_err());
        config.nplc = 1.0;
        config.function = Function::DcVoltage { range_v: Some(0.0) };
        assert!(config.validate().is_err());
        config.function = Function::FourWireResistance {
            range_ohm: Some(f64::INFINITY),
            offset_compensation: true,
        };
        assert!(config.validate().is_err());
    }

    #[test]
    fn measurement_parser_rejects_invalid_and_overrange_values() {
        assert_eq!(parse_measurement("-1.25e-3\n").unwrap(), -1.25e-3);
        assert!(parse_measurement("not-a-number").is_err());
        assert!(parse_measurement("NaN").is_err());
        assert!(parse_measurement("inf").is_err());
        assert!(parse_measurement("9.9e37").is_err());
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

        let mut config = Config::new(address.to_string(), 2);
        config.function = Function::DcVoltage {
            range_v: Some(10.0),
        };
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
            assert!(state.value >= 1.5);
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
                .any(|command| command == ":SENSe:VOLTage:RANGe 1.00000000000000000e1")
        );
        assert!(
            commands
                .iter()
                .filter(|command| *command == ":READ?")
                .count()
                >= 3
        );
    }

    #[test]
    fn worker_configures_four_wire_resistance_and_offset_compensation() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut writer = stream;
            let mut commands = Vec::new();
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
                    ":READ?" => writer.write_all(b"1000.25\n").unwrap(),
                    _ => {}
                }
                commands.push(command);
            }
            commands
        });

        let mut config = Config::new(address.to_string(), 3);
        config.function = Function::FourWireResistance {
            range_ohm: None,
            offset_compensation: true,
        };
        let driver = KeithleyDmm6500Driver::new(config).unwrap();
        assert_eq!(driver.peripheral().output_names()[0], "resistance_ohm");
        let ctx = ControllerCtx::default();
        let mut handle = driver.run(&ctx).unwrap();
        let deadline = Instant::now() + Duration::from_secs(1);
        while driver.shared.state.lock().unwrap().sample_sequence < 2 {
            assert!(
                Instant::now() < deadline,
                "resistance sample was not published"
            );
            thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(driver.shared.state.lock().unwrap().value, 1000.25);
        handle.join().unwrap();
        let commands = server.join().unwrap();

        for expected in [
            ":SENS:FUNC \"FRES\"",
            ":SENS:FRES:RANG:AUTO ON",
            ":SENS:FRES:OCOM ON",
            ":SENS:FRES:AZER ON",
            ":SENS:FRES:NPLC 1.00000000000000000e0",
        ] {
            assert!(commands.iter().any(|command| command == expected));
        }
    }
}
