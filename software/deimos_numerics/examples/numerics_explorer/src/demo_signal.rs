use std::f64::consts::TAU;

/// Smooth deterministic colored signal used for process-disturbance examples.
#[must_use]
pub fn colored_signal(step: usize, phase: f64) -> f64 {
    let k = step as f64;
    0.8 * (0.17 * k + phase).sin() + 0.35 * (0.07 * k + 0.5 * phase).cos()
}

/// Deterministic Gaussian-like jitter based on a fixed hashed stream.
///
/// This uses Box-Muller with a stable integer hash instead of runtime RNG so
/// the interactive examples remain repeatable across runs and screenshots.
#[must_use]
pub fn gaussianish_signal(step: usize, stream: u64) -> f64 {
    let u1 = unit_uniform(step * 2, stream).clamp(1.0e-12, 1.0 - 1.0e-12);
    let u2 =
        unit_uniform(step * 2 + 1, stream ^ 0x9e37_79b9_7f4a_7c15).clamp(1.0e-12, 1.0 - 1.0e-12);
    (-2.0 * u1.ln()).sqrt() * (TAU * u2).cos()
}

/// Deterministic short step-plus-tone stimulus used by filter-design demos.
///
/// The waveform starts with a short zero segment, steps to one, then transitions
/// into a sinusoid centered on the supplied normalized cutoff `frequency / fs`.
#[must_use]
pub fn step_then_tone_signal(sample_rate: f64, cutoff_over_fs: f64) -> (Vec<f64>, Vec<f64>) {
    let center = cutoff_over_fs.clamp(1.0e-6, 0.49);
    let period_samples = (1.0 / center).ceil() as usize;
    let tone_samples = (2 * period_samples).max(128);
    let step_start = (period_samples / 8).max(8);
    let step_hold_samples = tone_samples;
    let tone_start = step_start + step_hold_samples;
    let total_samples = tone_start + tone_samples;

    let times = (0..total_samples)
        .map(|index| index as f64 / sample_rate)
        .collect::<Vec<_>>();

    let mut phase = 0.0_f64;
    let mut signal = Vec::with_capacity(total_samples);
    for index in 0..total_samples {
        let sample = if index < step_start {
            0.0
        } else if index < tone_start {
            1.0
        } else {
            let sample = 1.0 + phase.sin();
            phase += TAU * center;
            sample
        };
        signal.push(sample);
    }

    (times, signal)
}

fn unit_uniform(index: usize, stream: u64) -> f64 {
    let mut x = (index as u64)
        .wrapping_mul(0x9e37_79b9_7f4a_7c15)
        .wrapping_add(stream.wrapping_mul(0xbf58_476d_1ce4_e5b9))
        .wrapping_add(0x94d0_49bb_1331_11eb);
    x ^= x >> 30;
    x = x.wrapping_mul(0xbf58_476d_1ce4_e5b9);
    x ^= x >> 27;
    x = x.wrapping_mul(0x94d0_49bb_1331_11eb);
    x ^= x >> 31;
    ((x >> 11) as f64) * (1.0 / ((1_u64 << 53) as f64))
}
