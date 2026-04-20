# Signal pipeline

This document follows a single measurement from the sensor pin to the value that lands in your database. If you just want to *use* Deimos, skim the six stages below and stop. If you're contributing firmware changes to the filter code, also read [`.claude/rules/signal-pipeline.md`](../.claude/rules/signal-pipeline.md) — it pins down exact firmware file paths, function names, and invariants.

## The six stages

A raw analog voltage at the sensor pin passes through six stages of conditioning before it becomes a reported sample. The first three are in hardware; the last three are in firmware running on the peripheral's MCU.

| # | Stage | Where | Purpose |
| --- | --- | --- | --- |
| 1 | RF filter | Hardware (passive) | Strip radio-frequency interference picked up on long cables |
| 2 | Active low-pass (Sallen-Key) | Hardware (analog) | Main anti-aliasing filter; cutoff 100 Hz – 5 kHz. Does the "heavy lifting." |
| 3 | ADC input filter | Hardware (~16 kHz) | Reduces pull-down, crosstalk, and aliasing at the ADC input pins |
| 4 | FIR fractional delay | Firmware | Align samples across channels in time ("synthetic simultaneous sampling") |
| 5 | Butterworth IIR | Firmware | Smooth the signal; low-pass at the user's requested cutoff |
| 6 | Decimation | Firmware | Reduce the rate from the internal 33 kHz down to the user's chosen 5 Hz – 5 kHz |

## Why so many filters?

Each stage targets a different failure mode. Only stage 2 does bulk noise rejection; the others are there to fix problems that would otherwise contaminate the measurement — RF pickup, inter-channel timing skew, quantization aliasing, etc.

One choice worth flagging: the pipeline **intentionally allows some aliasing** to preserve **phase margin**. Phase margin is how much room a control loop has before it becomes unstable. If Deimos filtered hard enough to eliminate all aliasing, the added phase lag would make closed-loop control on top of the measurements unstable or sluggish. Trading a small amount of aliasing for phase margin is what makes the system usable for *realtime control*, not just offline data logging.

## Stage 4: FIR fractional delay

The ADC can't sample every channel at the exact same instant — it scans them sequentially at 33 kHz. For most purposes that's fine, but if you want two signals to look like they were sampled simultaneously (e.g. computing instantaneous power from V and I), you have to re-align them in software.

Deimos uses a **3-tap FIR filter** per channel, with coefficients computed by **Lagrange polynomial interpolation**, to shift each channel's sample in time by a fractional amount relative to a reference channel. Implementation is a dot product on a ring buffer.

Why 3 taps specifically? A 3-tap (polynomial order 2) filter is the sweet spot: higher orders introduce overshoot and precision problems without meaningful accuracy gains for this use case.

## Stage 5: Butterworth IIR

The smoothing filter is an Nth-order Butterworth low-pass. Two implementation choices shape the rest of the pipeline:

### State-space canonical form, not biquad

Instead of the more common direct-form II or cascaded biquad implementation, Deimos uses **state-space canonical form**. This stores only the nontrivial row of the A matrix, the C vector, and a scalar D. The per-sample update reduces to two vector dot products — no matrix math. Fewer coefficients to get right, fewer floating-point operations, less roundoff.

### Steady-state initialization (no "step" on run start)

A naive IIR filter starts with state zeroed out, which causes a visible step at the beginning of every run as the filter charges up. Deimos initializes the filter state to its **steady-state value for the most recent input** at run start. The closed-form trick is: for constant input *u*, every state variable equals `u / (1 - sum(A))` — no matrix inversion required.

On the firmware side, the steady-state initialization fires whenever the reporting cutoff changes. On the host side (for host-side `Butter` calcs), the same initialization runs on the first sample of each run.

### Filter order switches with reporting rate

Firmware uses a **second-order Butterworth (`butter2`)** above a threshold (~40 Hz reporting rate) and falls back to **first-order (`butter1`)** below that — there's a minimum cutoff ratio below which second-order behaves badly. The host-side `Butter2` calc is always second-order.

### Floating-point precision

The C and A coefficient arrays are **reversed at filter construction time** so that the update's dot product sums smallest-to-largest. This order minimizes roundoff error in IEEE-754 arithmetic. The code does *not* compile with `-ffast-math`; the compiler is allowed to reorder sums only where it's safe, and the filter's correctness depends on that.

## Stage 6: Decimation

Decimation reduces the rate from 33 kHz (internal) to whatever reporting rate the user chose. In the firmware, this is **implicit**: the SysTick ISR reads the latest value from an atomic array that the sampling ISR has been overwriting at the ADC rate. Samples between SysTick firings are silently discarded.

## Time synchronization

Deimos achieves **sub-microsecond time alignment** between peripherals without specialized hardware (no PTP grandmaster, no GPS disciplined oscillator) by combining three mechanisms:

- **LOCAL:** Each peripheral adjusts its sampling timing via a closed-loop software controller to align with the control program's cycles.
- **GLOBAL:** The control program runs on system monotonic clocks that are gradually disciplined by NTP.
- **TOTAL:** Together, these converge to sub-microsecond alignment in a few seconds, on any commodity ethernet switch.

The practical consequence: you can correlate data across multiple peripherals (up to 8 units, ~200 channels total) without investing in a dedicated timing infrastructure.

## Where the code lives

If you want to read the actual implementation:

- **Firmware sampling + filtering:** `firmware/deimos_daq_rev7/src/board/subsystems/sampling.rs`
- **Firmware operating loop (SysTick decimation):** `firmware/deimos_daq_rev7/src/board/subsystems/operating.rs`
- **Host-side Butter calc:** `software/deimos/src/calc/butter.rs`
- **Filter primitives (FIR, IIR, Lagrange):** the external [`flaw`](https://crates.io/crates/flaw) crate
- **Firmware invariants and file:line references:** [`.claude/rules/signal-pipeline.md`](../.claude/rules/signal-pipeline.md)
