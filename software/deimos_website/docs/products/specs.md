---
hide:
- toc
---

# Deimos DAQ - Specs

## :material-controller-classic:{ .lg .middle } Outputs

| Kind | Range | Resolution | Notes |
|------|-------|------------|-------|
| :material-square-wave: 4x PWM  | 1Hz-100kHz | 16-bit | Each channel has independent frequency and pulse width<br> 40 ohm termination. |
| :material-square-wave: 4x GPIO  |  | 1-bit |  |
| :material-sine-wave: 2x DAC (Voltage) | 0-2.5V | 12-bit | Buffered |

## :material-ear-hearing:{ .lg .middle } Inputs

| Kind | Range | Accuracy | Resolution | Notes |
|------|-------|----------|------------|-------|
| :material-lightning-bolt: 2x Voltage, 1x Gain | 0-2.5V | 0.04% | 38uV | Single-ended.<br>40V tolerance.  |
| :material-lightning-bolt: 2x Voltage, (1/6)x Gain | 0-15V | 0.02% | 228uV | Single-ended.<br>40V tolerance.<br>12kOhm divider; 0.6mA max leakage. |
| :material-lightning-bolt: 2x Voltage, 25.7x Gain | -40 to +57mV | 0.04% | 1.5uV | Single-ended.<br>40V tolerance. |
| :material-fire: 2x K-Type Thermocouple | 90-1600K | 0.5K near room temp | 0.03K | Cold-junction compensated.<br>Material-matched connector. |
| :material-snowflake: 3x 3-Wire Resistance (RTD, strain, etc) | 70-1200K | 0.1K near room temp | 0.02K | Specs refer to use with Pt100 RTD.<br>Also compatible with 100-ohm strain gauges. |
| :fontawesome-solid-gauge-high: 4x 4-20mA | 0-33mA | 0.04% | 0.8uA | 24V excitation.<br>2 or 3-wire.<br>Short-circuit protected. |
| :material-square-wave: 2x GPIO  |  | 1-bit |  |
| :material-square-wave: 2x Frequency | 400Hz-1MHz | 100ppm | 16-bit | |
| :material-square-wave: 1x Pulse Counter | 400Hz-1MHz |  | 1 | 64-bit accumulator |
| :material-square-wave: 1x Encoder | |  | | Signed 64-bit accumulator, forward/backward counting. |
| :material-thermometer: Diagnostics | ||| Bus current.<br>Bus voltage.<br>Board / cold-junction temp. |
