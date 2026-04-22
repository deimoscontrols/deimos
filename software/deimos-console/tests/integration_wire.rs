//! Integration test: wire-format end-to-end — Schema + Row flow through receiver.
//!
//! Verifies that unit labels declared on a calc-produced channel survive the full
//! encode → UDP multicast → receive → decode pipeline. The test mimics what the
//! `ReportingDispatcher` sends on the wire and confirms `receiver::spawn` produces
//! correctly-decoded `Schema` and `Row` values on the channel it returns.
//!
//! # What is NOT tested here
//!
//! GUI axis-label rendering is visually verified during actual `hootl_with_console`
//! operator runs (two-terminal invocation documented in
//! `software/deimos/examples/hootl_with_console.rs` and in the top-level `CLAUDE.md`).
//!
//! # Multicast-via-loopback on macOS
//!
//! macOS routes multicast packets addressed to 239.255.0.x back through the loopback
//! interface when sender and receiver are on the same host. This behaviour is relied on
//! here. The test port (29574) is chosen to avoid colliding with the production default
//! (29573) and any other process that might be running.

use std::net::{Ipv4Addr, SocketAddrV4, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use deimos::controller::context::ControllerCtx;
use deimos::dispatcher::{Dispatcher, ReportingDispatcher, ReportingMessage};
use deimos_console::config::DeimosConsoleConfig;
use deimos_console::receiver;

/// Multicast group used only for this test (avoids collision with production default 239.255.0.1).
const TEST_MULTICAST_GROUP: Ipv4Addr = Ipv4Addr::new(239, 255, 42, 1);
/// Port used only for this test.
const TEST_PORT: u16 = 29574;

/// Simulated `RtdPt100` channel names and units — representative of a calc-produced channel
/// with a declared unit.  The unit-label invariant we are proving end-to-end is:
///   `Some("K")` survives encode → multicast → decode without truncation or loss.
const CHANNEL_NAMES: &[&str] = &["rtd_resistance_ohm", "rtd_temperature_K"];
const CHANNEL_UNITS: &[Option<&str>] = &[Some("ohm"), Some("K")];

fn make_config() -> DeimosConsoleConfig {
    DeimosConsoleConfig {
        multicast_group: TEST_MULTICAST_GROUP,
        port: TEST_PORT,
        interface: Some(Ipv4Addr::LOCALHOST),
        window_seconds: 30.0,
        staleness_threshold_secs: 2.0,
        panels: vec![],
        forensic_log_path: None,
    }
}

/// Build and return a sender socket bound to loopback with the multicast loop-back flag set.
///
/// `IP_MULTICAST_LOOP` is enabled by default on most platforms, but we set it explicitly
/// to make the test self-documenting.
fn make_sender_socket() -> UdpSocket {
    use socket2::{Domain, Protocol, Socket, Type};

    let raw =
        Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP)).expect("sender: create socket");

    // Enable multicast loopback so the packet is delivered to receivers on the same host.
    raw.set_multicast_loop_v4(true)
        .expect("sender: set_multicast_loop_v4");

    // Set TTL=1 — just like the ReportingDispatcher, to avoid leaking onto the LAN.
    raw.set_multicast_ttl_v4(1)
        .expect("sender: set_multicast_ttl_v4");

    // Route outbound multicast through the loopback interface.
    raw.set_multicast_if_v4(&Ipv4Addr::LOCALHOST)
        .expect("sender: set_multicast_if_v4");

    // Bind to INADDR_ANY; multicast senders must not bind to a specific unicast address
    // or macOS rejects the sendto with EADDRNOTAVAIL.
    let bind_addr: std::net::SocketAddr = SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 0).into();
    raw.bind(&bind_addr.into()).expect("sender: bind");

    raw.into()
}

/// Encode a `ReportingMessage` into a fresh `Vec<u8>`.
fn encode(msg: &ReportingMessage) -> Vec<u8> {
    let mut buf = Vec::new();
    msg.encode_into(&mut buf).expect("encode");
    buf
}

#[test]
fn schema_and_row_flow_through_receiver() {
    let config = make_config();

    // --- Spawn the receiver under test ---
    //
    // receiver::spawn joins the multicast group on the loopback interface, starts a background
    // thread, and returns a crossbeam Receiver<ReportingMessage>.
    let (_tx, rx, _handle) = receiver::spawn(&config).expect("receiver::spawn");

    // Give the receiver thread a moment to join the multicast group before we send.
    thread::sleep(Duration::from_millis(50));

    // --- Spawn sender thread ---
    let sender_handle = thread::spawn(move || {
        let socket = make_sender_socket();
        let dest = SocketAddrV4::new(TEST_MULTICAST_GROUP, TEST_PORT);

        // Build Schema — mirrors what ReportingDispatcher::init stores.
        let schema = ReportingMessage::Schema {
            channel_names: CHANNEL_NAMES.iter().map(|s| s.to_string()).collect(),
            channel_units: CHANNEL_UNITS
                .iter()
                .map(|u| u.map(str::to_string))
                .collect(),
            monotonic_epoch_ns: 1_713_530_000_000_000_000_u64,
            is_session_end: false,
        };

        // Build Row — one cycle of values: resistance = 108.5 ohm, temperature = 301.2 K.
        let row = ReportingMessage::Row {
            seq: 0,
            timestamp: 0.05,
            system_time: "2026-04-19T14:00:00.000000000Z".to_string(),
            values: vec![108.5, 301.2],
        };

        socket.send_to(&encode(&schema), dest).expect("send Schema");
        // Small delay so the receiver has time to forward Schema before Row arrives.
        thread::sleep(Duration::from_millis(10));
        socket.send_to(&encode(&row), dest).expect("send Row");
    });

    // --- Assert Schema received within 2 s ---
    let received_schema = rx
        .recv_timeout(Duration::from_secs(2))
        .expect("Schema not received within 2 s");

    match &received_schema {
        ReportingMessage::Schema {
            channel_names,
            channel_units,
            ..
        } => {
            let expected_names: Vec<String> = CHANNEL_NAMES.iter().map(|s| s.to_string()).collect();
            let expected_units: Vec<Option<String>> = CHANNEL_UNITS
                .iter()
                .map(|u| u.map(str::to_string))
                .collect();

            assert_eq!(
                channel_names, &expected_names,
                "Schema channel names must survive encode → multicast → decode"
            );
            assert_eq!(
                channel_units, &expected_units,
                "Unit labels (e.g. \"K\" for temperature) must survive encode → multicast → decode"
            );

            // Spot-check the specific unit we care about for the RtdPt100 temperature channel.
            assert_eq!(
                channel_units[1],
                Some("K".to_string()),
                "temperature channel unit must be \"K\""
            );
        }
        other => panic!("expected Schema, got: {other:?}"),
    }

    // --- Assert Row received within 2 s ---
    let received_row = rx
        .recv_timeout(Duration::from_secs(2))
        .expect("Row not received within 2 s");

    match &received_row {
        ReportingMessage::Row { seq, values, .. } => {
            assert_eq!(*seq, 0, "sequence number must be 0");
            assert_eq!(values.len(), 2, "Row must have 2 channel values");
            assert!(
                (values[0] - 108.5).abs() < 1e-9,
                "resistance value must round-trip exactly"
            );
            assert!(
                (values[1] - 301.2).abs() < 1e-9,
                "temperature value must round-trip exactly"
            );
        }
        other => panic!("expected Row, got: {other:?}"),
    }

    sender_handle.join().expect("sender thread panicked");
}

// ---------------------------------------------------------------------------
// Late-joiner test
// ---------------------------------------------------------------------------
//
// The realtime-reporting spec's core value proposition for operators is:
//   "A viewer that joins mid-run MUST discover channel metadata within
//    `schema_period`, without waiting for the next controller run."
//
// `schema_and_row_flow_through_receiver` above only exercises the wire format
// by hand-sending packets. This test drives the real `ReportingDispatcher`:
// it starts the dispatcher, lets enough time pass that the initial Schema
// is emitted into a void (no receiver listening), then attaches a receiver
// and asserts a Schema arrives within the re-emit window.
//
// If this test regresses, the fix to `last_schema_sent`-on-`WouldBlock`
// (reporting/mod.rs:311-313) or the periodic-emission loop itself is broken.

/// Distinct multicast group + port — must not collide with the hand-rolled
/// wire test above (239.255.42.1:29574) or with the production default
/// (239.255.0.1:29573).
const LATE_JOIN_GROUP: Ipv4Addr = Ipv4Addr::new(239, 255, 42, 2);
const LATE_JOIN_PORT: u16 = 29575;

/// Shorter than the production 2 s default so the test runs quickly.
const TEST_SCHEMA_PERIOD: Duration = Duration::from_millis(500);

#[test]
fn late_joiner_receives_schema_within_reemit_window() {
    // --- Build and init the dispatcher (no receiver listening yet) ---
    let mut dispatcher = ReportingDispatcher::new(
        LATE_JOIN_GROUP,
        LATE_JOIN_PORT,
        Some(Ipv4Addr::LOCALHOST),
        TEST_SCHEMA_PERIOD,
    );

    let mut ctx = ControllerCtx::default();
    ctx.channel_units = vec![Some("V".to_string()), None];
    let channel_names: Vec<String> = vec!["voltage_v".to_string(), "counter".to_string()];

    dispatcher
        .init(&ctx, &channel_names, 0)
        .expect("ReportingDispatcher::init");

    // --- Drive consume() on a background thread at 50 Hz ---
    //
    // Schema re-emission is driven from inside consume(), so the loop must keep
    // running for the duration of the test.
    let shutdown = Arc::new(AtomicBool::new(false));
    let shutdown_clone = Arc::clone(&shutdown);
    let expected_names = channel_names.clone();
    let consume_handle = thread::spawn(move || {
        let start = Instant::now();
        let mut seq = 0u64;
        while !shutdown_clone.load(Ordering::Relaxed) {
            let timestamp = start.elapsed().as_nanos() as i64;
            let values = vec![seq as f64 * 0.01, seq as f64];
            let _ = dispatcher.consume(SystemTime::now(), timestamp, values);
            seq += 1;
            thread::sleep(Duration::from_millis(20));
        }
        let _ = dispatcher.terminate();
    });

    // --- Late-joiner window ---
    //
    // First consume() fires at t≈0 and sends Schema #1 into the void.
    // Schema #2 fires at t≈500 ms and also goes to no one.
    // We attach the receiver at t≈800 ms, past both of those emissions.
    // Schema #3 should fire at t≈1000 ms, ~200 ms after attach — well inside
    // our generous 1500 ms budget.
    thread::sleep(Duration::from_millis(800));

    let attach_time = Instant::now();

    let config = DeimosConsoleConfig {
        multicast_group: LATE_JOIN_GROUP,
        port: LATE_JOIN_PORT,
        interface: Some(Ipv4Addr::LOCALHOST),
        window_seconds: 30.0,
        staleness_threshold_secs: 2.0,
        panels: vec![],
        forensic_log_path: None,
    };
    let (_tx, rx, _handle) = receiver::spawn(&config).expect("receiver::spawn");

    // --- Drain until we see a Schema; Rows before that are expected ---
    let timeout = TEST_SCHEMA_PERIOD * 2 + Duration::from_millis(500);
    let deadline = attach_time + timeout;
    let mut got_schema = false;
    while Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        match rx.recv_timeout(remaining) {
            Ok(ReportingMessage::Schema {
                channel_names: names,
                channel_units: units,
                is_session_end,
                ..
            }) => {
                assert!(
                    !is_session_end,
                    "late joiner must not see a session-end Schema mid-run"
                );
                assert_eq!(
                    names, expected_names,
                    "re-emitted Schema must carry the same channel names as init"
                );
                assert_eq!(
                    units,
                    vec![Some("V".to_string()), None],
                    "re-emitted Schema must carry the same channel units as init"
                );
                got_schema = true;
                break;
            }
            Ok(ReportingMessage::Row { .. }) => {
                // Expected: the viewer sees Rows before its first Schema.
                continue;
            }
            Err(_) => break,
        }
    }

    // --- Clean shutdown before the final assertion so the consumer thread
    //     always joins, even on failure. ---
    shutdown.store(true, Ordering::Relaxed);
    consume_handle.join().expect("consumer thread panicked");

    assert!(
        got_schema,
        "late-joining receiver did not see a Schema within {timeout:?} of attach"
    );
}
