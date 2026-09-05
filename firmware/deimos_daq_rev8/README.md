# Deimos DAQ rev8 firmware

## Timer pinout

| Channel | Phase | Pin | Timer channel | Alternate function |
| --- | --- | --- | --- | --- |
| Encoder0 | A | PE9 | TIM1_CH1 | AF1 |
| Encoder0 | B | PE11 | TIM1_CH2 | AF1 |
| Encoder1 | A | PC6 | TIM8_CH1 | AF3 |
| Encoder1 | B | PC7 | TIM8_CH2 | AF3 |
| Encoder2 | A | PB6 | TIM4_CH1 | AF2 |
| Encoder2 | B | PB7 | TIM4_CH2 | AF2 |
| Encoder3 | A | PB4 | TIM3_CH1 | AF2 |
| Encoder3 | B | PB5 | TIM3_CH2 | AF2 |

PWM0 moves to PE6/TIM15_CH2 AF4. PWM1 through PWM3 remain on
PB14/TIM12_CH1 AF2, PB8/TIM16_CH1 AF1, and PB9/TIM17_CH1 AF1.

The calibration image is selected by `firmware/flash.py` before each build:

Calibration images contain both ADC and DAC affine coefficients. Images generated
before DAC calibration was added have a different binary layout and must be regenerated.

- If the assigned unit has a generated `calibration.bin` in its website records
  directory, the script copies it to `static/calibration.in` before building.
  The completed artifact has `firmware_calibrated = 1`.
- If that unit-specific `calibration.bin` is absent, the script removes any
  previously staged `static/calibration.in`. `build.rs` then embeds identity
  affine coefficients and sets `firmware_calibrated = 0`. This is the image to
  flash immediately before a calibration run.

The controller enforces the operational convention: calibration collection
requires the identity image, while normal Deimos operation requires the final
calibrated image. Consequently, a stale calibrated image cannot accidentally be
calibrated a second time, and an identity image cannot silently be used for
normal operation.

For an identity calibration run, use `--nocal`. This overrides an existing
generated calibration and removes any staged `static/calibration.in` before
building:

```sh
uv run python firmware/flash.py --nocal
```

For the final calibrated image, leave the generated file in its records
directory and run:

```sh
uv run python firmware/flash.py
```

Both commands are run from the repository root. `flash.py` selects the board,
probe, serial number, and MAC address from `firmware/assignments.json`.

## Encoder0 loopback validation on rev7 hardware

Run the validation through the ordinary nonblocking controller with the normal
rev8 firmware. Connect DO0 to Encoder0 A and DO1 to Encoder0 B, including the
corresponding signal reference/return connections required by the front-panel
wiring. DO0 and DO1 must remain manually writable rather than driven by calcs.

The runnable application-level procedure is
`software/deimos/examples/rev8_encoder_loopback_validation.py`. For example:

```sh
uv run --project software/deimos python \
  software/deimos/examples/rev8_encoder_loopback_validation.py
```

It uses only the existing nonblocking `RunHandle` read/write interface, emits
64 positive quadrature cycles, and requires `encoder0` to advance by 256 counts.
Every phase transition is synchronized to controller snapshots, and DO0/DO1
are restored low before the procedure returns.
