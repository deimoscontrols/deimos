//! Rev7 DAQ calibration entrypoint.
//!
//! The procedure implementation lives with the rev7 peripheral module so it can
//! be reused by a future CLI or GUI.

use std::{
    env,
    path::{Path, PathBuf},
};

use deimos::peripheral::deimos_daq_rev7::calibration_7_0_0::run_procedure;

const DEFAULT_SERIAL_NUMBER: u64 = 2;

struct Args {
    sn: u64,
    collect: bool,
    process: bool,
    dst: PathBuf,
}

fn main() -> Result<(), String> {
    let args = parse_args()?;
    run_procedure(args.sn, args.collect, args.process, &args.dst)
}

fn parse_args() -> Result<Args, String> {
    let mut sn = DEFAULT_SERIAL_NUMBER;
    let mut mode = Mode::CollectAndProcess;
    let mut dst = None;
    let mut positional = Vec::new();
    let mut args = env::args().skip(1);

    while let Some(arg) = args.next() {
        if arg == "--sn" {
            let value = args
                .next()
                .ok_or_else(|| "Missing value after --sn".to_owned())?;
            sn = value
                .parse::<u64>()
                .map_err(|e| format!("Expected integer serial number after --sn: {e}"))?;
        } else {
            positional.push(arg);
        }
    }

    match positional.as_slice() {
        [] => {}
        [arg] if arg == "collect" => mode = Mode::Collect,
        [arg] if arg == "process" => mode = Mode::Process,
        [arg] => dst = Some(PathBuf::from(arg)),
        [arg, path] if arg == "collect" => {
            mode = Mode::Collect;
            dst = Some(PathBuf::from(path));
        }
        [arg, path] if arg == "process" => {
            mode = Mode::Process;
            dst = Some(PathBuf::from(path));
        }
        _ => return Err(usage()),
    }

    let dst = dst.unwrap_or_else(|| default_calibration_dir(sn));

    Ok(Args {
        sn,
        collect: mode.collect(),
        process: mode.process(),
        dst,
    })
}

#[derive(Clone, Copy)]
enum Mode {
    Collect,
    Process,
    CollectAndProcess,
}

impl Mode {
    fn collect(self) -> bool {
        matches!(self, Self::Collect | Self::CollectAndProcess)
    }

    fn process(self) -> bool {
        matches!(self, Self::Process | Self::CollectAndProcess)
    }
}

fn default_calibration_dir(sn: u64) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../deimos_website/docs/cals")
        .join(format!("sn{sn}"))
}

fn usage() -> String {
    "Usage:\n  rev7_0_0_calibration [--sn <serial>] [<dst>]\n  rev7_0_0_calibration [--sn <serial>] collect [<dst>]\n  rev7_0_0_calibration [--sn <serial>] process [<dst>]".to_owned()
}
