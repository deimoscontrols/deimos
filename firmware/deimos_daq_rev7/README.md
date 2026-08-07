# Deimos DAQ firmware

The calibration image is selected by `firmware/flash.py` before each build:

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
