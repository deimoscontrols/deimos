//! Modbus/TCP lifecycle and stress checks for a calibrated rev7 DAQ.
//!
//! Run with:
//!
//! ```text
//! cargo run -p deimos_procedures --bin rev7_modbus_test -- [SUITE] [IP[:PORT]]
//! ```
//!
//! `SUITE` is `quick` (the default), `protocol`, `lifecycle`, `endpoints`,
//! `corrections`, `backpressure`, `address-hold`, `timeout`, or `all`.
//! `address-hold` keeps a fallback-address session active for 20 seconds so a
//! DHCP server can be introduced externally. The one-minute timeout check is
//! omitted from `quick`. Except for the finite two-request protocol check and
//! the intentionally adversarial backpressure test, the client keeps exactly
//! one request outstanding.
//!
//! References:
//!   \[1\] Modbus Organization, *MODBUS Application Protocol Specification
//!   V1.1b3*, 2012.
//!   \[2\] Modbus Organization, *MODBUS Messaging on TCP/IP Implementation
//!   Guide V1.0b*, 2006.

use std::{
    env,
    error::Error,
    io::{ErrorKind, Read, Write},
    net::{Shutdown, TcpStream, ToSocketAddrs},
    thread,
    time::{Duration, Instant},
};

use deimos_shared::peripherals::deimos_daq_rev7::{
    MIN_CYCLE_RATE_HZ, ModbusInitialConfig, OperatingSnapshot,
    modbus::{
        HOLDING_CYCLE_PERIOD_NS, HOLDING_GPIO, HOLDING_LOSS_OF_CONTACT_COUNTER,
        HOLDING_PERIOD_DELTA_NS, HOLDING_PHASE_DELTA_NS, HOLDING_PWM_DUTY_FRAC,
        HOLDING_REGISTER_COUNT, HOLDING_SNAPSHOT_REGISTER_COUNT, HOLDING_SNAPSHOT_START,
        MODBUS_MAX_CYCLE_RATE_HZ, MODBUS_MAX_READ_REGISTERS, MODBUS_MAX_READ_WRITE_WRITE_REGISTERS,
        MODBUS_MAX_WRITE_REGISTERS, MODBUS_READ_WRITE_MULTIPLE_REGISTERS_FUNCTION,
        SNAPSHOT_INPUT_REGISTER_COUNT, SNAPSHOT_INPUT_START, holding_registers,
        snapshot_from_input_registers,
    },
};
use socket2::SockRef;

/// Default rev7 SN3 fallback endpoint in `host:port` form.
const DEFAULT_ENDPOINT: &str = "169.254.101.34:502";
/// Maximum duration of one blocking host socket operation.
const IO_TIMEOUT: Duration = Duration::from_secs(3);
/// Maximum nominal duration allowed for a board relisten transition.
const RECONNECT_TIMEOUT: Duration = Duration::from_secs(8);
/// Observation interval covering the default one-minute application timeout.
const DEFAULT_TIMEOUT_TEST: Duration = Duration::from_secs(62);

/// Fallible result used by the finite hardware-test suites.
type TestResult<T = ()> = Result<T, Box<dyn Error>>;

/// Select and run one finite Modbus/TCP hardware-test suite.
fn main() -> TestResult {
    let mut args = env::args().skip(1);
    let suite = args.next().unwrap_or_else(|| "quick".to_owned());
    let endpoint = normalize_endpoint(args.next().unwrap_or_else(|| DEFAULT_ENDPOINT.to_owned()));
    if args.next().is_some() {
        return Err("usage: rev7_modbus_test [SUITE] [IP[:PORT]]".into());
    }

    println!("modbus_suite={suite}");
    println!("endpoint={endpoint}");
    match suite.as_str() {
        "quick" => {
            protocol_suite(&endpoint)?;
            lifecycle_suite(&endpoint)?;
            endpoint_suite(&endpoint)?;
            correction_suite(&endpoint)?;
            backpressure_suite(&endpoint)?;
        }
        "protocol" => protocol_suite(&endpoint)?,
        "lifecycle" => lifecycle_suite(&endpoint)?,
        "endpoints" => endpoint_suite(&endpoint)?,
        "corrections" => correction_suite(&endpoint)?,
        "backpressure" => backpressure_suite(&endpoint)?,
        "address-hold" => address_hold_suite(&endpoint)?,
        "timeout" => timeout_suite(&endpoint)?,
        "all" => {
            protocol_suite(&endpoint)?;
            lifecycle_suite(&endpoint)?;
            endpoint_suite(&endpoint)?;
            correction_suite(&endpoint)?;
            backpressure_suite(&endpoint)?;
            timeout_suite(&endpoint)?;
        }
        _ => return Err(format!("unknown Modbus/TCP suite {suite:?}").into()),
    }
    println!("modbus_result=pass");
    Ok(())
}

/// Keep a fallback-address session active while the network gains DHCP service.
///
/// The surrounding test changes the link externally during this finite window,
/// then checks that the board applies the deferred lease only after this client
/// closes. Every request remains sequential and validates a complete snapshot.
///
/// Args:
///   endpoint: Board fallback TCP endpoint in `host:port` form.
fn address_hold_suite(endpoint: &str) -> TestResult {
    println!("suite=address-hold status=running duration_s=20");
    let mut client = ModbusClient::connect_retry(endpoint)?;
    let started = Instant::now();
    let mut reads = 0_u32;
    let mut last_id = 0_u64;
    while started.elapsed() < Duration::from_secs(20) && reads < 250 {
        last_id = client.read_snapshot(37)?.metrics.id;
        reads += 1;
    }
    if started.elapsed() < Duration::from_secs(20) {
        return Err("address-hold exhausted its finite read bound early".into());
    }
    println!("address_hold_reads={reads} last_snapshot_id={last_id}");
    println!("suite=address-hold status=pass");
    Ok(())
}

/// Exercise fragmented, rejected, pipelined, and connection-resetting requests.
///
/// Args:
///   endpoint: Board TCP endpoint in `host:port` form.
fn protocol_suite(endpoint: &str) -> TestResult {
    println!("suite=protocol status=running");
    let mut client = ModbusClient::connect(endpoint)?;

    // Split a valid request both within the MBAP prefix and within the PDU. A
    // TCP implementation must use the MBAP length instead of recv boundaries.
    let request = read_request(1, 0, 0x04, SNAPSHOT_INPUT_START, 1);
    client.stream.write_all(&request[..3])?;
    thread::sleep(Duration::from_millis(30));
    client.stream.write_all(&request[3..9])?;
    thread::sleep(Duration::from_millis(30));
    client.stream.write_all(&request[9..])?;
    let response = read_one_adu(&mut client.stream)?;
    validate_response_header(&response, 1, 0, 0x04)?;
    parse_read_registers(&response, 0x04, 1)?;
    client.next_transaction = 2;

    expect_exception(&mut client, &[0x06, 0, 0, 0, 0], 0x01)?;
    expect_exception_for_read(&mut client, 0x04, 0, 0, 0x03)?;
    // Start at the final valid register and extend one register past the
    // shared snapshot boundary. Deriving this address keeps the malformed
    // probe valid when the synchronized snapshot gains fields.
    expect_exception_for_read(
        &mut client,
        0x04,
        SNAPSHOT_INPUT_START + SNAPSHOT_INPUT_REGISTER_COUNT - 1,
        2,
        0x02,
    )?;

    // A complete FC16 whose byte count disagrees with its register count is a
    // framed semantic error, so the connection remains usable afterward.
    let transaction = client.take_transaction();
    let bad_write = adu(transaction, 0, 0, &[0x10, 0, 0, 0, 2, 2, 0, 0]);
    client.stream.write_all(&bad_write)?;
    let response = read_one_adu(&mut client.stream)?;
    validate_exception(&response, transaction, 0, 0x10, 0x03)?;
    client.read_registers(0x04, 0, 1, 7)?;

    // FC23 is the synchronized control contract: apply one complete writable
    // block and return the snapshot captured at the start of that board cycle.
    let snapshot = client.read_write_snapshot(HOLDING_GPIO, &[0], 37)?;
    if snapshot.magic == 0 {
        return Err("FC23 returned an uninitialized snapshot".into());
    }

    // Place two complete FC23 requests in one TCP segment. Both must be drained
    // during one publication and therefore return the exact same latched sample.
    let first_transaction = client.take_transaction();
    let second_transaction = client.take_transaction();
    let mut pipeline = read_write_request(
        first_transaction,
        0,
        HOLDING_SNAPSHOT_START,
        HOLDING_SNAPSHOT_REGISTER_COUNT,
        HOLDING_GPIO,
        &[0],
    )?;
    pipeline.extend_from_slice(&read_write_request(
        second_transaction,
        255,
        HOLDING_SNAPSHOT_START,
        HOLDING_SNAPSHOT_REGISTER_COUNT,
        HOLDING_GPIO,
        &[0],
    )?);
    client.stream.write_all(&pipeline)?;
    let first_response = read_one_adu(&mut client.stream)?;
    let second_response = read_one_adu(&mut client.stream)?;
    validate_response_header(
        &first_response,
        first_transaction,
        0,
        MODBUS_READ_WRITE_MULTIPLE_REGISTERS_FUNCTION,
    )?;
    validate_response_header(
        &second_response,
        second_transaction,
        255,
        MODBUS_READ_WRITE_MULTIPLE_REGISTERS_FUNCTION,
    )?;
    let first_snapshot = snapshot_from_input_registers(&parse_read_registers(
        &first_response,
        MODBUS_READ_WRITE_MULTIPLE_REGISTERS_FUNCTION,
        HOLDING_SNAPSHOT_REGISTER_COUNT,
    )?)
    .map_err(|error| format!("invalid first pipelined FC23 snapshot: {error:?}"))?;
    let second_snapshot = snapshot_from_input_registers(&parse_read_registers(
        &second_response,
        MODBUS_READ_WRITE_MULTIPLE_REGISTERS_FUNCTION,
        HOLDING_SNAPSHOT_REGISTER_COUNT,
    )?)
    .map_err(|error| format!("invalid second pipelined FC23 snapshot: {error:?}"))?;
    if first_snapshot.metrics.id != second_snapshot.metrics.id
        || first_snapshot.sample_time_ns != second_snapshot.sample_time_ns
    {
        return Err("two queued FC23 requests were not served from one cycle snapshot".into());
    }
    drop(client);

    // Nonzero protocol IDs and impossible MBAP lengths cannot be resynchronized
    // safely; each one must reset only that connection and allow a fresh one.
    for malformed in [
        adu(100, 1, 0, &[0x04, 0, 0, 0, 1]),
        vec![0, 101, 0, 0, 0, 1],
        vec![0, 102, 0, 0, 0, 251],
    ] {
        let mut stream = connect_with_retry(endpoint, RECONNECT_TIMEOUT)?;
        stream.write_all(&malformed)?;
        expect_peer_disconnect(&mut stream)?;
        verify_fresh_connection(endpoint)?;
    }

    // Closing in the middle of an otherwise valid ADU must not contaminate the
    // framing state retained for the next client.
    let mut stream = connect_with_retry(endpoint, RECONNECT_TIMEOUT)?;
    let partial = read_request(103, 0, 0x04, 0, SNAPSHOT_INPUT_REGISTER_COUNT);
    stream.write_all(&partial[..9])?;
    stream.shutdown(Shutdown::Both)?;
    drop(stream);
    verify_fresh_connection(endpoint)?;
    println!("suite=protocol status=pass");
    Ok(())
}

/// Repeatedly close and reopen otherwise healthy sessions.
///
/// Args:
///   endpoint: Board TCP endpoint in `host:port` form.
fn lifecycle_suite(endpoint: &str) -> TestResult {
    println!("suite=lifecycle status=running");
    for iteration in 0..4 {
        let mut client = ModbusClient::connect_retry(endpoint)?;
        let snapshot = client.read_snapshot((iteration * 31) as u8)?;
        println!(
            "lifecycle_iteration={iteration} snapshot_id={} margin_ns={}",
            snapshot.metrics.id, snapshot.metrics.cycle_time_margin_ns
        );
        client.stream.shutdown(Shutdown::Both)?;
        drop(client);
    }
    verify_fresh_connection(endpoint)?;
    println!("suite=lifecycle status=pass");
    Ok(())
}

/// Exercise both supported rate endpoints and complete map-width operations.
///
/// Args:
///   endpoint: Board TCP endpoint in `host:port` form.
fn endpoint_suite(endpoint: &str) -> TestResult {
    println!("suite=endpoints status=running");
    let mut client = ModbusClient::connect_retry(endpoint)?;
    run_rate_endpoint(
        &mut client,
        MIN_CYCLE_RATE_HZ as f32,
        240,
        Duration::from_secs(4),
        101,
        0,
    )?;
    run_rate_endpoint(
        &mut client,
        MODBUS_MAX_CYCLE_RATE_HZ,
        u16::MAX,
        Duration::from_secs(5),
        203,
        0,
    )?;

    // Restore documented defaults before an orderly close. A reconnect would
    // also restore them, but this checks the complete timing FC16 once more.
    client.write_timing(10.0, 600, 11)?;
    let holding = client.read_registers(0x03, 0, HOLDING_REGISTER_COUNT, 11)?;
    require_cycle_period(&holding, 100_000_000)?;
    println!("suite=endpoints status=pass");
    Ok(())
}

/// Verify persistent-period, one-shot-phase, and bounded correction behavior.
///
/// Args:
///   endpoint: Board TCP endpoint in `host:port` form.
fn correction_suite(endpoint: &str) -> TestResult {
    const RATE_HZ: f32 = 100.0;
    const DT_NS: i64 = 10_000_000;
    const CORRECTION_LIMIT_NS: i64 = DT_NS / 10;
    const STEP_TOLERANCE_NS: i64 = 100_000;

    println!("suite=corrections status=running");
    let mut client = ModbusClient::connect_retry(endpoint)?;
    client.write_timing(RATE_HZ, 10_000, 41)?;
    client.write_corrections(0, 0, 41)?;

    // An extreme period request persists, but the applied interval remains at
    // the existing +10% clamp. A write occupies the intervening publication
    // cycle, so the first observed span covers one nominal and one corrected
    // interval.
    let baseline = client.read_snapshot(41)?;
    client.write_corrections(i64::MAX, 0, 41)?;
    let period_first = client.read_snapshot(41)?;
    let period_second = client.read_snapshot(41)?;
    let period_write_span_ns = period_first.sample_time_ns - baseline.sample_time_ns;
    let persistent_step_ns = period_second.sample_time_ns - period_first.sample_time_ns;
    require_near(
        "period write span",
        period_write_span_ns,
        2 * DT_NS + CORRECTION_LIMIT_NS,
        STEP_TOLERANCE_NS,
    )?;
    require_near(
        "persistent period step",
        persistent_step_ns,
        DT_NS + CORRECTION_LIMIT_NS,
        STEP_TOLERANCE_NS,
    )?;
    let holding = client.read_registers(0x03, 0, HOLDING_REGISTER_COUNT, 41)?;
    require_corrections(&holding, i64::MAX, 0)?;

    // Clear the persistent term before taking a phase baseline. The minimum
    // phase request applies to exactly one following interval and then reads
    // back as zero.
    client.write_corrections(0, 0, 41)?;
    let phase_baseline = client.read_snapshot(41)?;
    client.write_corrections(0, i64::MIN, 41)?;
    let phase_first = client.read_snapshot(41)?;
    let phase_second = client.read_snapshot(41)?;
    let phase_write_span_ns = phase_first.sample_time_ns - phase_baseline.sample_time_ns;
    let phase_reset_step_ns = phase_second.sample_time_ns - phase_first.sample_time_ns;
    require_near(
        "phase write span",
        phase_write_span_ns,
        2 * DT_NS - CORRECTION_LIMIT_NS,
        STEP_TOLERANCE_NS,
    )?;
    require_near(
        "phase reset step",
        phase_reset_step_ns,
        DT_NS,
        STEP_TOLERANCE_NS,
    )?;
    let holding = client.read_registers(0x03, 0, HOLDING_REGISTER_COUNT, 41)?;
    require_corrections(&holding, 0, 0)?;

    println!(
        "period_write_span_ns={period_write_span_ns} persistent_step_ns={persistent_step_ns} phase_write_span_ns={phase_write_span_ns} phase_reset_step_ns={phase_reset_step_ns}"
    );

    // Exercise the shortest allowed interval at the supported 500 Hz endpoint
    // under continuous complete-snapshot reads.
    run_rate_endpoint(
        &mut client,
        MODBUS_MAX_CYCLE_RATE_HZ,
        u16::MAX,
        Duration::from_secs(3),
        41,
        i64::MIN,
    )?;

    client.write_corrections(0, 0, 41)?;
    client.write_timing(10.0, 600, 41)?;
    println!("suite=corrections status=pass");
    Ok(())
}

/// Fill the peer receive window, then verify bounded recovery through reconnect.
///
/// Unlike the two-request protocol check, this suite creates a large finite
/// pipeline while the host does not consume responses, forcing the board to
/// retain a response under TCP transmit backpressure instead of spinning or
/// accepting unbounded work.
///
/// Args:
///   endpoint: Board TCP endpoint in `host:port` form.
fn backpressure_suite(endpoint: &str) -> TestResult {
    println!("suite=backpressure status=running");
    let mut client = ModbusClient::connect_retry(endpoint)?;
    client.write_timing(MODBUS_MAX_CYCLE_RATE_HZ, u16::MAX, 19)?;

    // Linux doubles small SO_RCVBUF requests internally. The effective buffer
    // is still intentionally much smaller than this finite response burst.
    SockRef::from(&client.stream).set_recv_buffer_size(256)?;
    let mut burst = Vec::with_capacity(12 * 8_192);
    for index in 0..8_192_u16 {
        burst.extend_from_slice(&read_request(
            index,
            0,
            0x04,
            0,
            SNAPSHOT_INPUT_REGISTER_COUNT,
        ));
    }
    client
        .stream
        .set_write_timeout(Some(Duration::from_secs(2)))?;
    let mut accepted_by_host = match client.stream.write(&burst) {
        Ok(count) => count,
        Err(error) if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => 0,
        Err(error) => return Err(error.into()),
    };
    if accepted_by_host < 12 {
        return Err("host accepted no complete backpressure request".into());
    }
    let trailing = accepted_by_host % 12;
    if trailing != 0 {
        let completion = 12 - trailing;
        client
            .stream
            .write_all(&burst[accepted_by_host..accepted_by_host + completion])?;
        accepted_by_host += completion;
    }
    thread::sleep(Duration::from_secs(2));

    // Drain every complete request accepted by the host. This proves the same
    // connection resumes after its receive window reopens and exposes margin
    // data from the stressed interval before the reconnect check.
    let response_count = accepted_by_host / 12;
    let mut min_margin_ns = i64::MAX;
    let mut deadline_misses = 0_usize;
    let mut margins_ns = Vec::with_capacity(response_count);
    for index in 0..response_count {
        let response = read_one_adu(&mut client.stream)?;
        validate_response_header(&response, index as u16, 0, 0x04)?;
        let registers = parse_read_registers(&response, 0x04, SNAPSHOT_INPUT_REGISTER_COUNT)?;
        let snapshot = snapshot_from_input_registers(&registers)
            .map_err(|error| format!("invalid backpressure snapshot: {error:?}"))?;
        let margin_ns = snapshot.metrics.cycle_time_margin_ns;
        min_margin_ns = min_margin_ns.min(margin_ns);
        deadline_misses += usize::from(margin_ns < 0);
        margins_ns.push(margin_ns);
    }
    margins_ns.sort_unstable();
    let margin_p01_ns = margins_ns[(margins_ns.len() - 1) / 100];
    let holding = client.read_registers(0x03, 0, HOLDING_REGISTER_COUNT, 0)?;
    let loss_counter = holding[usize::from(HOLDING_LOSS_OF_CONTACT_COUNTER)];
    println!(
        "backpressure_request_bytes_accepted={accepted_by_host} drained_responses={response_count} deadline_misses={deadline_misses} min_margin_ns={min_margin_ns} margin_p01_ns={margin_p01_ns} loss_counter={loss_counter}",
    );
    drop(client);
    verify_fresh_connection(endpoint)?;
    println!("suite=backpressure status=pass");
    Ok(())
}

/// Verify the documented one-minute default application-contact timeout.
///
/// Args:
///   endpoint: Board TCP endpoint in `host:port` form.
fn timeout_suite(endpoint: &str) -> TestResult {
    println!("suite=timeout status=running wait_s=62");
    let mut client = ModbusClient::connect_retry(endpoint)?;
    client.read_snapshot(23)?;
    thread::sleep(DEFAULT_TIMEOUT_TEST);

    // A TCP reset is not retransmitted, so a completely passive peer can miss
    // it and retain a locally stale socket. Prove the application timeout by
    // opening a replacement connection while the old descriptor is still live.
    // The board has only one TCP socket, making that a direct state-exit check.
    let mut replacement = ModbusClient::connect_retry(endpoint)?;
    let snapshot = replacement.read_snapshot(29)?;
    println!("timeout_reconnect_snapshot_id={}", snapshot.metrics.id);

    // Once the stale peer transmits again, it must not be able to resume the
    // timed-out session. Either the write or the following read observes reset.
    let stale_request = read_request(0x7a7a, 23, 0x04, 0, 1);
    match client.stream.write_all(&stale_request) {
        Ok(()) => expect_peer_disconnect(&mut client.stream)?,
        Err(error)
            if matches!(
                error.kind(),
                ErrorKind::ConnectionReset | ErrorKind::ConnectionAborted | ErrorKind::BrokenPipe
            ) => {}
        Err(error) => return Err(format!("unexpected stale-stream write result: {error}").into()),
    }
    println!("suite=timeout status=pass");
    Ok(())
}

/// Collect sustained synchronized FC23 control roundtrips at one publishing rate.
///
/// Args:
///   client: Connected single-outstanding-request client.
///   rate_hz: Requested publishing rate in `Hz`; the oversampled ADC-filter
///     cutoff is `0.4` times this value.
///   timeout_cycles: Application-contact timeout in publishing cycles.
///   duration: Minimum sustained test duration.
///   unit: Arbitrary Unit Identifier to echo.
///   period_delta_ns: Persistent requested period correction, in `ns`.
///
/// Returns:
///   `Ok(())` after the complete timed endpoint run passes its snapshot,
///   timing-margin, and loss-of-contact checks.
fn run_rate_endpoint(
    client: &mut ModbusClient,
    rate_hz: f32,
    timeout_cycles: u16,
    duration: Duration,
    unit: u8,
    period_delta_ns: i64,
) -> TestResult {
    client.write_timing(rate_hz, timeout_cycles, unit)?;
    client.write_corrections(period_delta_ns, 0, unit)?;
    let expected_period_ns = (1.0e9_f32 / rate_hz + 0.5) as u32;
    let holding = client.read_registers(0x03, 0, HOLDING_REGISTER_COUNT, unit)?;
    require_cycle_period(&holding, expected_period_ns)?;

    // One full-width FC16 covers all 21 writable output registers. Values come
    // from the shared safe default so this hardware stress test cannot energize
    // an output accidentally.
    let safe = holding_registers(&ModbusInitialConfig::default(), 0);
    let safe_outputs =
        &safe[usize::from(HOLDING_PWM_DUTY_FRAC)..usize::from(HOLDING_PERIOD_DELTA_NS)];
    client.write_registers(HOLDING_PWM_DUTY_FRAC, safe_outputs, unit)?;

    let started = Instant::now();
    let mut snapshots = 0_u64;
    let mut first_id = None;
    let mut last_id = 0_u64;
    let mut min_margin_ns = i64::MAX;
    let mut previous_sample_time_ns = None;
    let mut min_sample_step_ns = i64::MAX;
    let mut max_sample_step_ns = i64::MIN;
    // Time and count bounds make a host/network regression terminate cleanly.
    while started.elapsed() < duration && snapshots < 100_000 {
        let snapshot = client.read_write_snapshot(HOLDING_PWM_DUTY_FRAC, safe_outputs, unit)?;
        first_id.get_or_insert(snapshot.metrics.id);
        last_id = snapshot.metrics.id;
        min_margin_ns = min_margin_ns.min(snapshot.metrics.cycle_time_margin_ns);
        if let Some(previous) = previous_sample_time_ns {
            let step_ns = snapshot.sample_time_ns - previous;
            min_sample_step_ns = min_sample_step_ns.min(step_ns);
            max_sample_step_ns = max_sample_step_ns.max(step_ns);
        }
        previous_sample_time_ns = Some(snapshot.sample_time_ns);
        snapshots += 1;
    }
    let elapsed = started.elapsed();
    if snapshots == 0 || elapsed < duration {
        return Err(format!("endpoint {rate_hz} Hz did not complete its sustained window").into());
    }
    let holding = client.read_registers(0x03, 0, HOLDING_REGISTER_COUNT, unit)?;
    let loss_counter = holding[usize::from(HOLDING_LOSS_OF_CONTACT_COUNTER)];
    println!(
        "rate_hz={rate_hz} period_delta_ns={period_delta_ns} duration_s={:.3} reads={} reads_per_s={:.1} first_id={} last_id={} min_margin_ns={} min_sample_step_ns={} max_sample_step_ns={} loss_counter={}",
        elapsed.as_secs_f64(),
        snapshots,
        snapshots as f64 / elapsed.as_secs_f64(),
        first_id.unwrap_or(0),
        last_id,
        min_margin_ns,
        min_sample_step_ns,
        max_sample_step_ns,
        loss_counter,
    );
    Ok(())
}

/// Encode one signed 64-bit value in most-significant-register-first order.
fn i64_registers(value: i64) -> [u16; 4] {
    let bits = value as u64;
    [
        (bits >> 48) as u16,
        (bits >> 32) as u16,
        (bits >> 16) as u16,
        bits as u16,
    ]
}

/// Require the holding map to contain the requested timing corrections.
fn require_corrections(registers: &[u16], period_delta_ns: i64, phase_delta_ns: i64) -> TestResult {
    let period_start = usize::from(HOLDING_PERIOD_DELTA_NS);
    let phase_start = usize::from(HOLDING_PHASE_DELTA_NS);
    if registers.get(period_start..period_start + 4) != Some(&i64_registers(period_delta_ns))
        || registers.get(phase_start..phase_start + 4) != Some(&i64_registers(phase_delta_ns))
    {
        return Err("holding timing corrections do not match the requested values".into());
    }
    Ok(())
}

/// Require one measured acquisition-time span within a symmetric tolerance.
fn require_near(name: &str, actual_ns: i64, expected_ns: i64, tolerance_ns: i64) -> TestResult {
    if actual_ns.abs_diff(expected_ns) > tolerance_ns as u64 {
        return Err(format!(
            "{name} is {actual_ns} ns, expected {expected_ns} +/- {tolerance_ns} ns"
        )
        .into());
    }
    Ok(())
}

/// Small sequential Modbus/TCP client used by the hardware test suites.
struct ModbusClient {
    /// Connected stream with finite I/O timeouts.
    stream: TcpStream,
    /// Next transaction identifier, wrapping naturally as specified by Modbus/TCP.
    next_transaction: u16,
}

impl ModbusClient {
    /// Connect once and initialize finite socket timeouts.
    fn connect(endpoint: &str) -> TestResult<Self> {
        Ok(Self {
            stream: connect_once(endpoint)?,
            next_transaction: 1,
        })
    }

    /// Connect with a finite retry window for the board's relisten transition.
    fn connect_retry(endpoint: &str) -> TestResult<Self> {
        Ok(Self {
            stream: connect_with_retry(endpoint, RECONNECT_TIMEOUT)?,
            next_transaction: 1,
        })
    }

    /// Allocate one transaction identifier.
    fn take_transaction(&mut self) -> u16 {
        let value = self.next_transaction;
        self.next_transaction = self.next_transaction.wrapping_add(1);
        value
    }

    /// Read and decode one register range.
    fn read_registers(
        &mut self,
        function: u8,
        address: u16,
        count: u16,
        unit: u8,
    ) -> TestResult<Vec<u16>> {
        let transaction = self.take_transaction();
        let request = read_request(transaction, unit, function, address, count);
        let response = self.exchange(&request, transaction, unit, function)?;
        parse_read_registers(&response, function, count)
    }

    /// Read and validate one complete coherent engineering snapshot.
    fn read_snapshot(&mut self, unit: u8) -> TestResult<OperatingSnapshot> {
        let registers = self.read_registers(
            0x04,
            SNAPSHOT_INPUT_START,
            SNAPSHOT_INPUT_REGISTER_COUNT,
            unit,
        )?;
        snapshot_from_input_registers(&registers)
            .map_err(|error| format!("invalid snapshot: {error:?}").into())
    }

    /// Atomically write one control block and return this cycle's snapshot.
    ///
    /// Args:
    ///   write_address: Zero-based first writable holding-register address.
    ///   values: Complete writable field block with shape `(write_count,)`.
    ///   unit: Arbitrary one-byte Modbus Unit Identifier to echo.
    ///
    /// Returns:
    ///   Validated beginning-of-cycle engineering snapshot from the FC23 read.
    fn read_write_snapshot(
        &mut self,
        write_address: u16,
        values: &[u16],
        unit: u8,
    ) -> TestResult<OperatingSnapshot> {
        let transaction = self.take_transaction();
        let request = read_write_request(
            transaction,
            unit,
            HOLDING_SNAPSHOT_START,
            HOLDING_SNAPSHOT_REGISTER_COUNT,
            write_address,
            values,
        )?;
        let response = self.exchange(
            &request,
            transaction,
            unit,
            MODBUS_READ_WRITE_MULTIPLE_REGISTERS_FUNCTION,
        )?;
        let registers = parse_read_registers(
            &response,
            MODBUS_READ_WRITE_MULTIPLE_REGISTERS_FUNCTION,
            HOLDING_SNAPSHOT_REGISTER_COUNT,
        )?;
        snapshot_from_input_registers(&registers)
            .map_err(|error| format!("invalid FC23 snapshot: {error:?}").into())
    }

    /// Write one contiguous holding-register span and validate its echo.
    fn write_registers(&mut self, address: u16, values: &[u16], unit: u8) -> TestResult {
        let transaction = self.take_transaction();
        let request = write_request(transaction, 0, unit, address, values)?;
        let response = self.exchange(&request, transaction, unit, 0x10)?;
        if response.len() != 12
            || u16::from_be_bytes([response[8], response[9]]) != address
            || usize::from(u16::from_be_bytes([response[10], response[11]])) != values.len()
        {
            return Err("invalid FC16 write echo".into());
        }
        Ok(())
    }

    /// Write the complete three-register timing configuration.
    fn write_timing(&mut self, rate_hz: f32, timeout_cycles: u16, unit: u8) -> TestResult {
        let bits = rate_hz.to_bits();
        self.write_registers(0, &[(bits >> 16) as u16, bits as u16, timeout_cycles], unit)
    }

    /// Atomically replace the persistent period and one-shot phase requests.
    fn write_corrections(
        &mut self,
        period_delta_ns: i64,
        phase_delta_ns: i64,
        unit: u8,
    ) -> TestResult {
        let mut values = [0_u16; 8];
        values[..4].copy_from_slice(&i64_registers(period_delta_ns));
        values[4..].copy_from_slice(&i64_registers(phase_delta_ns));
        self.write_registers(HOLDING_PERIOD_DELTA_NS, &values, unit)
    }

    /// Exchange one request and require its transaction, unit, and function echo.
    fn exchange(
        &mut self,
        request: &[u8],
        transaction: u16,
        unit: u8,
        function: u8,
    ) -> TestResult<Vec<u8>> {
        self.stream.write_all(request)?;
        let response = read_one_adu(&mut self.stream)?;
        validate_response_header(&response, transaction, unit, function)?;
        Ok(response)
    }
}

/// Normalize a bare host to the standard Modbus/TCP port.
fn normalize_endpoint(endpoint: String) -> String {
    if endpoint.contains(':') {
        endpoint
    } else {
        format!("{endpoint}:502")
    }
}

/// Open one stream with finite read/write timeouts.
fn connect_once(endpoint: &str) -> TestResult<TcpStream> {
    let address = endpoint
        .to_socket_addrs()?
        .next()
        .ok_or("endpoint resolved to no socket address")?;
    let stream = TcpStream::connect_timeout(&address, IO_TIMEOUT)?;
    stream.set_read_timeout(Some(IO_TIMEOUT))?;
    stream.set_write_timeout(Some(IO_TIMEOUT))?;
    stream.set_nodelay(true)?;
    Ok(stream)
}

/// Retry connection for a finite board state-machine transition window.
fn connect_with_retry(endpoint: &str, timeout: Duration) -> TestResult<TcpStream> {
    let deadline = Instant::now() + timeout;
    let mut last_error = None;
    for _ in 0..80 {
        match connect_once(endpoint) {
            Ok(stream) => return Ok(stream),
            Err(error) => last_error = Some(error),
        }
        if Instant::now() >= deadline {
            break;
        }
        thread::sleep(Duration::from_millis(100));
    }
    Err(format!(
        "could not reconnect within {timeout:?}: {}",
        last_error
            .map(|error| error.to_string())
            .unwrap_or_else(|| "no attempt completed".to_owned())
    )
    .into())
}

/// Build one Modbus/TCP ADU from its MBAP and PDU components.
fn adu(transaction: u16, protocol: u16, unit: u8, pdu: &[u8]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(7 + pdu.len());
    bytes.extend_from_slice(&transaction.to_be_bytes());
    bytes.extend_from_slice(&protocol.to_be_bytes());
    bytes.extend_from_slice(&((pdu.len() + 1) as u16).to_be_bytes());
    bytes.push(unit);
    bytes.extend_from_slice(pdu);
    bytes
}

/// Build one FC03/FC04 read ADU.
fn read_request(transaction: u16, unit: u8, function: u8, address: u16, count: u16) -> Vec<u8> {
    let address = address.to_be_bytes();
    let count = count.to_be_bytes();
    adu(
        transaction,
        0,
        unit,
        &[function, address[0], address[1], count[0], count[1]],
    )
}

/// Build one bounded FC16 write ADU.
fn write_request(
    transaction: u16,
    protocol: u16,
    unit: u8,
    address: u16,
    values: &[u16],
) -> TestResult<Vec<u8>> {
    if values.is_empty() || values.len() > usize::from(MODBUS_MAX_WRITE_REGISTERS) {
        return Err("FC16 register count must be in 1..=123".into());
    }
    let address = address.to_be_bytes();
    let count = (values.len() as u16).to_be_bytes();
    let mut pdu = Vec::with_capacity(6 + values.len() * 2);
    pdu.extend_from_slice(&[0x10, address[0], address[1], count[0], count[1]]);
    pdu.push((values.len() * 2) as u8);
    for value in values {
        pdu.extend_from_slice(&value.to_be_bytes());
    }
    Ok(adu(transaction, protocol, unit, &pdu))
}

/// Build one bounded FC23 read/write holding-register ADU.
///
/// Args:
///   transaction: Modbus/TCP transaction identifier.
///   unit: One-byte Modbus Unit Identifier.
///   read_address: Zero-based first holding-register read address.
///   read_count: Read span length, in 16-bit registers.
///   write_address: Zero-based first holding-register write address.
///   values: Write-register values with shape `(write_count,)`.
///
/// Returns:
///   Complete network-byte-order ADU, or an error when either count is outside
///   the standard FC23 bounds.
///
/// References:
///   \[1\] Modbus Organization, *MODBUS Application Protocol Specification
///   V1.1b3*, section 6.17, 2012.
fn read_write_request(
    transaction: u16,
    unit: u8,
    read_address: u16,
    read_count: u16,
    write_address: u16,
    values: &[u16],
) -> TestResult<Vec<u8>> {
    if read_count == 0 || read_count > MODBUS_MAX_READ_REGISTERS {
        return Err("FC23 read register count must be in 1..=125".into());
    }
    if values.is_empty() {
        return Err("FC23 write register count must be nonzero".into());
    }
    if values.len() > usize::from(MODBUS_MAX_READ_WRITE_WRITE_REGISTERS) {
        return Err("FC23 write register count must be in 1..=121".into());
    }

    let read_address = read_address.to_be_bytes();
    let read_count = read_count.to_be_bytes();
    let write_address = write_address.to_be_bytes();
    let write_count = (values.len() as u16).to_be_bytes();
    let mut pdu = Vec::with_capacity(10 + values.len() * 2);
    pdu.extend_from_slice(&[
        MODBUS_READ_WRITE_MULTIPLE_REGISTERS_FUNCTION,
        read_address[0],
        read_address[1],
        read_count[0],
        read_count[1],
        write_address[0],
        write_address[1],
        write_count[0],
        write_count[1],
        (values.len() * 2) as u8,
    ]);
    for value in values {
        pdu.extend_from_slice(&value.to_be_bytes());
    }
    Ok(adu(transaction, 0, unit, &pdu))
}

/// Read exactly one MBAP-length-delimited response ADU.
fn read_one_adu(stream: &mut TcpStream) -> TestResult<Vec<u8>> {
    let mut prefix = [0_u8; 6];
    stream.read_exact(&mut prefix)?;
    let length = usize::from(u16::from_be_bytes([prefix[4], prefix[5]]));
    let total_len = 6 + length;
    if !(8..=256).contains(&total_len) {
        return Err(format!("invalid response ADU length {total_len}").into());
    }
    let mut response = vec![0_u8; total_len];
    response[..6].copy_from_slice(&prefix);
    stream.read_exact(&mut response[6..])?;
    Ok(response)
}

/// Validate common response fields and reject unexpected exception responses.
fn validate_response_header(
    response: &[u8],
    transaction: u16,
    unit: u8,
    function: u8,
) -> TestResult {
    if response.len() < 8
        || u16::from_be_bytes([response[0], response[1]]) != transaction
        || u16::from_be_bytes([response[2], response[3]]) != 0
        || response[6] != unit
    {
        return Err("response MBAP fields do not match request".into());
    }
    if response[7] == function | 0x80 {
        if response.len() < 9 {
            return Err("truncated Modbus exception response".into());
        }
        return Err(format!("unexpected Modbus exception code {:#04x}", response[8]).into());
    }
    if response[7] != function {
        return Err(format!("unexpected response function {:#04x}", response[7]).into());
    }
    Ok(())
}

/// Decode one FC03/FC04 register payload.
fn parse_read_registers(response: &[u8], function: u8, count: u16) -> TestResult<Vec<u16>> {
    let byte_count = usize::from(count) * 2;
    if response.len() != 9 + byte_count
        || response[7] != function
        || usize::from(response[8]) != byte_count
    {
        return Err("invalid read-register response length".into());
    }
    Ok(response[9..]
        .chunks_exact(2)
        .map(|pair| u16::from_be_bytes([pair[0], pair[1]]))
        .collect())
}

/// Require one exact standard Modbus exception response.
fn validate_exception(
    response: &[u8],
    transaction: u16,
    unit: u8,
    function: u8,
    exception: u8,
) -> TestResult {
    if response.len() != 9
        || u16::from_be_bytes([response[0], response[1]]) != transaction
        || u16::from_be_bytes([response[2], response[3]]) != 0
        || response[6] != unit
        || response[7] != function | 0x80
        || response[8] != exception
    {
        return Err(format!("unexpected exception response {response:02x?}").into());
    }
    Ok(())
}

/// Send a nonempty arbitrary PDU and require an exception without losing the stream.
///
/// Args:
///   client: Connected sequential test client.
///   pdu: Request protocol data unit with shape `(pdu_len,)` in network byte order.
///   exception: Expected Modbus exception code.
fn expect_exception(client: &mut ModbusClient, pdu: &[u8], exception: u8) -> TestResult {
    let transaction = client.take_transaction();
    let function = *pdu.first().ok_or("exception test PDU must not be empty")?;
    let request = adu(transaction, 0, 0, pdu);
    client.stream.write_all(&request)?;
    let response = read_one_adu(&mut client.stream)?;
    validate_exception(&response, transaction, 0, function, exception)
}

/// Send an FC03/FC04 request and require an exact exception.
fn expect_exception_for_read(
    client: &mut ModbusClient,
    function: u8,
    address: u16,
    count: u16,
    exception: u8,
) -> TestResult {
    let transaction = client.take_transaction();
    client
        .stream
        .write_all(&read_request(transaction, 0, function, address, count))?;
    let response = read_one_adu(&mut client.stream)?;
    validate_exception(&response, transaction, 0, function, exception)
}

/// Require the board to close one structurally unrecoverable stream.
fn expect_peer_disconnect(stream: &mut TcpStream) -> TestResult {
    let mut byte = [0_u8; 1];
    match stream.read(&mut byte) {
        Ok(0) => Ok(()),
        Err(error)
            if matches!(
                error.kind(),
                ErrorKind::ConnectionReset
                    | ErrorKind::ConnectionAborted
                    | ErrorKind::BrokenPipe
                    | ErrorKind::UnexpectedEof
            ) =>
        {
            Ok(())
        }
        Ok(_) => Err("malformed connection returned application data".into()),
        Err(error) => Err(format!("peer did not disconnect: {error}").into()),
    }
}

/// Connect after a state transition and complete one full valid snapshot read.
fn verify_fresh_connection(endpoint: &str) -> TestResult {
    let mut client = ModbusClient::connect_retry(endpoint)?;
    client.read_snapshot(255)?;
    Ok(())
}

/// Require the read-only period diagnostic to equal the configured value.
fn require_cycle_period(registers: &[u16], expected_ns: u32) -> TestResult {
    let index = usize::from(HOLDING_CYCLE_PERIOD_NS);
    if registers.len() < index + 2 {
        return Err("holding-register response omits the cycle period".into());
    }
    let actual = u32::from_be_bytes([
        (registers[index] >> 8) as u8,
        registers[index] as u8,
        (registers[index + 1] >> 8) as u8,
        registers[index + 1] as u8,
    ]);
    if actual != expected_ns {
        return Err(format!("cycle period is {actual} ns, expected {expected_ns} ns").into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_have_consistent_mbap_lengths() {
        for request in [
            read_request(7, 255, 0x04, 0, SNAPSHOT_INPUT_REGISTER_COUNT),
            write_request(8, 0, 3, 6, &[0; 21]).unwrap(),
            write_request(9, 0, 3, HOLDING_PERIOD_DELTA_NS, &[0; 8]).unwrap(),
            read_write_request(
                10,
                37,
                HOLDING_SNAPSHOT_START,
                HOLDING_SNAPSHOT_REGISTER_COUNT,
                HOLDING_PWM_DUTY_FRAC,
                &[0; 21],
            )
            .unwrap(),
        ] {
            assert_eq!(
                usize::from(u16::from_be_bytes([request[4], request[5]])),
                request.len() - 6
            );
        }
    }

    #[test]
    fn read_write_request_uses_standard_fc23_field_order() {
        let request = read_write_request(0x1234, 0xab, 0x0100, 79, 6, &[0x1122, 0x3344]).unwrap();
        assert_eq!(request[7], MODBUS_READ_WRITE_MULTIPLE_REGISTERS_FUNCTION);
        assert_eq!(&request[8..17], &[0x01, 0x00, 0x00, 79, 0, 6, 0, 2, 4]);
        assert_eq!(&request[17..], &[0x11, 0x22, 0x33, 0x44]);
    }

    #[test]
    fn read_write_request_enforces_protocol_count_bounds() {
        assert!(read_write_request(1, 0, 0, 0, 6, &[0]).is_err());
        assert!(read_write_request(1, 0, 0, MODBUS_MAX_READ_REGISTERS + 1, 6, &[0]).is_err());
        assert!(read_write_request(1, 0, 0, 1, 6, &[]).is_err());
        assert!(
            read_write_request(
                1,
                0,
                0,
                1,
                6,
                &vec![0; usize::from(MODBUS_MAX_READ_WRITE_WRITE_REGISTERS) + 1],
            )
            .is_err()
        );
    }

    #[test]
    fn cycle_period_uses_big_endian_register_order() {
        let mut registers = vec![0_u16; HOLDING_REGISTER_COUNT as usize];
        let index = HOLDING_CYCLE_PERIOD_NS as usize;
        registers[index] = 0x1234;
        registers[index + 1] = 0x5678;
        require_cycle_period(&registers, 0x1234_5678).unwrap();
    }

    #[test]
    fn truncated_exception_is_rejected_without_indexing_past_response() {
        let response = [0, 7, 0, 0, 0, 2, 255, 0x84];
        assert!(validate_response_header(&response, 7, 255, 0x04).is_err());
    }

    #[test]
    fn cycle_period_requires_both_registers() {
        assert!(require_cycle_period(&[0; 4], 0).is_err());
    }

    #[test]
    fn signed_correction_registers_are_most_significant_first() {
        assert_eq!(
            i64_registers(0x0123_4567_89ab_cdef),
            [0x0123, 0x4567, 0x89ab, 0xcdef]
        );
        assert_eq!(i64_registers(-2), [0xffff, 0xffff, 0xffff, 0xfffe]);
    }
}
