use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::thread;
use std::time::{Duration, Instant};

use deimos::{
    Controller, ControllerCtx, LoopMethod, Termination,
    peripheral::instruments::{keithley_dmm6500, siglent_sdg2042x},
};

fn start_siglent() -> (String, thread::JoinHandle<Vec<String>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap().to_string();
    let thread = thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut writer = stream;
        let mut commands = Vec::new();
        let mut output_enabled = [false; 2];
        let mut waveforms: [String; 2] =
            std::array::from_fn(|number| format!("C{}:BSWV WVTP,DC,OFST,0V", number + 1));
        loop {
            let mut command = String::new();
            if reader.read_line(&mut command).unwrap() == 0 {
                break;
            }
            let command = command.trim_end().to_owned();
            for number in 1..=2 {
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
                    .write_all(b"Siglent Technologies,SDG2042X,FAKE,1.0\n")
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
    (address, thread)
}

fn start_dmm() -> (String, thread::JoinHandle<Vec<String>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap().to_string();
    let thread = thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut writer = stream;
        let mut commands = Vec::new();
        let mut sample = 0_u64;
        loop {
            let mut command = String::new();
            if reader.read_line(&mut command).unwrap() == 0 {
                break;
            }
            let command = command.trim_end().to_owned();
            match command.as_str() {
                "*IDN?" => writer
                    .write_all(b"KEITHLEY INSTRUMENTS,DMM6500,FAKE,1.0\n")
                    .unwrap(),
                ":READ?" => {
                    thread::sleep(Duration::from_millis(25));
                    writeln!(writer, "{:.6e}", 0.5 + sample as f64 * 0.001).unwrap();
                    sample += 1;
                }
                _ => {}
            }
            commands.push(command);
        }
        commands
    });
    (address, thread)
}

#[test]
fn controller_runs_configured_instruments_and_both_siglent_channels() {
    let (siglent_address, siglent_server) = start_siglent();
    let (dmm_address, dmm_server) = start_dmm();

    let mut ctx = ControllerCtx::default();
    ctx.op_name = format!("instrument-integration-{}", std::process::id());
    ctx.op_dir = std::env::temp_dir();
    ctx.dt_ns = 10_000_000;
    ctx.binding_timeout_ms = 100;
    ctx.configuring_timeout_ms = 300;
    ctx.peripheral_loss_of_contact_limit = 20;
    ctx.controller_loss_of_contact_limit = 20;
    ctx.loop_method = LoopMethod::Efficient;
    ctx.termination_criteria = Some(Termination::Timeout(Duration::from_secs(3)));

    let mut controller = Controller::new(ctx);
    controller.clear_sockets();
    let mut siglent_config = siglent_sdg2042x::Config::new(siglent_address, 101);
    siglent_config.channels[0].waveform = siglent_sdg2042x::Waveform::Sine;
    siglent_config.channels[1].waveform = siglent_sdg2042x::Waveform::Square;
    let dmm_config = keithley_dmm6500::Config::new(dmm_address, 102);
    let mut siglent_handle =
        siglent_sdg2042x::attach("siglent", siglent_config, &mut controller).unwrap();
    let mut dmm_handle = keithley_dmm6500::attach("dmm", dmm_config, &mut controller).unwrap();

    let serialized = serde_json::to_string(&controller).unwrap();
    assert!(serialized.contains("SiglentSdg2042X"));
    assert!(serialized.contains("KeithleyDmm6500"));
    assert!(!serialized.contains("FAKE"));

    let mut run = controller.run_nonblocking(None, None, true).unwrap();

    run.write(HashMap::from([
        ("siglent.ch1_enabled".to_owned(), 1.0),
        ("siglent.ch1_frequency_hz".to_owned(), 1_000.0),
        ("siglent.ch1_offset_voltage_v".to_owned(), 0.75),
        ("siglent.ch1_phase_deg".to_owned(), 10.0),
        ("siglent.ch2_enabled".to_owned(), 1.0),
        ("siglent.ch2_frequency_hz".to_owned(), 2_000.0),
        ("siglent.ch2_offset_voltage_v".to_owned(), -0.25),
        ("siglent.ch2_pulse_duty_cycle".to_owned(), 0.25),
        ("siglent.ch2_phase_deg".to_owned(), 20.0),
    ]))
    .unwrap();

    let deadline = Instant::now() + Duration::from_secs(2);
    let mut prior_sample = None;
    let mut observed_repeated_sample = false;
    loop {
        let values = run.read().values;
        let applied_offset = values
            .get("siglent.ch1_applied_offset_voltage_v")
            .copied()
            .unwrap_or_default();
        let channel_2_applied = values.get("siglent.ch2_applied_enabled") == Some(&1.0)
            && values.get("siglent.ch2_applied_frequency_hz") == Some(&2_000.0)
            && values.get("siglent.ch2_applied_offset_voltage_v") == Some(&-0.25)
            && values.get("siglent.ch2_applied_pulse_duty_cycle") == Some(&0.25)
            && values.get("siglent.ch2_applied_phase_deg") == Some(&20.0);
        let sample_sequence = values
            .get("dmm.sample_sequence")
            .copied()
            .unwrap_or_default();
        if prior_sample == Some(sample_sequence) && sample_sequence > 0.0 {
            observed_repeated_sample = true;
        }
        prior_sample = Some(sample_sequence);
        if applied_offset == 0.75 && channel_2_applied && sample_sequence >= 2.0 {
            assert!(values["dmm.voltage_v"].is_finite());
            assert_eq!(values["dmm.resistance_ohm"], 0.0);
            assert_eq!(values["dmm.voltage_active"], 1.0);
            assert_eq!(values["dmm.resistance_active"], 0.0);
            assert!(values["dmm.sample_age_s"].is_finite());
            break;
        }
        assert!(
            Instant::now() < deadline,
            "instrument state did not converge"
        );
        thread::sleep(Duration::from_millis(2));
    }
    assert!(observed_repeated_sample);

    run.stop();
    run.join().unwrap();
    siglent_handle.join().unwrap();
    dmm_handle.join().unwrap();

    let siglent_commands = siglent_server.join().unwrap();
    let dmm_commands = dmm_server.join().unwrap();
    assert!(
        siglent_commands
            .iter()
            .any(|command| command == "C1:OUTP ON,LOAD,100000")
    );
    assert!(
        siglent_commands
            .iter()
            .any(|command| command.starts_with("C1:BSWV WVTP,SINE,"))
    );
    assert!(
        siglent_commands
            .iter()
            .any(|command| command.starts_with("C2:BSWV WVTP,SQUARE,"))
    );
    assert!(
        siglent_commands
            .iter()
            .any(|command| command == "C2:OUTP ON,LOAD,100000")
    );
    assert_eq!(
        &siglent_commands[siglent_commands.len() - 10..],
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
    assert!(
        dmm_commands
            .iter()
            .filter(|command| *command == ":READ?")
            .count()
            >= 2
    );
}
