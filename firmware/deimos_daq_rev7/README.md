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

Normal rev7 operation publishes one coherent engineering snapshot from both
the Deimos UDP and Modbus/TCP paths. Its `sample_time_ns` field is the board
timestamp immediately before the first ADC conversion in the associated raw
sample group; it does not include a correction for digital-filter group delay.
The host converts this integer timestamp to `f64` with the other controller
outputs.

Modbus/TCP is available on port 502 only after a generated calibration is
embedded. The first supported read or write received while binding selects
Modbus operation; a read-only client therefore receives the safe output
defaults at the default 10 Hz rate. The complete synchronized register layout
and timeout behavior are documented in
[`plans/MODBUS_REGISTER_MAP.md`](../../plans/MODBUS_REGISTER_MAP.md).

The engineering snapshot is an intentionally breaking rev7 protocol change.
Flash the corresponding firmware and update the controller software together;
there is no legacy rev7 packet decoder.
