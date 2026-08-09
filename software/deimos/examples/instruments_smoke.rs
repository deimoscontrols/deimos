//! Supervised loopback smoke test for an SDG2042X channel connected to a DMM6500.
//!
//! With no arguments, the test connects to the lab-reserved addresses below,
//! drives Siglent channel 1 to 1 V DC, and checks fresh DMM readings against a
//! 50 mV tolerance. This commands physical hardware and should only be run
//! while channel 1 is safely connected to the DMM input.
//!
//! Normal and error exits join both drivers. The Siglent driver drives both
//! channels to 0 V DC before opening their output relays during that shutdown.
//!
//! Usage:
//! `cargo run -p deimos --example instruments_smoke -- [siglent-host] [dmm-host] [voltage-v] [samples]`

use std::collections::HashMap;
use std::time::{Duration, Instant};

use deimos::{
    Controller, ControllerCtx, LoopMethod, Termination, ThreadChannelSocket,
    peripheral::instruments::{
        keithley_dmm6500::{Config as KeithleyConfig, KeithleyDmm6500Driver},
        siglent_sdg2042x::{Config as SiglentConfig, SiglentSdg2042XDriver, Waveform},
    },
};

const DEFAULT_SIGLENT_HOST: &str = "192.168.10.169";
const DEFAULT_DMM_HOST: &str = "192.168.10.213";

fn main() -> Result<(), String> {
    let mut args = std::env::args().skip(1);
    let siglent_host = args
        .next()
        .unwrap_or_else(|| DEFAULT_SIGLENT_HOST.to_owned());
    let dmm_host = args.next().unwrap_or_else(|| DEFAULT_DMM_HOST.to_owned());
    let voltage_v = args
        .next()
        .map(|value| value.parse::<f64>().map_err(|err| err.to_string()))
        .transpose()?
        .unwrap_or(1.0);
    let sample_count = args
        .next()
        .map(|value| value.parse::<usize>().map_err(|err| err.to_string()))
        .transpose()?
        .unwrap_or(5);
    if args.next().is_some() || sample_count == 0 {
        return Err(usage());
    }
    if !voltage_v.is_finite() || !(-2.0..=2.0).contains(&voltage_v) {
        return Err("smoke-test voltage must be finite and within -2..=2 V".to_owned());
    }

    let mut siglent_config = SiglentConfig::new(siglent_host, 1);
    siglent_config.channels[0].waveform = Waveform::Dc;
    siglent_config.channels[0].offset_voltage_v = (-2.0, 2.0);
    siglent_config.channels[1].waveform = Waveform::Dc;
    let siglent = SiglentSdg2042XDriver::new(siglent_config)?;
    let dmm = KeithleyDmm6500Driver::new(KeithleyConfig::new(dmm_host, 1))?;

    let mut ctx = ControllerCtx::default();
    ctx.op_name = "instruments-smoke".to_owned();
    ctx.op_dir = std::env::temp_dir();
    ctx.dt_ns = 20_000_000;
    ctx.loop_method = LoopMethod::Efficient;
    ctx.termination_criteria = Some(Termination::Timeout(Duration::from_secs(15)));

    let mut controller = Controller::new(ctx);
    controller.clear_sockets();
    let siglent_channel_name = siglent.channel_name().to_owned();
    let dmm_channel_name = dmm.channel_name().to_owned();
    controller.add_socket(
        &siglent_channel_name,
        Box::new(ThreadChannelSocket::new(&siglent_channel_name)),
    );
    controller.add_socket(
        &dmm_channel_name,
        Box::new(ThreadChannelSocket::new(&dmm_channel_name)),
    );
    controller.add_peripheral("siglent", Box::new(siglent.peripheral()))?;
    controller.add_peripheral("dmm", Box::new(dmm.peripheral()))?;

    let mut siglent_handle = siglent.run(&controller.ctx)?;
    let mut dmm_handle = match dmm.run(&controller.ctx) {
        Ok(handle) => handle,
        Err(err) => {
            siglent_handle.join()?;
            return Err(err);
        }
    };
    println!(
        "Siglent identity: {}",
        siglent.identity().unwrap_or_default()
    );
    println!("Keithley identity: {}", dmm.identity().unwrap_or_default());

    let mut controller_handle = match controller.run_nonblocking(None, None, true) {
        Ok(handle) => handle,
        Err(err) => {
            siglent_handle.join()?;
            dmm_handle.join()?;
            return Err(err);
        }
    };

    let test_result = (|| {
        let initial_sample = controller_handle
            .read()
            .values
            .get("dmm.sample_sequence")
            .copied()
            .unwrap_or_default();
        controller_handle.write(HashMap::from([
            ("siglent.ch1_enabled".to_owned(), 1.0),
            ("siglent.ch1_offset_voltage_v".to_owned(), voltage_v),
        ]))?;

        wait_until(Duration::from_secs(3), || {
            let values = controller_handle.read().values;
            values
                .get("siglent.ch1_applied_enabled")
                .is_some_and(|value| *value == 1.0)
                && values
                    .get("siglent.ch1_applied_offset_voltage_v")
                    .is_some_and(|value| *value == voltage_v)
        })?;

        // Exclude readings initiated before the source command and allow the
        // DMM input/filter state to settle before defining the sample window.
        std::thread::sleep(Duration::from_millis(250));
        let settled_sample = controller_handle
            .read()
            .values
            .get("dmm.sample_sequence")
            .copied()
            .unwrap_or(initial_sample);

        let deadline = Instant::now() + Duration::from_secs(5);
        let mut last_sequence = settled_sample;
        let mut samples = Vec::with_capacity(sample_count);
        while samples.len() < sample_count {
            let values = controller_handle.read().values;
            if values.get("siglent.ch1_applied_enabled") != Some(&1.0)
                || values.get("siglent.ch1_applied_offset_voltage_v") != Some(&voltage_v)
            {
                return Err(format!(
                    "Siglent left the requested applied state while sampling: enabled={:?}, offset={:?}",
                    values.get("siglent.ch1_applied_enabled"),
                    values.get("siglent.ch1_applied_offset_voltage_v")
                ));
            }
            let sequence = values
                .get("dmm.sample_sequence")
                .copied()
                .unwrap_or_default();
            let age = values
                .get("dmm.sample_age_s")
                .copied()
                .unwrap_or(f64::INFINITY);
            if sequence > last_sequence && age < 0.5 {
                samples.push(values["dmm.voltage_v"]);
                last_sequence = sequence;
            }
            if Instant::now() >= deadline {
                return Err(format!(
                    "timed out after collecting {} of {sample_count} fresh DMM samples",
                    samples.len()
                ));
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        let mean = samples.iter().sum::<f64>() / samples.len() as f64;
        println!("Requested {voltage_v:.6} V; DMM mean {mean:.9} V from {sample_count} samples");
        if (mean - voltage_v).abs() > 0.05 {
            return Err(format!(
                "loopback error {:.6} V exceeded the 0.05 V smoke-test tolerance",
                mean - voltage_v
            ));
        }
        Ok(())
    })();

    controller_handle.stop();
    let controller_result = controller_handle.join();
    let siglent_result = siglent_handle.join();
    let dmm_result = dmm_handle.join();
    test_result?;
    controller_result?;
    siglent_result?;
    dmm_result?;
    println!("Loopback smoke test passed; both Siglent outputs are off.");
    Ok(())
}

/// Poll a condition until it succeeds or a bounded deadline expires.
fn wait_until(timeout: Duration, mut condition: impl FnMut() -> bool) -> Result<(), String> {
    let deadline = Instant::now() + timeout;
    while !condition() {
        if Instant::now() >= deadline {
            return Err("timed out waiting for the Siglent command to be applied".to_owned());
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    Ok(())
}

/// Build command-line usage text including the current lab defaults.
fn usage() -> String {
    format!(
        "usage: instruments_smoke [siglent-host[:port]] [dmm-host[:port]] [voltage-v] [samples]\n\
         defaults: Siglent {DEFAULT_SIGLENT_HOST}, DMM6500 {DEFAULT_DMM_HOST}, 1.0 V, 5 samples"
    )
}
