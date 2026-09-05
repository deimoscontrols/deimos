//! Stages a validated, fixed-layout calibration image for firmware inclusion.

fn main() {
    use deimos_shared::{
        peripherals::deimos_daq_rev8::calc::Calibration,
        states::{ByteStruct, ByteStructLen},
    };
    use std::{env, fs, path::PathBuf};

    println!("cargo:rerun-if-changed=memory.x");
    println!("cargo:rerun-if-changed=itcm.x");
    println!("cargo:rerun-if-changed=dtcm.x");
    println!("cargo:rerun-if-changed=static/calibration.in");

    let source = PathBuf::from("static/calibration.in");
    let bytes = if source.exists() {
        fs::read(&source).expect("read static/calibration.in")
    } else {
        // A missing installed image is operationally defined as a valid,
        // explicitly uncalibrated identity record for the next calibration run.
        let mut bytes = vec![0_u8; Calibration::BYTE_LEN];
        Calibration::default().write_bytes(&mut bytes);
        bytes
    };
    assert_eq!(
        bytes.len(),
        Calibration::BYTE_LEN,
        "calibration blob has the wrong length"
    );
    assert!(
        Calibration::read_bytes(&bytes).is_valid(),
        "calibration blob contains invalid values"
    );
    let output = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR")).join("calibration.in");
    fs::write(output, bytes).expect("write build calibration blob");
}
