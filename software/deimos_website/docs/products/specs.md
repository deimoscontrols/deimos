# Deimos DAQ - Specs

Specifications refer to performance including applied calibrations.<br>
Values are preliminary and may be adjusted as further test data is collected.

| Feature | Performance |
|---------|-------------|
| Comm. Medium | Ethernet. |
| Power Supply | 24V DC 1A |
| Cycle Rate | 5 Hz - 8 kHz in Deimos synchronized mode (UDP). <br>5 Hz - 500 Hz over Modbus/TCP. |
| Multi-Unit Time Sync | ~1 microsecond (100ns typ.) |
| Sample Jitter | 0.01% of Δt |
| Voltage Reference | 0.02% accuracy, ultra-low thermal sensitivity. |
| ADCs | 16-bit SAR, self-calibrating. |
| Internal Samplerate | Nominal 9kHz delayed-simultaneous sampling.<br>Adjusted to synchronize with the reporting rate.<br>See [dynamics page](./dynamics.md#internal-samplerate-variation) for details. |
| Onboard Filtering | Every analog channel has:<br>- Active analog filter.<br>- Digital anti-aliasing filter.<br>- Digital sample synchronization filter. |

## :material-controller-classic:{ .lg .middle } Outputs

| Kind | Range | Resolution | Notes |
|------|-------|------------|-------|
| :material-square-wave: 4x PWM  | 1Hz-100kHz | 16-bit | Independent frequencies and duty cycles.<br>40Ω termination. |
| :material-square-wave: 4x GPIO Output |  | 1-bit | 40Ω termination. |
| :material-sine-wave: 2x DAC (Voltage) | 0-2.5V | 12-bit (0.6mV) | Buffered & self-calibrating. |

## :material-ear-hearing:{ .lg .middle } Inputs

| Kind | Range | Accuracy | Resolution | Notes |
|------|-------|----------|------------|-------|
| :material-lightning-bolt: 2x Voltage, 1x Gain | 0-2.5V | 0.04% | 38uV | Single-ended.<br>40V tolerance.  |
| :material-lightning-bolt: 2x Voltage, (1/6)x Gain | 0-15V | 0.02% | 228uV | Single-ended.<br>40V tolerance.<br>12kOhm divider; 0.6mA max leakage. |
| :material-lightning-bolt: 2x Voltage, 25.7x Gain | ±35mV | 0.04% | 1.5uV | Single-ended.<br>40V tolerance. |
| :material-fire: 2x K-Type Thermocouple | 73-1543K | 0.5K near room temp | 0.03K | Cold-junction compensated.<br>Material-matched connector. |
| :material-snowflake: 3x 3-Wire Resistance (RTD, strain, etc) | 73-1073K | 0.1K near room temp | 0.02K | Specs refer to use with Pt100 RTD.<br>Also compatible with 100-ohm strain gauges. |
| :fontawesome-solid-gauge-high: 4x 4-20mA | 0-33mA | 0.04% | 0.8uA | 24V excitation.<br>2 or 3-wire.<br>Short-circuit protected. |
| :material-square-wave: 2x GPIO Input |  | 1-bit |  |
| :material-square-wave: 2x Frequency | 400Hz-1MHz | 100ppm | 16-bit | |
| :material-square-wave: 1x Pulse Counter | 400Hz-1MHz |  | 1 | 64-bit accumulator |
| :material-square-wave: 1x Encoder | |  | | Signed 64-bit accumulator, forward/backward counting. |
| :material-thermometer: Diagnostics | ||| Bus current.<br>Bus voltage.<br>Board / cold-junction temp. |

## :material-gauge-full:{ .lg .middle } Benchmarks

The operating space of the Deimos DAQ is one-dimensional: everything is a function of cycle rate.

The following benchmark data is collected using a consumer laptop host machine
running ubuntu linux with a low-latency usb-to-ethernet adapter.

Networking hardware with added latency can limit maximum cycle rate. As a result,
this system performance is typical, but is not guaranteed.

<div class="bode-plot-frame rate-sweep-plot-frame">
  <div class="bode-plot-loader" aria-label="Loading rate sweep benchmark plot"></div>
  <iframe
    class="bode-plot"
    src="../../assets/rev7_rate_sweep_light.html"
    data-theme-src-dark="../../assets/rev7_rate_sweep_dark.html"
    data-theme-src-light="../../assets/rev7_rate_sweep_light.html"
    title="Deimos DAQ Rev7 cycle-rate benchmark"
    loading="lazy"
    onload="this.closest('.bode-plot-frame').classList.add('bode-plot-frame--loaded')"
  ></iframe>
</div>
