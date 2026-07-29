# Deimos DAQ rev7 firmware

The calibration image is selected at build time:

- If `static/calibration.in` is absent, `build.rs` embeds identity affine
  coefficients and sets `firmware_calibrated = 0`. This is the image to flash
  immediately before a calibration run.
- A completed rev7 calibration run writes `calibration.bin` beside its JSON
  calibration record. Copy that file to `static/calibration.in`, rebuild, and
  flash the board to install the final coefficients. The completed artifact has
  `firmware_calibrated = 1`.

The controller enforces the operational convention: calibration collection
requires the identity image, while normal Deimos operation requires the final
calibrated image. Consequently, a stale calibrated image cannot accidentally be
calibrated a second time, and an identity image cannot silently be used for
normal operation.

For an identity calibration run:

```sh
rm firmware/deimos_daq_rev7/static/calibration.in
python firmware/flash.py
```

For the final calibrated image:

```sh
cp /path/to/calibration.bin firmware/deimos_daq_rev7/static/calibration.in
python firmware/flash.py
```

Both commands are run from the repository root. `flash.py` selects the board,
probe, serial number, and MAC address from `firmware/assignments.json`.
