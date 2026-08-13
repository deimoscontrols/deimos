use std::time::Duration;

use deimos_shared::states::ByteStruct;

use super::super::responder::InstrumentProxy;
use super::super::scpi::ScpiClient;
use super::driver::{apply_request, basic_wave_command, combine_worker_results};
use super::peripheral::{ChannelState, INPUT_SIZE, InstrumentState, OperatingInput};
use super::*;
use crate::controller::context::ControllerCtx;
use crate::peripheral::Peripheral;
use crate::peripheral::instruments::test_support::{
    SiglentSimulator, contains_command, wait_until,
};

fn valid_state(offset_voltage_v: f64) -> InstrumentState {
    InstrumentState {
        ch1: ChannelState {
            enabled: 1.0,
            frequency_hz: 1_000.0,
            offset_voltage_v,
            pulse_duty_cycle: 0.5,
            phase_deg: 10.0,
            stdev: 0.1,
        },
        ch2: ChannelState {
            enabled: 0.0,
            frequency_hz: 1_000.0,
            offset_voltage_v: 0.0,
            pulse_duty_cycle: 0.5,
            phase_deg: 0.0,
            stdev: 0.1,
        },
    }
}

#[test]
fn repeated_controller_state_replaces_the_pending_state() {
    let config = Config::new("localhost", 1);
    let driver = SiglentSdg2042XDriver::new(config.clone()).unwrap();
    let request = valid_state(1.0);
    let normalized = request.normalized(&config.channels);
    let mut bytes = vec![0; INPUT_SIZE];
    OperatingInput {
        id: 1,
        state: request,
    }
    .write_bytes(&mut bytes);
    assert_eq!(driver.process_request(&bytes), 1);
    assert_eq!(driver.take_queued(), Some(normalized));
    assert_eq!(driver.process_request(&bytes), 1);
    assert_eq!(driver.queued(), Some(normalized));
}

#[test]
fn safe_state_precedes_commands_received_after_returning_to_binding() {
    let config = Config::new("localhost", 1);
    let driver = SiglentSdg2042XDriver::new(config.clone()).unwrap();
    let old = valid_state(1.0);
    let new = valid_state(2.0);

    driver.submit(old);
    driver.request_safe_state();
    driver.submit(new);

    assert_eq!(driver.take_queued(), Some(InstrumentState::default()));
    assert_eq!(driver.take_queued(), Some(new.normalized(&config.channels)));
    assert_eq!(driver.take_queued(), None);
}

#[test]
fn controller_values_are_clamped_to_configured_ranges() {
    let configs = std::array::from_fn(|_| ChannelConfig::default());
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
    let driver = SiglentSdg2042XDriver::new(Config::new("localhost", 1)).unwrap();
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
    assert_eq!(driver.process_request(&bytes), 1);
    let request = driver.queued().unwrap();
    assert_eq!(request.ch1, ChannelState::default());
    assert_eq!(request.ch2.enabled, 1.0);
    assert_eq!(request.ch2.offset_voltage_v, 2.0);
    assert!(driver.latched_error().is_none());
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
    for (waveform, frequency, amplitude, offset, duty, phase, noise) in [
        (Waveform::Sine, true, true, true, false, true, false),
        (Waveform::Square, true, true, true, true, true, false),
        (Waveform::Ramp, true, true, true, false, true, false),
        (Waveform::Pulse, true, true, true, true, false, false),
        (Waveform::Noise, false, false, false, false, false, true),
        (Waveform::Dc, false, false, true, false, false, false),
    ] {
        let config = ChannelConfig {
            waveform,
            ..ChannelConfig::default()
        };
        let command = basic_wave_command(1, &config, request);
        assert_eq!(command.contains(",FRQ,"), frequency);
        assert_eq!(command.contains(",AMP,"), amplitude);
        assert_eq!(command.contains(",OFST,"), offset);
        assert_eq!(command.contains(",DUTY,"), duty);
        assert_eq!(command.contains(",PHSE,"), phase);
        assert_eq!(command.contains(",MEAN,"), noise);
        assert_eq!(command.contains(",STDEV,"), noise);
        if duty {
            assert!(command.contains(",DUTY,4.00000000000000000e1"));
        }
    }
}

#[test]
fn request_application_emits_only_changed_channel_commands() {
    let (address, server) = SiglentSimulator::default().spawn();
    let config = Config::new(address, 1);
    let mut client = ScpiClient::connect(&config.connection).unwrap();
    let initial = InstrumentState::default();
    let enabled = valid_state(1.0).normalized(&config.channels);

    apply_request(&mut client, &config, initial, enabled).unwrap();
    apply_request(&mut client, &config, enabled, enabled).unwrap();

    let mut unused_field_change = enabled;
    unused_field_change.ch1.frequency_hz = 2_000.0;
    apply_request(&mut client, &config, enabled, unused_field_change).unwrap();

    let mut voltage_change = unused_field_change;
    voltage_change.ch1.offset_voltage_v = 1.5;
    apply_request(&mut client, &config, unused_field_change, voltage_change).unwrap();
    apply_request(
        &mut client,
        &config,
        voltage_change,
        InstrumentState::default(),
    )
    .unwrap();
    client.shutdown().unwrap();

    let commands = server.join().unwrap();
    assert_eq!(
        commands,
        [
            basic_wave_command(1, &config.channels[0], enabled.ch1),
            "C1:OUTP ON,LOAD,100000".to_owned(),
            "*OPC?".to_owned(),
            basic_wave_command(1, &config.channels[0], voltage_change.ch1),
            "*OPC?".to_owned(),
            "C1:BSWV WVTP,DC,OFST,0".to_owned(),
            "C1:OUTP OFF,LOAD,100000".to_owned(),
            "*OPC?".to_owned(),
        ]
    );
}

#[test]
fn startup_rejects_an_unverified_safe_state() {
    let simulator = SiglentSimulator::default().failing_waveforms_after(0);
    let (address, server) = simulator.spawn();

    let driver = SiglentSdg2042XDriver::new(Config::new(address, 1)).unwrap();
    let error = match driver.run(&ControllerCtx::default()) {
        Ok(mut handle) => {
            let _ = handle.join();
            panic!("startup accepted an unverified safe state");
        }
        Err(error) => error,
    };
    assert!(error.contains("setup failed"));
    assert!(error.contains("was not 0 V DC"));
    server.join().unwrap();
}

#[test]
fn startup_rejects_scpi_errors_reported_by_the_event_status_register() {
    let simulator = SiglentSimulator::default().with_esr(32);
    let (address, server) = simulator.spawn();

    let driver = SiglentSdg2042XDriver::new(Config::new(address, 2)).unwrap();
    let error = match driver.run(&ControllerCtx::default()) {
        Err(error) => error,
        Ok(mut handle) => {
            let _ = handle.join();
            panic!("startup accepted an event-status command error");
        }
    };
    assert!(error.contains("setup failed"));
    assert!(error.contains("SCPI error bits"));
    server.join().unwrap();
}

#[test]
fn operating_and_shutdown_failures_are_both_reported() {
    let error = combine_worker_results(
        Err("operating failure".to_owned()),
        Err("safe-state failure".to_owned()),
    )
    .unwrap_err();
    assert!(error.contains("operating failure"));
    assert!(error.contains("additionally failed to shut down SDG2042X"));
    assert!(error.contains("safe-state failure"));
}

#[test]
fn shutdown_rejects_an_unverified_safe_state() {
    let simulator = SiglentSimulator::default().failing_waveforms_after(4);
    let (address, server) = simulator.spawn();

    let driver = SiglentSdg2042XDriver::new(Config::new(address, 1)).unwrap();
    let mut handle = driver.run(&ControllerCtx::default()).unwrap();
    let error = handle.join().unwrap_err();
    assert!(error.contains("failed to shut down SDG2042X"));
    assert!(error.contains("safe-state failure"));
    assert!(error.contains("was not 0 V DC"));
    server.join().unwrap();
}

#[test]
fn worker_applies_complete_two_channel_state_and_shuts_down_safe() {
    let (address, server) = SiglentSimulator::default().spawn();

    let mut config = Config::new(address, 1);
    config.channels[0].waveform = Waveform::Sine;
    config.channels[1].waveform = Waveform::Noise;
    let driver = SiglentSdg2042XDriver::new(config.clone()).unwrap();
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
    let expected = OperatingInput::read_bytes(&bytes)
        .state
        .normalized(&config.channels);
    assert_eq!(driver.process_request(&bytes), 8);

    wait_until(Duration::from_secs(1), || {
        driver.applied().unwrap() == expected
    });
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
    for expected in ["C1:OUTP ON,LOAD,100000", "C2:OUTP ON,LOAD,100000"] {
        assert!(contains_command(&commands, expected));
    }
    // Four startup readbacks and two shutdown readbacks; Operating adds none.
    assert_eq!(
        commands
            .iter()
            .filter(|command| command.ends_with(":BSWV?"))
            .count(),
        6
    );
    assert_eq!(
        &commands[commands.len() - 10..],
        [
            "C1:BSWV WVTP,DC,OFST,0",
            "C2:BSWV WVTP,DC,OFST,0",
            "*OPC?",
            "C1:BSWV?",
            "C2:BSWV?",
            "C1:OUTP OFF,LOAD,100000",
            "C2:OUTP OFF,LOAD,100000",
            "*OPC?",
            "C1:OUTP?",
            "C2:OUTP?",
        ]
    );
}

#[test]
fn worker_does_not_reassert_an_unchanged_controller_state() {
    let (address, server) = SiglentSimulator::default().spawn();
    let config = Config::new(address, 1);
    let driver = SiglentSdg2042XDriver::new(config.clone()).unwrap();
    let mut handle = driver.run(&ControllerCtx::default()).unwrap();
    let mut bytes = vec![0; INPUT_SIZE];
    let request = valid_state(1.0).normalized(&config.channels);
    OperatingInput {
        id: 1,
        state: request,
    }
    .write_bytes(&mut bytes);

    for _ in 0..100 {
        driver.process_request(&bytes);
    }
    wait_until(Duration::from_secs(1), || {
        driver.applied().unwrap() == request
    });
    for _ in 0..100 {
        driver.process_request(&bytes);
    }
    std::thread::sleep(Duration::from_millis(50));
    handle.join().unwrap();

    let commands = server.join().unwrap();
    let waveform = basic_wave_command(1, &config.channels[0], request.ch1);
    assert_eq!(
        commands
            .iter()
            .filter(|command| command.as_str() == waveform)
            .count(),
        1
    );
    assert_eq!(
        commands
            .iter()
            .filter(|command| command.as_str() == "C1:OUTP ON,LOAD,100000")
            .count(),
        1
    );
}
