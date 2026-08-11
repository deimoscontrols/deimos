use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::controller::Controller;
use crate::controller::context::{ControllerCtx, LoopMethod, Termination};
use crate::peripheral::instruments::{keithley_dmm6500, siglent_sdg2042x};

pub(super) type TestServer = (String, JoinHandle<Vec<String>>);

pub(super) fn spawn_scpi_server(
    mut respond: impl FnMut(&str) -> Option<String> + Send + 'static,
) -> TestServer {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap().to_string();
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
            if let Some(response) = respond(&command) {
                writeln!(writer, "{response}").unwrap();
            }
            commands.push(command);
        }
        commands
    });
    (address, server)
}

pub(super) fn wait_until(timeout: Duration, mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + timeout;
    while !condition() {
        assert!(Instant::now() < deadline, "condition did not converge");
        thread::sleep(Duration::from_millis(1));
    }
}

pub(super) fn contains_command(commands: &[String], expected: &str) -> bool {
    commands.iter().any(|command| command == expected)
}

#[derive(Default)]
pub(super) struct SiglentSimulator {
    waveforms: [String; 2],
    outputs: [bool; 2],
    esr: u8,
    fail_waveform_after: Option<usize>,
    waveform_reads: usize,
}

impl SiglentSimulator {
    pub(super) fn with_esr(mut self, esr: u8) -> Self {
        self.esr = esr;
        self
    }

    pub(super) fn failing_waveforms_after(mut self, reads: usize) -> Self {
        self.fail_waveform_after = Some(reads);
        self
    }

    pub(super) fn spawn(mut self) -> TestServer {
        spawn_scpi_server(move |command| self.respond(command))
    }

    fn respond(&mut self, command: &str) -> Option<String> {
        for number in 1..=2 {
            if command == format!("C{number}:OUTP ON,LOAD,100000") {
                self.outputs[number - 1] = true;
            } else if command.starts_with(&format!("C{number}:OUTP OFF")) {
                self.outputs[number - 1] = false;
            } else if command.starts_with(&format!("C{number}:BSWV ")) {
                self.waveforms[number - 1] = command.to_owned();
            }
        }
        match command {
            "*IDN?" => Some("Siglent Technologies,SDG2042X,TEST,1.0".to_owned()),
            "*OPC?" => Some("1".to_owned()),
            "*ESR?" => Some(format!("*ESR {}", self.esr)),
            "C1:OUTP?" | "C2:OUTP?" => {
                let index = usize::from(command.as_bytes()[1] - b'1');
                let state = if self.outputs[index] { "ON" } else { "OFF" };
                Some(format!("C{}:OUTP {state},LOAD,100000,PLRT,NOR", index + 1))
            }
            "C1:BSWV?" | "C2:BSWV?" => {
                let index = usize::from(command.as_bytes()[1] - b'1');
                let fail = self
                    .fail_waveform_after
                    .is_some_and(|limit| self.waveform_reads >= limit);
                self.waveform_reads += 1;
                if fail {
                    Some(format!(
                        "C{}:BSWV WVTP,SINE,FRQ,1000HZ,AMP,1V,OFST,0V,PHSE,0",
                        index + 1
                    ))
                } else {
                    Some(self.waveforms[index].clone())
                }
            }
            _ => None,
        }
    }
}

#[test]
fn controller_runs_both_instrument_drivers() {
    let (siglent_address, siglent_server) = SiglentSimulator::default().spawn();
    let mut sample = 0_u64;
    let (dmm_address, dmm_server) = spawn_scpi_server(move |command| match command {
        "*IDN?" => Some("KEITHLEY INSTRUMENTS,DMM6500,TEST,1.0".to_owned()),
        ":SYSTem:ERRor:NEXT?" => Some("0,\"No error;0;0 0\"".to_owned()),
        ":READ?" => {
            thread::sleep(Duration::from_millis(25));
            sample += 1;
            Some((0.5 + sample as f64 * 0.001).to_string())
        }
        _ => None,
    });

    let mut ctx = ControllerCtx::default();
    ctx.op_name = format!("instrument-integration-{}", std::process::id());
    ctx.op_dir = std::env::temp_dir();
    ctx.dt_ns = 10_000_000;
    ctx.configuring_timeout_ms = 300;
    ctx.peripheral_loss_of_contact_limit = 20;
    ctx.controller_loss_of_contact_limit = 20;
    ctx.loop_method = LoopMethod::Efficient;
    ctx.termination_criteria = Some(Termination::Timeout(Duration::from_secs(3)));

    let mut controller = Controller::new(ctx);
    controller.clear_sockets();
    let siglent_config = siglent_sdg2042x::Config::new(siglent_address, 101);
    let dmm_config = keithley_dmm6500::Config::new(dmm_address, 102);
    let mut siglent = siglent_sdg2042x::attach("siglent", siglent_config, &mut controller).unwrap();
    let mut dmm = keithley_dmm6500::attach("dmm", dmm_config, &mut controller).unwrap();
    let serialized = serde_json::to_string(&controller).unwrap();
    assert!(serialized.contains("SiglentSdg2042X"));
    assert!(serialized.contains("KeithleyDmm6500"));

    let mut run = controller.run_nonblocking(None, None, true).unwrap();
    run.write(std::collections::HashMap::from([
        ("siglent.ch1_enabled".to_owned(), 1.0),
        ("siglent.ch1_offset_voltage_v".to_owned(), 0.75),
        ("siglent.ch2_enabled".to_owned(), 1.0),
        ("siglent.ch2_offset_voltage_v".to_owned(), -0.25),
    ]))
    .unwrap();

    wait_until(Duration::from_secs(2), || {
        let values = run.read().values;
        values.get("siglent.ch1_applied_offset_voltage_v") == Some(&0.75)
            && values.get("siglent.ch2_applied_offset_voltage_v") == Some(&-0.25)
            && values
                .get("dmm.sample_sequence")
                .is_some_and(|value| *value >= 2.0)
            && values["dmm.voltage_v"].is_finite()
    });

    run.stop();
    run.join().unwrap();
    siglent.join().unwrap();
    dmm.join().unwrap();
    siglent_server.join().unwrap();
    dmm_server.join().unwrap();
}
