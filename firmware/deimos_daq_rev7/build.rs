//! Stages a validated, fixed-layout rev7 calibration image for firmware inclusion.

fn main() {
    use std::{env, fs, path::PathBuf};

    println!("cargo:rerun-if-changed=memory.x");
    println!("cargo:rerun-if-changed=itcm.x");
    println!("cargo:rerun-if-changed=static/calibration.in");

    const CHANNEL_COUNT: usize = 18;
    // One u8 status followed by `(slope, offset)` f32 pairs for all 18 ADC channels.
    const CALIBRATION_LEN: usize = 1 + CHANNEL_COUNT * 2 * size_of::<f32>();
    let source = PathBuf::from("static/calibration.in");
    let mut bytes = if source.exists() {
        fs::read(&source).expect("read static/calibration.in")
    } else {
        // A missing installed image is operationally defined as a valid,
        // explicitly uncalibrated identity record for the next calibration run.
        let mut identity = Vec::with_capacity(CALIBRATION_LEN);
        identity.push(0); // firmware_calibrated = false
        for _ in 0..CHANNEL_COUNT {
            identity.extend_from_slice(&1.0_f32.to_le_bytes());
            identity.extend_from_slice(&0.0_f32.to_le_bytes());
        }
        identity
    };
    assert_eq!(
        bytes.len(),
        CALIBRATION_LEN,
        "rev7 calibration blob has the wrong length"
    );
    let output = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR"))
        .join("calibration.in");
    fs::write(output, &mut bytes).expect("write build calibration blob");
}
