use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::thread;
use std::time::{Duration, Instant};

use deimos_shared::states::ByteStruct;

use super::super::responder::InstrumentProxy;
use super::config::CHANNEL_COUNT;
use super::driver::{basic_wave_command, combine_worker_results};
use super::peripheral::{ChannelState, INPUT_SIZE, InstrumentState, OperatingInput};
use super::*;
use crate::controller::context::ControllerCtx;
use crate::peripheral::Peripheral;

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
fn repeated_controller_state_is_queued_for_reassertion() {
    let driver = SiglentSdg2042XDriver::new(Config::new("localhost", 1)).unwrap();
    let request = valid_state(1.0);
    let mut bytes = vec![0; INPUT_SIZE];
    OperatingInput {
        id: 1,
        state: request,
    }
    .write_bytes(&mut bytes);
    assert_eq!(driver.process_request(&bytes), 1);
    assert_eq!(driver.take_queued(), Some(request));
    assert_eq!(driver.process_request(&bytes), 1);
    assert_eq!(driver.queued(), Some(request));
}

#[test]
fn safe_state_precedes_commands_received_after_returning_to_binding() {
    let driver = SiglentSdg2042XDriver::new(Config::new("localhost", 1)).unwrap();
    let old = valid_state(1.0);
    let new = valid_state(2.0);

    driver.submit(old);
    driver.request_safe_state();
    driver.submit(new);

    assert_eq!(driver.take_queued(), Some(InstrumentState::default()));
    assert_eq!(driver.take_queued(), Some(new));
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
fn startup_rejects_an_unverified_safe_state() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut writer = stream;
        loop {
            let mut command = String::new();
            if reader.read_line(&mut command).unwrap() == 0 {
                break;
            }
            match command.trim_end() {
                "*IDN?" => writer
                    .write_all(b"Siglent Technologies,SDG2042X,TEST,1.0\n")
                    .unwrap(),
                "*OPC?" => writer.write_all(b"1\n").unwrap(),
                "C1:BSWV?" => writer
                    .write_all(b"C1:BSWV WVTP,SINE,FRQ,1000HZ,AMP,1V,OFST,0V,PHSE,0\n")
                    .unwrap(),
                "C2:BSWV?" => writer
                    .write_all(b"C2:BSWV WVTP,SINE,FRQ,1000HZ,AMP,1V,OFST,0V,PHSE,0\n")
                    .unwrap(),
                "C1:OUTP?" => writer
                    .write_all(b"C1:OUTP OFF,LOAD,100000,PLRT,NOR\n")
                    .unwrap(),
                "C2:OUTP?" => writer
                    .write_all(b"C2:OUTP OFF,LOAD,100000,PLRT,NOR\n")
                    .unwrap(),
                _ => {}
            }
        }
    });

    let driver = SiglentSdg2042XDriver::new(Config::new(address.to_string(), 1)).unwrap();
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
fn operating_and_shutdown_failures_are_both_reported() {
    let error = combine_worker_results(
        Err("operating failure".to_owned()),
        Err("safe-state failure".to_owned()),
    )
    .unwrap_err();
    assert!(error.contains("operating failure"));
    assert!(error.contains("additionally failed to apply safe state during shutdown"));
    assert!(error.contains("safe-state failure"));
}

#[test]
fn shutdown_rejects_an_unverified_safe_state() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut writer = stream;
        let mut waveform_queries = 0;
        loop {
            let mut command = String::new();
            if reader.read_line(&mut command).unwrap() == 0 {
                break;
            }
            match command.trim_end() {
                "*IDN?" => writer
                    .write_all(b"Siglent Technologies,SDG2042X,TEST,1.0\n")
                    .unwrap(),
                "*OPC?" => writer.write_all(b"1\n").unwrap(),
                "C1:OUTP?" => writer
                    .write_all(b"C1:OUTP OFF,LOAD,100000,PLRT,NOR\n")
                    .unwrap(),
                "C2:OUTP?" => writer
                    .write_all(b"C2:OUTP OFF,LOAD,100000,PLRT,NOR\n")
                    .unwrap(),
                query @ ("C1:BSWV?" | "C2:BSWV?") => {
                    let channel = &query[..2];
                    let response = if waveform_queries < 4 {
                        format!("{channel}:BSWV WVTP,DC,OFST,0V")
                    } else {
                        format!("{channel}:BSWV WVTP,SINE,FRQ,1000HZ,AMP,1V,OFST,0V,PHSE,0")
                    };
                    writeln!(writer, "{response}").unwrap();
                    waveform_queries += 1;
                }
                _ => {}
            }
        }
    });

    let driver = SiglentSdg2042XDriver::new(Config::new(address.to_string(), 1)).unwrap();
    let mut handle = driver.run(&ControllerCtx::default()).unwrap();
    let error = handle.join().unwrap_err();
    assert!(error.contains("failed to apply safe state during shutdown"));
    assert!(error.contains("was not 0 V DC"));
    server.join().unwrap();
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
        let mut waveforms: [String; CHANNEL_COUNT] =
            std::array::from_fn(|number| format!("C{}:BSWV WVTP,DC,OFST,0V", number + 1));
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
                } else if command.starts_with(&format!("C{number}:BSWV ")) {
                    waveforms[number - 1] = command.clone();
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
                "C1:BSWV?" => writeln!(writer, "{}", waveforms[0]).unwrap(),
                "C2:BSWV?" => writeln!(writer, "{}", waveforms[1]).unwrap(),
                "*OPC?" => writer.write_all(b"1\n").unwrap(),
                _ => {}
            }
            commands.push(command);
        }
        commands
    });

    let mut config = Config::new(address.to_string(), 1);
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

    let deadline = Instant::now() + Duration::from_secs(1);
    loop {
        if driver.applied().unwrap() == expected {
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
