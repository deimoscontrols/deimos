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
//! `software/deimos/examples/hootl_with_console.rs`).
//!
//! # Multicast-via-loopback
//!
//! The test sends and receives on the same host. Delivery relies on `IP_MULTICAST_LOOP`
//! being enabled on the sender socket (see [`make_sender_socket`]) so outgoing multicast
//! datagrams are echoed back to receivers on this host, combined with routing the
//! outbound multicast through the loopback interface (`set_multicast_if_v4(LOCALHOST)`).
//! The administratively-scoped group `239.255.42.1` is chosen to avoid colliding with
//! the production default (`239.255.0.1`); the test port (29574) avoids colliding with
//! the production port (29573) and any other process that might be running.

use std::net::{Ipv4Addr, SocketAddrV4, UdpSocket};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
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
        tail_keep_secs: 0.5,
        recovery_settle_secs: 2.0,
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

    let (rx, _handle, _counters) = receiver::spawn(&config).expect("receiver::spawn");

    // Give the receiver thread a moment to join the multicast group before we send.
    thread::sleep(Duration::from_millis(50));

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
                *channel_names, expected_names,
                "Schema channel names must survive encode → multicast → decode"
            );
            assert_eq!(
                *channel_units, expected_units,
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

// Late-joiner test: a viewer that joins mid-run must discover channel metadata within
// `schema_period` without waiting for the next controller run. This test drives the real
// `ReportingDispatcher`, lets the initial Schema emit into a void (no receiver listening),
// then attaches a receiver and asserts a Schema arrives within the re-emit window.

/// Distinct multicast group + port — must not collide with the hand-rolled
/// wire test above (239.255.42.1:29574) or with the production default
/// (239.255.0.1:29573).
const LATE_JOIN_GROUP: Ipv4Addr = Ipv4Addr::new(239, 255, 42, 2);
const LATE_JOIN_PORT: u16 = 29575;

/// Shorter than the production 2 s default so the test runs quickly.
const TEST_SCHEMA_PERIOD: Duration = Duration::from_millis(500);

#[test]
fn late_joiner_receives_schema_within_reemit_window() {
    // Build the dispatcher with no receiver yet listening.
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

    // Schema re-emission is driven from inside consume(), so the loop must keep running for
    // the duration of the test.
    let shutdown = Arc::new(AtomicBool::new(false));
    let shutdown_clone = Arc::clone(&shutdown);
    let expected_names = channel_names.clone();
    let consume_handle = thread::spawn(move || {
        let start = Instant::now();
        let mut seq = 0u64;
        while !shutdown_clone.load(Ordering::Relaxed) {
            let timestamp = start.elapsed().as_nanos() as i64;
            let values = vec![seq as f64 * 0.01, seq as f64];
            dispatcher
                .consume(SystemTime::now(), timestamp, values)
                .expect("ReportingDispatcher::consume must not return Err post-init");
            seq += 1;
            thread::sleep(Duration::from_millis(20));
        }
        dispatcher
            .terminate()
            .expect("ReportingDispatcher::terminate must not return Err");
    });

    // Schema #1 fires at t≈0 and Schema #2 at t≈500 ms — both into the void. Attach the
    // receiver at t≈800 ms; Schema #3 at t≈1000 ms lands ~200 ms after attach, inside the
    // 1500 ms budget.
    thread::sleep(Duration::from_millis(800));

    let attach_time = Instant::now();

    let config = DeimosConsoleConfig {
        multicast_group: LATE_JOIN_GROUP,
        port: LATE_JOIN_PORT,
        interface: Some(Ipv4Addr::LOCALHOST),
        window_seconds: 30.0,
        staleness_threshold_secs: 2.0,
        tail_keep_secs: 0.5,
        recovery_settle_secs: 2.0,
        panels: vec![],
        forensic_log_path: None,
    };
    let (rx, _handle, _counters) = receiver::spawn(&config).expect("receiver::spawn");

    // Drain until we see a Schema; Rows before that are expected.
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

    // Clean shutdown before the final assertion so the consumer thread always joins.
    shutdown.store(true, Ordering::Relaxed);
    consume_handle.join().expect("consumer thread panicked");

    assert!(
        got_schema,
        "late-joining receiver did not see a Schema within {timeout:?} of attach"
    );
}

// Failure-mode tests: the receiver thread must survive arbitrary wire input. The decode-error
// path is rate-limited and logged, but a regression that lets a bad packet poison the channel
// or kill the thread is the kind of failure that would only show in a multi-hour operator run.

const GARBAGE_GROUP: Ipv4Addr = Ipv4Addr::new(239, 255, 42, 3);
const GARBAGE_PORT: u16 = 29576;

const GAP_GROUP: Ipv4Addr = Ipv4Addr::new(239, 255, 42, 4);
const GAP_PORT: u16 = 29577;

const MIXED_GROUP: Ipv4Addr = Ipv4Addr::new(239, 255, 42, 5);
const MIXED_PORT: u16 = 29578;

fn config_for(group: Ipv4Addr, port: u16) -> DeimosConsoleConfig {
    DeimosConsoleConfig {
        multicast_group: group,
        port,
        interface: Some(Ipv4Addr::LOCALHOST),
        window_seconds: 30.0,
        staleness_threshold_secs: 2.0,
        tail_keep_secs: 0.5,
        recovery_settle_secs: 2.0,
        panels: vec![],
        forensic_log_path: None,
    }
}

/// A non-postcard datagram on the multicast group must be silently dropped (postcard decode
/// fails, the receiver logs at most one rate-limited line and continues). The next valid
/// `Schema` must still flow through. Regression: a panic-on-bad-decode or a poisoned channel
/// here would silently kill the receiver thread, leaving the operator console permanently
/// `NoSchemaYet` even though packets are arriving.
#[test]
fn receiver_skips_garbage_packet_and_delivers_valid_followup() {
    let config = config_for(GARBAGE_GROUP, GARBAGE_PORT);
    let (rx, _handle, _counters) = receiver::spawn(&config).expect("receiver::spawn");

    // Let the receiver join the multicast group before sending.
    thread::sleep(Duration::from_millis(50));

    let socket = make_sender_socket();
    let dest = SocketAddrV4::new(GARBAGE_GROUP, GARBAGE_PORT);

    // Garbage payload: bytes that are vanishingly unlikely to parse as a valid `ReportingMessage`.
    let garbage = b"this is not a postcard-encoded ReportingMessage!!!11";
    socket.send_to(garbage, dest).expect("send garbage");

    // Brief gap so the receiver definitely reads and discards the garbage before the Schema.
    thread::sleep(Duration::from_millis(20));

    let schema = ReportingMessage::Schema {
        channel_names: vec!["ch0".to_string()],
        channel_units: vec![Some("V".to_string())],
        monotonic_epoch_ns: 1,
        is_session_end: false,
    };
    socket
        .send_to(&encode(&schema), dest)
        .expect("send valid Schema");

    // The garbage must NOT appear on the channel; the Schema must arrive within a normal window.
    let msg = rx
        .recv_timeout(Duration::from_secs(2))
        .expect("Schema not delivered within 2 s after a garbage packet");
    match msg {
        ReportingMessage::Schema { channel_names, .. } => {
            assert_eq!(channel_names, vec!["ch0".to_string()]);
        }
        other => panic!("expected Schema after garbage, got: {other:?}"),
    }

    // No further messages should be queued — the garbage was dropped, not converted.
    assert!(
        rx.recv_timeout(Duration::from_millis(50)).is_err(),
        "garbage packet must not produce any decoded message"
    );
}

/// A jump in `seq` on the wire must not stop the receiver from delivering the post-gap rows
/// in order. The receiver is opaque to `seq` (gap counting happens downstream in
/// `ConsoleState::process_row`), so the contract here is "channel still delivers FIFO."
#[test]
fn receiver_delivers_rows_in_order_through_gap_in_seq() {
    let config = config_for(GAP_GROUP, GAP_PORT);
    let (rx, _handle, _counters) = receiver::spawn(&config).expect("receiver::spawn");

    thread::sleep(Duration::from_millis(50));

    let socket = make_sender_socket();
    let dest = SocketAddrV4::new(GAP_GROUP, GAP_PORT);

    let schema = ReportingMessage::Schema {
        channel_names: vec!["a".to_string()],
        channel_units: vec![None],
        monotonic_epoch_ns: 1,
        is_session_end: false,
    };
    socket.send_to(&encode(&schema), dest).expect("send Schema");

    // Send seq 0, 1, 5 (gap of 2, 3, 4). Brief pauses keep loopback delivery in order;
    // UDP on loopback is reliable enough for this assertion in practice.
    for seq in [0u64, 1, 5] {
        let row = ReportingMessage::Row {
            seq,
            timestamp: seq as f64 * 0.01,
            system_time: "2026-04-19T14:00:00.000000000Z".to_string(),
            values: vec![seq as f64],
        };
        socket.send_to(&encode(&row), dest).expect("send Row");
        thread::sleep(Duration::from_millis(5));
    }

    // Skip the Schema.
    let first = rx
        .recv_timeout(Duration::from_secs(2))
        .expect("Schema not received");
    assert!(matches!(first, ReportingMessage::Schema { .. }));

    let mut got = Vec::new();
    while got.len() < 3 {
        match rx.recv_timeout(Duration::from_secs(2)) {
            Ok(ReportingMessage::Row { seq, .. }) => got.push(seq),
            Ok(other) => panic!("expected Row, got: {other:?}"),
            Err(e) => panic!("Row not received: {e}"),
        }
    }
    assert_eq!(
        got,
        vec![0, 1, 5],
        "rows must traverse the channel in send order, gap preserved"
    );
}

/// Stress test: a mix of garbage, truncated valid prefixes, and valid messages must result
/// in exactly the valid messages arriving on the channel, in order, with the receiver thread
/// still alive at the end (proven by a final round-trip).
#[test]
fn receiver_filters_garbage_from_mixed_stream_and_stays_alive() {
    let config = config_for(MIXED_GROUP, MIXED_PORT);
    let (rx, _handle, _counters) = receiver::spawn(&config).expect("receiver::spawn");

    thread::sleep(Duration::from_millis(50));

    let socket = make_sender_socket();
    let dest = SocketAddrV4::new(MIXED_GROUP, MIXED_PORT);

    let schema = ReportingMessage::Schema {
        channel_names: vec!["a".to_string(), "b".to_string()],
        channel_units: vec![None, Some("K".to_string())],
        monotonic_epoch_ns: 1,
        is_session_end: false,
    };
    let row0 = ReportingMessage::Row {
        seq: 0,
        timestamp: 0.0,
        system_time: "2026-04-19T14:00:00.000000000Z".to_string(),
        values: vec![1.0, 300.0],
    };
    let row1 = ReportingMessage::Row {
        seq: 1,
        timestamp: 0.01,
        system_time: "2026-04-19T14:00:00.010000000Z".to_string(),
        values: vec![1.1, 300.1],
    };

    let valid_schema = encode(&schema);
    let valid_row0 = encode(&row0);
    let valid_row1 = encode(&row1);

    // Interleave bad and good packets. The bad ones are: random bytes; a half-truncated copy
    // of a valid schema (postcard sees an unexpected end); and zero-length. Each must be
    // rejected without disrupting delivery of the valids.
    let half = &valid_schema[..valid_schema.len() / 2];
    let zero: &[u8] = b"";

    socket
        .send_to(b"\x00\x01\x02garbage", dest)
        .expect("send garbage");
    thread::sleep(Duration::from_millis(5));
    socket.send_to(&valid_schema, dest).expect("send schema");
    thread::sleep(Duration::from_millis(5));
    socket.send_to(half, dest).expect("send truncated");
    thread::sleep(Duration::from_millis(5));
    socket.send_to(&valid_row0, dest).expect("send row0");
    thread::sleep(Duration::from_millis(5));
    socket.send_to(zero, dest).expect("send zero-length");
    thread::sleep(Duration::from_millis(5));
    socket.send_to(&valid_row1, dest).expect("send row1");

    // Collect three messages; nothing else should arrive.
    let mut got = Vec::new();
    for _ in 0..3 {
        let msg = rx
            .recv_timeout(Duration::from_secs(2))
            .expect("expected valid message did not arrive");
        got.push(msg);
    }

    match &got[0] {
        ReportingMessage::Schema { channel_names, .. } => {
            assert_eq!(channel_names, &vec!["a".to_string(), "b".to_string()]);
        }
        other => panic!("expected Schema first, got: {other:?}"),
    }
    match &got[1] {
        ReportingMessage::Row { seq, .. } => assert_eq!(*seq, 0),
        other => panic!("expected Row seq=0, got: {other:?}"),
    }
    match &got[2] {
        ReportingMessage::Row { seq, .. } => assert_eq!(*seq, 1),
        other => panic!("expected Row seq=1, got: {other:?}"),
    }

    // Nothing else is pending.
    assert!(
        rx.recv_timeout(Duration::from_millis(50)).is_err(),
        "no spurious messages must arrive from the garbage packets"
    );

    // Receiver thread liveness check: send a final Schema and confirm delivery. If any of
    // the bad packets had killed the thread, this would time out.
    let final_schema = ReportingMessage::Schema {
        channel_names: vec!["c".to_string()],
        channel_units: vec![None],
        monotonic_epoch_ns: 2,
        is_session_end: false,
    };
    socket
        .send_to(&encode(&final_schema), dest)
        .expect("send final schema");
    let final_msg = rx
        .recv_timeout(Duration::from_secs(2))
        .expect("final schema did not arrive — receiver thread may have died");
    match final_msg {
        ReportingMessage::Schema { channel_names, .. } => {
            assert_eq!(channel_names, vec!["c".to_string()]);
        }
        other => panic!("expected final Schema, got: {other:?}"),
    }
}
