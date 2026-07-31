# Deimos DAQ rev7 firmware

The calibration image is selected by `firmware/flash.py` before each build:

- If the assigned unit has a generated
  `software/deimos_website/docs/records/DeimosDaqRev7/<serial>/calibration.bin`,
  the script copies it to `static/calibration.in` before building. The completed
  artifact has `firmware_calibrated = 1`.
- If that unit-specific `calibration.bin` is absent, the script removes any
  previously staged `static/calibration.in`. `build.rs` then embeds identity
  affine coefficients and sets `firmware_calibrated = 0`. This is the image to
  flash immediately before a calibration run.

The controller enforces the operational convention: calibration collection
requires the identity image, while normal Deimos operation requires the final
calibrated image. Consequently, a stale calibrated image cannot accidentally be
calibrated a second time, and an identity image cannot silently be used for
normal operation.

For an identity calibration run, make sure the assigned unit's generated
`calibration.bin` is absent (move an existing artifact aside rather than
deleting it), then run the flash script. For the final calibrated image, leave
the generated file in its records directory. In both cases:

```sh
uv run python firmware/flash.py
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
