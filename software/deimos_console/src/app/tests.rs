use super::*;
use std::net::Ipv4Addr;

/// Build a minimal `DeimosConsoleConfig` with a tight staleness threshold for testing.
fn test_config(staleness_threshold_secs: f64) -> DeimosConsoleConfig {
    DeimosConsoleConfig {
        multicast_group: Ipv4Addr::new(239, 255, 0, 1),
        port: 5000,
        interface: None,
        window_seconds: 30.0,
        staleness_threshold_secs,
        tail_keep_secs: 0.5,
        recovery_settle_secs: 2.0,
        panels: Vec::new(),
        columns: 1,
        forensic_log_path: None,
    }
}

/// Create a `ConsoleState` with a disconnected receiver channel, suitable for unit tests
/// that exercise state transitions without needing a real receiver thread. Returns the
/// state directly (no wrapper / no processor thread) so tests can inspect and mutate fields.
fn test_app(staleness_threshold_secs: f64) -> ConsoleState {
    let (_tx, rx) = crossbeam_channel::unbounded();
    ConsoleState::new(
        test_config(staleness_threshold_secs),
        rx,
        ReceiverCounters::default(),
    )
}

/// Create a `ConsoleState` plus its live `Sender`, so tests can inject messages on the
/// channel and then drive `drain_messages` on the state.
fn test_app_with_tx(
    staleness_threshold_secs: f64,
) -> (ConsoleState, crossbeam_channel::Sender<ReportingMessage>) {
    let (tx, rx) = crossbeam_channel::unbounded();
    let app = ConsoleState::new(
        test_config(staleness_threshold_secs),
        rx,
        ReceiverCounters::default(),
    );
    (app, tx)
}

/// Before any Schema is received, `connection_health` must return `NoSchemaYet`.
#[test]
fn health_no_schema_yet() {
    let app = test_app(2.0);
    assert_eq!(app.connection_health(), ConnectionHealth::NoSchemaYet);
}

/// After a Schema arrives but before any Row, `connection_health` must return `Stale`.
#[test]
fn health_stale_before_first_row() {
    let mut app = test_app(2.0);
    app.schema = Some(SchemaState::new(
        vec!["ch0".to_string()],
        vec![None],
        false,
        0,
    ));
    assert_eq!(app.connection_health(), ConnectionHealth::Stale);
}

/// A session-end `Schema` must surface as `SessionEnded` regardless of how recent
/// the last `Row` was *or* whether a stall just fired, so an operator can immediately
/// distinguish a clean shutdown from a silent stall or a recently-bumpy stream.
#[test]
fn health_session_ended_overrides_fresh() {
    let mut app = test_app(2.0);
    app.schema = Some(SchemaState::new(
        vec!["ch0".to_string()],
        vec![None],
        true,
        0,
    ));
    app.last_row_received_at = Some(Instant::now());
    // Also assert the Recovering state cannot mask a session-end signal.
    app.last_stall_at = Some(Instant::now());
    assert_eq!(app.connection_health(), ConnectionHealth::SessionEnded);
}

/// After a Schema and an immediate Row, `connection_health` must return `Fresh`, then
/// transition to `Stale` once the staleness threshold has elapsed. With no stall ever
/// recorded, the new `Recovering` state must NOT appear in either snapshot — it only
/// activates after a real stall fires.
#[test]
fn health_fresh_then_stale_after_threshold() {
    // Use a tight threshold so the test completes quickly.
    let threshold_ms = 50u64;
    let mut app = test_app(threshold_ms as f64 / 1000.0);

    // Simulate Schema arrival.
    app.schema = Some(SchemaState::new(
        vec!["ch0".to_string()],
        vec![None],
        false,
        0,
    ));

    // Simulate Row arrival.
    app.last_row_received_at = Some(Instant::now());

    // No stall has ever fired, so `last_stall_at` stays None — the precedence rules
    // must skip Recovering and land on Fresh.
    assert_eq!(app.last_stall_at, None);
    assert_eq!(
        app.connection_health(),
        ConnectionHealth::Fresh,
        "expected Fresh immediately after Row arrival"
    );

    // Wait longer than the staleness threshold and assert the transition to Stale.
    // Stale must outrank Recovering, so even if a stall had been recorded the answer
    // would still be Stale once rows stop flowing.
    std::thread::sleep(Duration::from_millis(threshold_ms * 2));

    assert_eq!(
        app.connection_health(),
        ConnectionHealth::Stale,
        "expected Stale after staleness threshold elapsed"
    );
}

/// When the receiver thread exits, its `Sender` is dropped and the bounded channel
/// reports `Disconnected`. The drain loop must flip the sticky `receiver_dead` flag
/// so `connection_health` resolves to `ReceiverDead` instead of pretending the link
/// is still alive.
#[test]
fn receiver_dead_flag_set_when_sender_drops() {
    let (mut app, tx) = test_app_with_tx(2.0);

    // Channel is connected and empty; drain leaves the flag clear.
    app.drain_messages();
    assert!(
        !app.receiver_dead,
        "receiver_dead must stay clear while the sender is alive"
    );
    assert_eq!(app.connection_health(), ConnectionHealth::NoSchemaYet);

    // Drop the sender to simulate the receiver thread exiting.
    drop(tx);

    app.drain_messages();
    assert!(
        app.receiver_dead,
        "receiver_dead must be set after the sender is dropped"
    );
    assert_eq!(
        app.connection_health(),
        ConnectionHealth::ReceiverDead,
        "ReceiverDead must override every other health state"
    );
}

/// A `Row` whose channel-value count disagrees with the active schema is silently
/// dropped (the alternative — drawing it — would produce visually-wrong plots).
/// `schema_drift_drops` counts these so an operator can see drift in the status bar.
#[test]
fn schema_drift_drops_row_with_mismatched_channel_count() {
    let (mut app, tx) = test_app_with_tx(2.0);

    // Send a Schema declaring two channels.
    tx.send(ReportingMessage::Schema {
        channel_names: vec!["a".to_string(), "b".to_string()],
        channel_units: vec![None, None],
        monotonic_epoch_ns: 1,
        is_session_end: false,
    })
    .expect("send Schema");

    // Send a Row carrying three values — schema drift.
    tx.send(ReportingMessage::Row {
        seq: 0,
        timestamp: 0.0,
        system_time: "2026-04-26T00:00:00Z".to_string(),
        values: vec![1.0, 2.0, 3.0],
    })
    .expect("send Row");

    app.drain_messages();

    assert_eq!(
        app.schema_drift_drops, 1,
        "schema-drift drop counter must increment for a mismatched-arity Row"
    );
    // The drop must not pollute the channel ring buffers.
    assert!(
        app.channel_data.is_empty() || app.channel_data.values().all(|buf| buf.is_empty()),
        "channel ring buffers must remain empty after a schema-drift drop"
    );
}

/// Format a `SystemTime` as an RFC3339 nanosecond string the way the controller serializes
/// `system_time` on the wire. Matches `process_row`'s parser exactly so the lag computation
/// reproduces what production code sees.
fn rfc3339_nanos(t: SystemTime) -> String {
    let dt: chrono::DateTime<chrono::Utc> = t.into();
    dt.to_rfc3339_opts(chrono::SecondsFormat::Nanos, true)
}

/// A drained batch that mixes a stale backlog with one live row must trigger exactly one
/// stall event, clear pre-stall samples down to the `tail_keep_secs` cutoff, and leave a
/// NaN sentinel marking the discontinuity.
///
/// The test simulates the realistic flow: a first drain primes the per-channel ring
/// buffer with two pre-stall samples, then a second drain delivers a backlog of rows with
/// stale `system_time` (older than `staleness_threshold_secs`) plus one live row. The
/// data-staleness signal should fire on the second drain, the stall handler should evict
/// the pre-stall samples that fall below `freshest_ts - tail_keep_secs`, and the NaN
/// sentinel should be inserted as the visible discontinuity.
#[test]
fn stall_clears_pre_stall_samples_and_emits_sentinel() {
    let (mut app, tx) = test_app_with_tx(0.5);
    // Use a tail that is small enough relative to the controller-timestamp gap below that
    // every pre-stall sample will fall below the cutoff and be evicted.
    app.config.tail_keep_secs = 0.1;
    let now = SystemTime::now();

    // --- Drain #1: prime channel_data with two "pre-stall" samples. ---
    // The system_time is fresh so this drain is treated as live (no stall fires).
    tx.send(ReportingMessage::Schema {
        channel_names: vec!["a".to_string()],
        channel_units: vec![None],
        monotonic_epoch_ns: 1,
        is_session_end: false,
    })
    .expect("send Schema");
    for (seq, ts) in [(0u64, 0.0_f64), (1, 0.05)] {
        tx.send(ReportingMessage::Row {
            seq,
            timestamp: ts,
            system_time: rfc3339_nanos(now),
            values: vec![ts],
        })
        .expect("send pre-stall Row");
    }
    app.drain_messages();
    assert_eq!(
        app.stalls_detected, 0,
        "first drain with fresh data must not trigger a stall"
    );
    let buf = app
        .channel_data
        .get("a")
        .expect("channel a must be populated after the priming drain");
    assert_eq!(buf.len(), 2, "two pre-stall samples should be present");

    // --- Drain #2: deliver a backlog of stale rows + one live row. ---
    // Stale rows carry a system_time older than staleness_threshold_secs, so the
    // data-staleness signal fires.
    let stale_st = rfc3339_nanos(now - Duration::from_secs(10));
    let live_st = rfc3339_nanos(SystemTime::now());
    for (seq, ts) in [(2u64, 1.0_f64), (3, 1.5), (4, 2.0)] {
        tx.send(ReportingMessage::Row {
            seq,
            timestamp: ts,
            system_time: stale_st.clone(),
            values: vec![ts],
        })
        .expect("send stale backlog Row");
    }
    let live_ts = 5.0_f64;
    tx.send(ReportingMessage::Row {
        seq: 5,
        timestamp: live_ts,
        system_time: live_st,
        values: vec![live_ts],
    })
    .expect("send live Row");

    app.drain_messages();

    assert_eq!(
        app.stalls_detected, 1,
        "data-staleness signal should fire exactly once on the catch-up drain"
    );
    assert!(
        app.stale_rows_evicted >= 2,
        "both pre-stall priming samples should have been evicted (got {})",
        app.stale_rows_evicted
    );

    let buf = app
        .channel_data
        .get("a")
        .expect("channel a must still exist after the stall");
    // The two priming samples at t={0.0, 0.05} and the stale catch-up rows at
    // t={1.0, 1.5, 2.0} were all below `freshest_ts - tail_keep_secs` (= 4.9) at
    // apply-pending-stall time, so they must have been evicted from the visible buffer.
    assert!(
        !buf.iter().any(|&(t, _)| t < 4.9),
        "all samples below the tail-keep cutoff (4.9) must be evicted, got {:?}",
        buf.iter().collect::<Vec<_>>()
    );
    assert!(
        buf.iter().any(|&(_, v)| v.is_nan()),
        "the discontinuity sentinel (NaN) must be present in the buffer"
    );
    // The live row must still be present so the operator sees the post-stall trace.
    assert!(
        buf.iter()
            .any(|&(t, v)| !v.is_nan() && (t - live_ts).abs() < 1e-9),
        "the live row at t={live_ts} must remain after stall handling"
    );

    // Sentinel-position invariant (regression for "stale catch-up rows still rendered"):
    // egui_plot draws points in buffer-insertion order, so any finite point inserted AFTER
    // the sentinel that has a smaller timestamp would render the line back through stale
    // data. Assert that no finite point in the buffer satisfies
    // `t < sentinel_ts && index > sentinel_index`.
    let sentinel_index = buf
        .iter()
        .position(|&(_, v)| v.is_nan())
        .expect("sentinel must exist by the assertion above");
    let sentinel_ts = buf[sentinel_index].0;
    for (i, &(t, v)) in buf.iter().enumerate() {
        if v.is_nan() {
            continue;
        }
        if i > sentinel_index {
            assert!(
                t >= sentinel_ts,
                "finite point at index {i} (t={t}) sits after the sentinel (ts={sentinel_ts}) but has an older timestamp; egui_plot would render the line back through stale data"
            );
        }
    }
}

/// A stall that fired within `recovery_settle_secs` must surface as `Recovering`, even
/// while rows are otherwise fresh. Once the settle window has elapsed, the same state
/// must collapse back to `Fresh`. The test ages `last_stall_at` by subtracting from
/// `Instant::now()` so the assertion is deterministic without sleeping.
#[test]
fn health_recovering_then_fresh_after_settle_window() {
    let settle_secs = 2.0_f64;
    let mut app = test_app(60.0);
    app.config.recovery_settle_secs = settle_secs;

    // Schema received and a Row just arrived — without the stall mark this would be Fresh.
    app.schema = Some(SchemaState::new(
        vec!["ch0".to_string()],
        vec![None],
        false,
        0,
    ));
    app.last_row_received_at = Some(Instant::now());
    assert_eq!(
        app.connection_health(),
        ConnectionHealth::Fresh,
        "without a recorded stall the baseline must be Fresh"
    );

    // Drive a stall: set `last_stall_at = now`. Indicator must flip to Recovering.
    app.last_stall_at = Some(Instant::now());
    assert_eq!(
        app.connection_health(),
        ConnectionHealth::Recovering,
        "Recovering must surface immediately after a stall is recorded"
    );

    // Refresh the row mark too (so the row is still fresh) and age `last_stall_at`
    // past the settle window. Indicator must collapse back to Fresh without sleeping.
    app.last_row_received_at = Some(Instant::now());
    let aged = Instant::now()
        .checked_sub(Duration::from_secs_f64(settle_secs * 2.0))
        .expect("aging Instant by 2x settle window must not underflow");
    app.last_stall_at = Some(aged);
    assert_eq!(
        app.connection_health(),
        ConnectionHealth::Fresh,
        "Recovering must collapse to Fresh once the settle window has elapsed"
    );
}

/// A contiguous live stream — fresh `system_time` on every row, frequent drains — must
/// never flag a stall. The data-staleness check needs row lag above
/// `staleness_threshold_secs`, which never holds for a healthy stream.
#[test]
fn contiguous_live_stream_does_not_trigger_stall() {
    let (mut app, tx) = test_app_with_tx(0.5);

    tx.send(ReportingMessage::Schema {
        channel_names: vec!["a".to_string()],
        channel_units: vec![None],
        monotonic_epoch_ns: 1,
        is_session_end: false,
    })
    .expect("send Schema");
    app.drain_messages();
    assert_eq!(
        app.stalls_detected, 0,
        "schema-only drain must not flag a stall"
    );

    // Drive five contiguous drains, each with a freshly-stamped row, with a tiny pause
    // between drains. Row lag stays well under the 0.5 s threshold throughout.
    for seq in 0u64..5 {
        tx.send(ReportingMessage::Row {
            seq,
            timestamp: seq as f64 * 0.01,
            system_time: rfc3339_nanos(SystemTime::now()),
            values: vec![seq as f64],
        })
        .expect("send live Row");
        app.drain_messages();
        std::thread::sleep(Duration::from_millis(10));
    }

    assert_eq!(
        app.stalls_detected, 0,
        "a contiguous live stream must never trigger stall detection"
    );
    assert_eq!(
        app.stale_rows_evicted, 0,
        "no rows should be evicted by stall handling on a healthy stream"
    );
}

/// A repaint gap with fresh data behind it MUST NOT trigger a stall. The inverted
/// counterpart to the deleted `empty_drain_does_not_advance_last_drain_at` regression:
/// pre-fix, a long wall-clock gap between drains (simulating a paused UI thread)
/// armed the `UiPause` arm of `detect_stall` and the next non-empty drain reported
/// `stalls_detected == 1`. Post-fix, the wall-clock arm is gone — the processor thread
/// keeps buffers fresh independently of the repaint cycle — so a gap followed by a row
/// whose `system_time` is fresh MUST resolve as a normal live drain: no stall, no NaN
/// sentinel, no buffer eviction.
///
/// The test deliberately does NOT use `last_drain_at` (it no longer exists) to construct
/// the aged scenario. The relevant input axis is "wall-clock time passed between drains
/// while the data path stayed healthy" — modelled here by a short `sleep` and a fresh
/// `system_time` on the post-gap row. The data-staleness arm only fires on row lag, so
/// time alone, with no row staleness, must not flag anything.
#[test]
fn repaint_gap_with_fresh_data_does_not_trigger_stall() {
    let (mut app, tx) = test_app_with_tx(0.5);

    // Prime with Schema + one fresh row so the buffer has a real pre-gap sample.
    tx.send(ReportingMessage::Schema {
        channel_names: vec!["a".to_string()],
        channel_units: vec![None],
        monotonic_epoch_ns: 1,
        is_session_end: false,
    })
    .expect("send Schema");
    tx.send(ReportingMessage::Row {
        seq: 0,
        timestamp: 0.0,
        system_time: rfc3339_nanos(SystemTime::now()),
        values: vec![0.0],
    })
    .expect("send priming Row");
    app.drain_messages();
    assert_eq!(
        app.stalls_detected, 0,
        "priming drain with fresh data must not flag a stall"
    );

    // Simulate the repaint-pause scenario: wall-clock time passes (longer than the
    // staleness threshold) with no intervening drain activity. Pre-fix this is exactly
    // what `UiPause` armed off — even though the data path was healthy underneath.
    std::thread::sleep(Duration::from_millis(600));

    // Now a fresh row arrives — its `system_time` is current, so row lag stays well
    // below the threshold. The data-staleness arm has nothing to fire on; the deleted
    // wall-clock arm would have fired here.
    tx.send(ReportingMessage::Row {
        seq: 1,
        timestamp: 1.0,
        system_time: rfc3339_nanos(SystemTime::now()),
        values: vec![1.0],
    })
    .expect("send post-gap Row");
    app.drain_messages();

    assert_eq!(
        app.stalls_detected, 0,
        "a repaint gap with fresh data behind it must not flag a stall"
    );
    let buf = app
        .channel_data
        .get("a")
        .expect("channel a must still be populated after the post-gap drain");
    assert!(
        !buf.iter().any(|&(_, v)| v.is_nan()),
        "no discontinuity sentinel may be inserted when only a repaint gap occurred"
    );
    assert!(
        app.pending_stall.is_none(),
        "no deferred sentinel may be recorded when only a repaint gap occurred"
    );
}

/// The buffer-processor thread MUST drain channel messages without any `egui::App::update`
/// call. Build a real `DeimosConsoleApp` (which spawns the processor thread), inject a
/// `Schema` + `Row` over the channel, and poll the shared state under the lock for up to
/// ~1 s. The row must appear in `channel_data` purely under the processor thread's work —
/// this is the integration contract that earns the wrapper's existence.
///
/// The explicit `drop(app)` at the end of the test exercises [`DeimosConsoleApp::drop`]:
/// the shutdown atomic flips and `join` waits for the processor thread, so a hung join
/// surfaces as a hung test rather than a silently-leaked thread.
#[test]
fn processor_thread_drains_messages_without_egui_update() {
    let (tx, rx) = crossbeam_channel::unbounded();
    let app = DeimosConsoleApp::new(test_config(2.0), rx, ReceiverCounters::default());

    tx.send(ReportingMessage::Schema {
        channel_names: vec!["a".to_string()],
        channel_units: vec![None],
        monotonic_epoch_ns: 1,
        is_session_end: false,
    })
    .expect("send Schema");
    tx.send(ReportingMessage::Row {
        seq: 0,
        timestamp: 0.0,
        system_time: rfc3339_nanos(SystemTime::now()),
        values: vec![1.5],
    })
    .expect("send Row");

    // Poll the shared state for up to ~1 s. The processor thread cadence is 8 ms, so even
    // accounting for scheduling jitter the row should land within a handful of ticks; the
    // 1 s ceiling is a generous fail-fast bound rather than the expected case.
    let deadline = Instant::now() + Duration::from_secs(1);
    loop {
        {
            let state = app
                .state
                .lock()
                .expect("deimos-console state mutex poisoned");
            if let Some(buf) = state.channel_data.get("a")
                && buf.iter().any(|&(_, v)| (v - 1.5).abs() < 1e-9)
            {
                break;
            }
        }
        assert!(
            Instant::now() < deadline,
            "processor thread did not drain Schema+Row from the channel within 1 s",
        );
        std::thread::sleep(Duration::from_millis(20));
    }

    // Explicit drop so the shutdown-and-join path is part of the test body, not implicit
    // RAII at the closing brace — a hung join is a hung test, not a leaked thread.
    drop(app);
}

/// Apply a stall while frozen: the operator paused specifically to inspect history, so
/// `apply_stall` MUST NOT mutate the per-channel ring buffers (no eviction, no NaN
/// sentinel). The stall counter and `last_stall_at` MUST still advance so the per-tick
/// telemetry is accurate and the connection-health indicator surfaces `Recovering`. The
/// pending-stall slot MUST be populated so the deferred sentinel can materialize on the
/// first non-frozen drain after unfreeze (see
/// `frozen_stall_emits_deferred_sentinel_on_unfreeze` for that half).
#[test]
fn stall_while_frozen_preserves_buffers_but_records_event() {
    let mut app = test_app(2.0);
    app.config.tail_keep_secs = 0.5;

    // Seed channel "a" with a few historical samples and engage freeze.
    let mut buf = VecDeque::new();
    for (t, v) in [(0.0, 1.0_f64), (1.0, 2.0), (2.0, 3.0)] {
        buf.push_back((t, v));
    }
    app.channel_data.insert("a".to_string(), buf);
    app.frozen = true;
    app.frozen_xmax = Some(2.0);

    let snapshot: Vec<(f64, f64)> = app
        .channel_data
        .get("a")
        .expect("channel a was just inserted")
        .iter()
        .copied()
        .collect();

    // freshest_ts sits well past frozen_xmax; pre-fix this would evict every historical
    // sample (cutoff = 60.0 - 0.5 = 59.5, so every t < 59.5 would be popped).
    app.apply_stall(StallEvent {
        trigger: StallTrigger::DataStaleness,
        freshest_ts: 60.0,
    });

    assert_eq!(
        app.stalls_detected, 1,
        "stalls_detected must increment even while frozen"
    );
    assert!(
        app.last_stall_at.is_some(),
        "last_stall_at must be set so unfreeze surfaces Recovering"
    );
    assert_eq!(
        app.stale_rows_evicted, 0,
        "no rows may be evicted while frozen"
    );
    assert!(
        app.pending_stall.is_some(),
        "frozen stall must record a pending sentinel so unfreeze can emit it"
    );

    let after: Vec<(f64, f64)> = app
        .channel_data
        .get("a")
        .expect("channel a must still exist after a frozen stall")
        .iter()
        .copied()
        .collect();
    assert_eq!(
        after, snapshot,
        "frozen channel buffer must be byte-identical after apply_stall: no eviction, no NaN sentinel (the sentinel is recorded as pending, not pushed)"
    );
}

/// A stall that fires while the view is frozen MUST eventually surface its discontinuity
/// sentinel — the spec requires the visible discontinuity, with no freeze exemption. The
/// sentinel is deferred at apply time and materialized on the first non-frozen drain
/// after unfreeze (also serves as a regression for the buffer-mutation skip that hid the
/// stall entirely pre-fix).
#[test]
fn frozen_stall_emits_deferred_sentinel_on_unfreeze() {
    let (mut app, tx) = test_app_with_tx(0.5);
    app.config.tail_keep_secs = 0.5;
    app.schema = Some(SchemaState::new(
        vec!["a".to_string()],
        vec![None],
        false,
        0,
    ));

    // Seed channel "a" with a few historical samples that all sit above the eventual
    // tail-keep cutoff (so they survive eviction) and engage freeze.
    let mut buf = VecDeque::new();
    for (t, v) in [(9.6_f64, 96.0_f64), (9.7, 97.0), (9.8, 98.0)] {
        buf.push_back((t, v));
    }
    app.channel_data.insert("a".to_string(), buf);
    app.frozen = true;
    app.frozen_xmax = Some(9.8);

    // Stall fires while frozen — pending_stall is recorded, channel buffer untouched.
    app.apply_stall(StallEvent {
        trigger: StallTrigger::DataStaleness,
        freshest_ts: 10.0,
    });
    assert!(
        app.pending_stall.is_some(),
        "pending_stall must be set after a frozen stall"
    );
    assert!(
        app.channel_data
            .get("a")
            .expect("channel a present")
            .iter()
            .all(|&(_, v)| !v.is_nan()),
        "no sentinel may be inserted into the channel buffer while frozen"
    );

    // Operator unfreezes and a fresh row arrives. The drain runs `apply_pending_stall`
    // after the second pass, materializing the sentinel.
    app.frozen = false;
    app.frozen_xmax = None;
    tx.send(ReportingMessage::Row {
        seq: 0,
        timestamp: 10.05,
        system_time: rfc3339_nanos(SystemTime::now()),
        values: vec![10.05],
    })
    .expect("send post-unfreeze Row");
    app.drain_messages();

    assert!(
        app.pending_stall.is_none(),
        "pending_stall must be cleared once the deferred sentinel has been applied"
    );
    let buf = app
        .channel_data
        .get("a")
        .expect("channel a must still exist after unfreeze drain");
    assert!(
        buf.iter().any(|&(_, v)| v.is_nan()),
        "the deferred sentinel must materialize on the first non-frozen drain"
    );
    // Sentinel position invariant: every finite point inserted after the sentinel must
    // have `t >= sentinel_ts`. (Without this, the rendered line draws back through stale
    // data — see `stall_clears_pre_stall_samples_and_emits_sentinel`.)
    let sentinel_index = buf
        .iter()
        .position(|&(_, v)| v.is_nan())
        .expect("sentinel exists by previous assertion");
    let sentinel_ts = buf[sentinel_index].0;
    for (i, &(t, v)) in buf.iter().enumerate() {
        if !v.is_nan() && i > sentinel_index {
            assert!(
                t >= sentinel_ts,
                "finite point at index {i} (t={t}) sits after the sentinel (ts={sentinel_ts}) but has an older timestamp"
            );
        }
    }
}

/// `build_render_snapshot` MUST capture per-panel point data so the UI thread can release
/// the shared lock before [`Plot::show`] runs. This pins the contract: the snapshot owns
/// its data (no borrow into `ConsoleState`) and reflects the live `channel_data`.
#[test]
fn render_snapshot_captures_per_panel_points_for_lock_free_render() {
    use crate::config::PanelConfig;

    let mut app = test_app(2.0);
    app.config.panels = vec![PanelConfig {
        title: "voltages".to_string(),
        channels: vec!["a".to_string(), "missing".to_string()],
    }];
    app.schema = Some(SchemaState::new(
        vec!["a".to_string()],
        vec![Some("V".to_string())],
        false,
        0,
    ));
    let mut buf = VecDeque::new();
    for (t, v) in [(0.0_f64, 1.0_f64), (0.5, 2.0)] {
        buf.push_back((t, v));
    }
    app.channel_data.insert("a".to_string(), buf);
    app.frozen = true;
    app.frozen_xmax = Some(0.5);

    let snapshot = app.build_render_snapshot();

    // Outer state copied verbatim so the post-lock render closure has everything it needs.
    assert!(snapshot.frozen);
    assert_eq!(snapshot.frozen_xmax, Some(0.5));
    assert_eq!(snapshot.window_seconds, app.config.window_seconds);
    assert_eq!(snapshot.columns, 1);
    assert_eq!(snapshot.panels.len(), 1);

    let panel = &snapshot.panels[0];
    assert_eq!(panel.title, "voltages");
    assert_eq!(panel.y_label.as_deref(), Some("V"));
    // Channel "a" is in the live ring buffer; channel "missing" is filtered out.
    assert_eq!(panel.series.len(), 1);
    let (ch_name, points) = &panel.series[0];
    assert_eq!(ch_name, "a");
    assert_eq!(points, &[[0.0, 1.0], [0.5, 2.0]]);
}

#[test]
fn render_snapshot_captures_panel_column_count() {
    let mut app = test_app(2.0);
    app.config.columns = 2;

    let snapshot = app.build_render_snapshot();

    assert_eq!(snapshot.columns, 2);
}

#[test]
fn render_snapshot_clamps_zero_columns_to_one() {
    let mut app = test_app(2.0);
    app.config.columns = 0;

    let snapshot = app.build_render_snapshot();

    assert_eq!(snapshot.columns, 1);
}

/// The processor thread MUST keep draining the receiver channel while the UI thread is
/// off rendering plots. Simulate the post-snapshot half of `render_ui` by holding a
/// `RenderSnapshot` (without the lock) for longer than the processor cadence and asserting
/// new rows still land in the live ring buffer in the meantime. This is the regression
/// contract that motivates the snapshot refactor — without it, the processor would block
/// on the lock for the entire `Plot::show` duration.
#[test]
fn processor_drains_while_ui_thread_holds_snapshot_without_lock() {
    use crate::config::PanelConfig;

    let (tx, rx) = crossbeam_channel::unbounded();
    let mut config = test_config(2.0);
    config.panels = vec![PanelConfig {
        title: "p".to_string(),
        channels: vec!["a".to_string()],
    }];
    let app = DeimosConsoleApp::new(config, rx, ReceiverCounters::default());

    // Prime the schema so any subsequent rows are processed (not buffered as pending).
    tx.send(ReportingMessage::Schema {
        channel_names: vec!["a".to_string()],
        channel_units: vec![None],
        monotonic_epoch_ns: 1,
        is_session_end: false,
    })
    .expect("send Schema");
    tx.send(ReportingMessage::Row {
        seq: 0,
        timestamp: 0.0,
        system_time: rfc3339_nanos(SystemTime::now()),
        values: vec![1.0],
    })
    .expect("send first Row");

    // Wait until the processor has drained the seed schema + row (so we know
    // schema is in place before the snapshot grab).
    let deadline = Instant::now() + Duration::from_secs(1);
    loop {
        {
            let state = app
                .state
                .lock()
                .expect("deimos-console state mutex poisoned");
            if state
                .channel_data
                .get("a")
                .is_some_and(|buf| !buf.is_empty())
            {
                break;
            }
        }
        assert!(
            Instant::now() < deadline,
            "processor did not drain the seed Schema+Row within 1 s"
        );
        std::thread::sleep(Duration::from_millis(10));
    }

    // Build a snapshot and drop the lock — this models the UI thread's transition from
    // `build_render_snapshot` (under lock) to `render_panels` (no lock).
    let _snapshot = {
        let state = app
            .state
            .lock()
            .expect("deimos-console state mutex poisoned");
        state.build_render_snapshot()
    };

    // Inject more rows AFTER releasing the lock. If the processor thread were blocked on
    // a held lock (the regression), these rows would sit in the channel buffer untouched.
    for seq in 1..=5u64 {
        tx.send(ReportingMessage::Row {
            seq,
            timestamp: seq as f64 * 0.01,
            system_time: rfc3339_nanos(SystemTime::now()),
            values: vec![seq as f64],
        })
        .expect("send catch-up Row");
    }

    // Hold the snapshot for several processor ticks (cadence is 8 ms; 200 ms is ~25 ticks)
    // to mimic a slow `Plot::show`.
    std::thread::sleep(Duration::from_millis(200));

    // Without the lock held, the processor must have drained every row sent above.
    let buf_len = {
        let state = app
            .state
            .lock()
            .expect("deimos-console state mutex poisoned");
        state
            .channel_data
            .get("a")
            .map(|buf| buf.len())
            .unwrap_or(0)
    };
    assert_eq!(
        buf_len, 6,
        "processor must drain the 5 catch-up rows while the UI thread holds the snapshot lock-free"
    );

    drop(app);
}

/// The per-second tick line MUST advertise every counter the spec lists, because
/// downstream log parsers (and the operators reading them) key off these field names. A
/// rename or accidental drop would silently break the contract — this test pins the key
/// set without depending on stderr capture.
#[test]
fn tick_line_includes_all_required_keys() {
    let app = test_app(2.0);
    let line = app.format_tick_line(42, Duration::from_secs(1), None, 3);

    for key in [
        "rows=",
        "rate=",
        "last_seq=",
        "wire_drops=",
        "recv_drops=",
        "overwritten_frames=",
        "schema_drift_drops=",
        "pending_overflow_drops=",
        "stalls_detected=",
        "stale_rows_evicted=",
        "pending=",
    ] {
        assert!(
            line.contains(key),
            "tick line must include `{key}` field; got: {line}"
        );
    }
}

/// A sequence gap on the wire must increment `dropped_frames_on_wire` by exactly the gap
/// size, the high-water-mark `last_seen_seq` must track the freshest seq seen, and an
/// out-of-order packet (seq below the high-water mark) must not be counted as a gap nor
/// reset the high-water mark. This pins the gap-accounting contract at `app.rs:418-437`.
#[test]
fn wire_drops_increments_on_seq_gap() {
    let (mut app, tx) = test_app_with_tx(60.0);

    tx.send(ReportingMessage::Schema {
        channel_names: vec!["a".to_string()],
        channel_units: vec![None],
        monotonic_epoch_ns: 1,
        is_session_end: false,
    })
    .expect("send Schema");

    // Contiguous run: seq 0..=2. No gaps expected.
    for seq in 0u64..=2 {
        tx.send(ReportingMessage::Row {
            seq,
            timestamp: seq as f64 * 0.05,
            system_time: rfc3339_nanos(SystemTime::now()),
            values: vec![seq as f64],
        })
        .expect("send contiguous Row");
    }
    app.drain_messages();
    assert_eq!(
        app.dropped_frames_on_wire, 0,
        "contiguous run must not register any wire drops"
    );
    assert_eq!(app.last_seen_seq, Some(2));

    // Gap of two: jump from seq 2 to seq 5. Missing seq 3 and 4 — wire_drops += 2.
    tx.send(ReportingMessage::Row {
        seq: 5,
        timestamp: 0.25,
        system_time: rfc3339_nanos(SystemTime::now()),
        values: vec![5.0],
    })
    .expect("send post-gap Row");
    app.drain_messages();
    assert_eq!(
        app.dropped_frames_on_wire, 2,
        "gap from seq=2 to seq=5 must register exactly 2 dropped frames"
    );
    assert_eq!(app.last_seen_seq, Some(5));

    // Next contiguous seq (6) must not bump the counter further.
    tx.send(ReportingMessage::Row {
        seq: 6,
        timestamp: 0.30,
        system_time: rfc3339_nanos(SystemTime::now()),
        values: vec![6.0],
    })
    .expect("send post-gap contiguous Row");
    app.drain_messages();
    assert_eq!(
        app.dropped_frames_on_wire, 2,
        "contiguous follow-up after a gap must not increment the counter"
    );
    assert_eq!(app.last_seen_seq, Some(6));

    // Out-of-order arrival (seq=4 after seq=6): must NOT be counted as a gap, must NOT
    // rewind the high-water mark. A late packet from the wire is its own failure mode,
    // independent of forward sequence gaps.
    tx.send(ReportingMessage::Row {
        seq: 4,
        timestamp: 0.20,
        system_time: rfc3339_nanos(SystemTime::now()),
        values: vec![4.0],
    })
    .expect("send out-of-order Row");
    app.drain_messages();
    assert_eq!(
        app.dropped_frames_on_wire, 2,
        "out-of-order packet must not be counted as a gap"
    );
    assert_eq!(
        app.last_seen_seq,
        Some(6),
        "out-of-order packet must not rewind the high-water seq mark"
    );
}

/// The per-channel ring buffer evicts samples older than `window_seconds` relative to the
/// newest sample's controller timestamp. A row at `t = newest` sets `cutoff = newest -
/// window_seconds`; every prior sample with `t < cutoff` must be popped from the front of
/// the ring (`app.rs:441-460`). This is the visible-window eviction the live plot relies on.
#[test]
fn channel_data_evicts_samples_older_than_window() {
    let (mut app, tx) = test_app_with_tx(60.0);
    app.config.window_seconds = 0.5;

    tx.send(ReportingMessage::Schema {
        channel_names: vec!["a".to_string()],
        channel_units: vec![None],
        monotonic_epoch_ns: 1,
        is_session_end: false,
    })
    .expect("send Schema");

    // Drive 11 rows from t=0.0..=1.0 in 0.1-second steps. By the time the t=1.0 row lands,
    // `cutoff = 1.0 - 0.5 = 0.5`; samples at t<0.5 (i.e. t=0.0, 0.1, 0.2, 0.3, 0.4) must
    // have been evicted, leaving 6 samples (t = 0.5..=1.0).
    let st = rfc3339_nanos(SystemTime::now());
    for i in 0u64..=10 {
        tx.send(ReportingMessage::Row {
            seq: i,
            timestamp: i as f64 * 0.1,
            system_time: st.clone(),
            values: vec![i as f64],
        })
        .expect("send Row");
    }
    app.drain_messages();

    let buf = app
        .channel_data
        .get("a")
        .expect("channel a must be populated");
    assert_eq!(
        buf.len(),
        6,
        "expected 6 samples in window (t=0.5..=1.0), got {} samples at timestamps {:?}",
        buf.len(),
        buf.iter().map(|&(t, _)| t).collect::<Vec<_>>()
    );
    let oldest_ts = buf.front().expect("buffer non-empty").0;
    assert!(
        (oldest_ts - 0.5).abs() < 1e-9,
        "oldest retained sample must sit at t=0.5 (the window cutoff), got {oldest_ts}"
    );
}

/// Rows that arrive before the first `Schema` go into a bounded `pending_rows` queue
/// (capacity `MAX_PENDING_ROWS = 1000`). Beyond the cap, drop-oldest evicts the oldest
/// row and `pending_overflow_drops` ticks. Pins the pre-schema backpressure contract at
/// `app.rs:805-808`.
#[test]
fn pending_overflow_drops_when_rows_arrive_before_schema() {
    let (mut app, tx) = test_app_with_tx(60.0);

    // No Schema sent: rows accumulate in pending_rows.
    let over = 5u64;
    let total = MAX_PENDING_ROWS as u64 + over;
    let st = rfc3339_nanos(SystemTime::now());
    for seq in 0..total {
        tx.send(ReportingMessage::Row {
            seq,
            timestamp: seq as f64 * 0.001,
            system_time: st.clone(),
            values: vec![seq as f64],
        })
        .expect("send pre-schema Row");
    }
    app.drain_messages();

    assert_eq!(
        app.pending_overflow_drops, over,
        "exactly {over} oldest pre-schema rows should have been dropped"
    );
    assert_eq!(
        app.pending_rows.len(),
        MAX_PENDING_ROWS,
        "pending_rows must cap at MAX_PENDING_ROWS"
    );

    // Drop-oldest invariant: the *newest* MAX_PENDING_ROWS rows must remain, so the
    // front-of-queue seq should be `over` (the first non-dropped) and the back should be
    // `total - 1` (the most recent).
    match app.pending_rows.front() {
        Some(ReportingMessage::Row { seq, .. }) => assert_eq!(
            *seq, over,
            "front of pending_rows should be the oldest retained seq"
        ),
        other => panic!("expected Row at front of pending_rows, got {other:?}"),
    }
    match app.pending_rows.back() {
        Some(ReportingMessage::Row { seq, .. }) => assert_eq!(
            *seq,
            total - 1,
            "back of pending_rows should be the most recent seq"
        ),
        other => panic!("expected Row at back of pending_rows, got {other:?}"),
    }
}

/// A second `Schema` carrying a different `monotonic_epoch_ns` is a new controller session,
/// not a re-emission. On session change the gap-tracking and the per-channel ring buffers
/// must be cleared so the viewer doesn't draw a continuous trace across two distinct
/// controller runs (`app.rs:739-742`). Channel names can be reused across sessions but
/// the values are unrelated.
#[test]
fn new_schema_epoch_resets_gap_tracking_and_clears_buffers() {
    let (mut app, tx) = test_app_with_tx(60.0);

    // Session A: schema + a row pair with a gap, so `dropped_frames_on_wire` is non-zero
    // and `channel_data` is populated when the new schema arrives.
    tx.send(ReportingMessage::Schema {
        channel_names: vec!["a".to_string()],
        channel_units: vec![None],
        monotonic_epoch_ns: 1,
        is_session_end: false,
    })
    .expect("send Schema A");
    let st = rfc3339_nanos(SystemTime::now());
    tx.send(ReportingMessage::Row {
        seq: 0,
        timestamp: 0.0,
        system_time: st.clone(),
        values: vec![10.0],
    })
    .expect("send Row seq=0");
    tx.send(ReportingMessage::Row {
        seq: 3,
        timestamp: 0.15,
        system_time: st.clone(),
        values: vec![13.0],
    })
    .expect("send Row seq=3");
    app.drain_messages();
    assert!(
        app.dropped_frames_on_wire > 0,
        "test setup: gap from seq=0 to seq=3 must register wire drops"
    );
    assert!(
        !app.channel_data.is_empty(),
        "test setup: channel_data must be populated before the new-session schema"
    );
    assert_eq!(app.last_seen_seq, Some(3));

    // Session B: same channel name, fresh epoch. Must reset gap tracking and buffers.
    tx.send(ReportingMessage::Schema {
        channel_names: vec!["a".to_string()],
        channel_units: vec![None],
        monotonic_epoch_ns: 2,
        is_session_end: false,
    })
    .expect("send Schema B");
    app.drain_messages();

    assert_eq!(
        app.dropped_frames_on_wire, 0,
        "new session must reset the wire-drop counter"
    );
    assert_eq!(
        app.last_seen_seq, None,
        "new session must clear the seq high-water mark so the next seq=0 is not flagged as wrap-around"
    );
    assert!(
        app.channel_data.values().all(|buf| buf.is_empty()),
        "new session must clear per-channel ring buffers"
    );
}
