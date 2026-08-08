use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
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

const INPUT_SIZE: usize = 8;
const OUTPUT_COUNT: usize = 4;
const OUTPUT_SIZE: usize = OperatingMetrics::BYTE_LEN + OUTPUT_COUNT * 8;
const MAX_EXACT_F64_INTEGER: u64 = (1_u64 << 53) - 1;

/// Software model number for the Keithley DMM6500 integration.
pub const MODEL_NUMBER: u64 = SOFTWARE_MODEL_NUMBER_BASE + 2;

/// Connection and DC-voltage acquisition configuration for a DMM6500.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct KeithleyDmm6500Config {
    pub address: String,
    pub channel_name: String,
    pub serial_number: u64,
    pub expected_model: String,
    pub connect_timeout: Duration,
    pub read_timeout: Duration,
    pub write_timeout: Duration,
    /// `None` enables autorange; `Some(volts)` selects a fixed voltage range.
    pub range_v: Option<f64>,
    pub nplc: f64,
    pub autozero: bool,
}

impl KeithleyDmm6500Config {
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
            expected_model: "DMM6500".to_owned(),
            connect_timeout: Duration::from_secs(2),
            read_timeout: Duration::from_secs(5),
            write_timeout: Duration::from_secs(2),
            range_v: None,
            nplc: 1.0,
            autozero: true,
        }
    }

    fn validate(&self) -> Result<(), String> {
        if self.address.trim().is_empty() || self.channel_name.trim().is_empty() {
            return Err("address and channel_name cannot be empty".to_owned());
        }
        if self.expected_model.trim().is_empty() {
            return Err("expected_model cannot be empty".to_owned());
        }
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
    pub serial_number: u64,
}

impl KeithleyDmm6500 {
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
        [
            "voltage_v",
            "sample_sequence",
            "sample_age_s",
            "query_duration_s",
        ]
        .map(str::to_owned)
        .to_vec()
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
        write_u64(bytes, 0, id);
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

struct State {
    voltage_v: f64,
    sample_sequence: u64,
    sampled_at: Instant,
    query_duration: Duration,
    identity: Option<String>,
    error: Option<String>,
}

struct Shared {
    config: KeithleyDmm6500Config,
    state: Mutex<State>,
}

impl Shared {
    fn new(config: KeithleyDmm6500Config) -> Self {
        Self {
            config,
            state: Mutex::new(State {
                voltage_v: 0.0,
                sample_sequence: 0,
                sampled_at: Instant::now(),
                query_duration: Duration::ZERO,
                identity: None,
                error: None,
            }),
        }
    }
}

impl InstrumentProxy for Shared {
    fn id(&self) -> PeripheralId {
        KeithleyDmm6500::new(self.config.serial_number).id()
    }

    fn input_size(&self) -> usize {
        INPUT_SIZE
    }

    fn output_size(&self) -> usize {
        OUTPUT_SIZE
    }

    fn process_request(&self, bytes: &[u8]) -> Result<u64, String> {
        if bytes.len() == INPUT_SIZE {
            Ok(read_u64(bytes, 0))
        } else {
            Err(format!(
                "expected {INPUT_SIZE} request bytes, got {}",
                bytes.len()
            ))
        }
    }

    fn write_response(&self, metrics: OperatingMetrics, bytes: &mut [u8]) -> Result<(), String> {
        let state = self.state.lock().map_err(|_| "DMM6500 state poisoned")?;
        if let Some(error) = &state.error {
            return Err(error.clone());
        }
        metrics.write_bytes(&mut bytes[..OperatingMetrics::BYTE_LEN]);
        for (index, value) in [
            state.voltage_v,
            state.sample_sequence.min(MAX_EXACT_F64_INTEGER) as f64,
            state.sampled_at.elapsed().as_secs_f64(),
            state.query_duration.as_secs_f64(),
        ]
        .into_iter()
        .enumerate()
        {
            write_f64(bytes, OperatingMetrics::BYTE_LEN + index * 8, value);
        }
        Ok(())
    }

    fn on_loss_of_contact(&self) {}

    fn error(&self) -> Option<String> {
        self.state.lock().ok()?.error.clone()
    }
}

/// Owns the live DMM6500 connection and software-peripheral responder.
pub struct KeithleyDmm6500Driver {
    shared: Arc<Shared>,
}

impl KeithleyDmm6500Driver {
    pub fn new(config: KeithleyDmm6500Config) -> Result<Self, String> {
        config.validate()?;
        Ok(Self {
            shared: Arc::new(Shared::new(config)),
        })
    }

    pub fn peripheral(&self) -> KeithleyDmm6500 {
        KeithleyDmm6500::new(self.shared.config.serial_number)
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
            .name(format!("dmm6500-{}", self.shared.config.serial_number))
            .spawn(move || dmm_worker(worker_shared, worker_stop, startup_tx))
            .map_err(|err| format!("failed to spawn DMM6500 worker: {err}"))?;

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
                return Err(format!("DMM6500 startup did not complete: {err}"));
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
                let _ = worker.join();
                return Err(err);
            }
        };
        Ok(InstrumentRunHandle::new(stop, protocol, worker))
    }
}

/// Attach one DMM6500 using the default DC-voltage configuration.
///
/// This connects, configures, and obtains the first valid reading before
/// registering its software peripheral and generated thread-channel socket
/// with `controller`. Construct [`KeithleyDmm6500Config`] and
/// [`KeithleyDmm6500Driver`] directly when range, NPLC, autozero, or timeouts
/// need customization.
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
    let channel_name = format!("instrument-dmm6500-{peripheral_name}-{serial_number:016x}");
    let driver = KeithleyDmm6500Driver::new(KeithleyDmm6500Config::new(
        address,
        channel_name.clone(),
        serial_number,
    ))?;
    let mut handle = driver.run(&controller.ctx)?;
    if let Err(err) = controller.add_peripheral(peripheral_name, Box::new(driver.peripheral())) {
        return match handle.join() {
            Ok(()) => Err(err),
            Err(cleanup) => Err(format!(
                "{err}; additionally failed to stop DMM6500: {cleanup}"
            )),
        };
    }
    controller.add_socket(
        &channel_name,
        Box::new(ThreadChannelSocket::new(&channel_name)),
    );
    Ok(handle)
}

fn dmm_worker(
    shared: Arc<Shared>,
    stop: Arc<AtomicBool>,
    startup: mpsc::SyncSender<Result<(), String>>,
) -> Result<(), String> {
    let result = dmm_worker_inner(&shared, &stop, &startup);
    if let Err(error) = &result
        && let Ok(mut state) = shared.state.lock()
        && state.error.is_none()
    {
        state.error = Some(format!("DMM6500: {error}"));
    }
    result
}

fn dmm_worker_inner(
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
    let (voltage_v, query_duration) = match read_voltage(&mut client) {
        Ok(sample) => sample,
        Err(err) => {
            let _ = startup.send(Err(format!("DMM6500 initial read failed: {err}")));
            return Err(err);
        }
    };
    {
        let mut state = shared.state.lock().unwrap();
        state.identity = Some(identity);
        state.voltage_v = voltage_v;
        state.sample_sequence = 1;
        state.sampled_at = Instant::now();
        state.query_duration = query_duration;
    }
    let _ = startup.send(Ok(()));

    while !stop.load(Ordering::Relaxed) {
        let (voltage_v, query_duration) = read_voltage(&mut client)?;
        let mut state = shared.state.lock().unwrap();
        state.voltage_v = voltage_v;
        state.sample_sequence = state.sample_sequence.wrapping_add(1);
        state.sampled_at = Instant::now();
        state.query_duration = query_duration;
    }
    Ok(())
}

fn setup_dmm(client: &mut ScpiClient, config: &KeithleyDmm6500Config) -> Result<String, String> {
    let identity = client.identify()?;
    let uppercase = identity.to_ascii_uppercase();
    if !uppercase.contains("KEITHLEY")
        || !uppercase.contains(&config.expected_model.to_ascii_uppercase())
    {
        return Err(format!(
            "identity `{identity}` did not match KEITHLEY {}",
            config.expected_model
        ));
    }
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

fn read_voltage(client: &mut ScpiClient) -> Result<(f64, Duration), String> {
    let started = Instant::now();
    let response = client.query(":READ?")?;
    let duration = started.elapsed();
    let value = response
        .trim()
        .parse::<f64>()
        .map_err(|err| format!("invalid voltage response `{response}`: {err}"))?;
    if !value.is_finite() {
        return Err(format!("non-finite voltage response `{response}`"));
    }
    Ok((value, duration))
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
        let peripheral = KeithleyDmm6500::new(9);
        assert!(peripheral.input_names().is_empty());
        assert_eq!(peripheral.output_names().len(), OUTPUT_COUNT);
        assert_eq!(peripheral.operating_roundtrip_input_size(), INPUT_SIZE);
        assert_eq!(peripheral.operating_roundtrip_output_size(), OUTPUT_SIZE);
    }

    #[test]
    fn configuration_rejects_invalid_measurement_settings() {
        let mut config = KeithleyDmm6500Config::new("localhost", "dmm", 1);
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

        let config = KeithleyDmm6500Config::new(address.to_string(), "dmm-test", 2);
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
            assert!(state.query_duration >= Duration::from_millis(1));
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
