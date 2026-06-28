---
hide:
- toc
---


# Deimos DAQ - Overview

## :material-graph-outline:{ .lg .middle } Overview

![image](../assets/DSC02285.jpg)

The Deimos DAQ boasts a set of 22 input channels and 6 output channels,
all available together and operated simultaneously on every cycle.

Fully open-source, the Deimos DAQ's design files, firmware, and control program can all be
found under permissive licenses in the [Deimos project repository](https://github.com/deimoscontrols/deimos).

| Feature | Performance |
|---------|-------------|
| Comm. Medium | Ethernet. |
| Cycle Rate | 5Hz - 5000Hz, round-trip control with full state reassertion. |
| Multi-Unit Time Sync | ~1 microsecond (100ns typ.) |
| Voltage Reference | 0.02% accuracy, 2.5V, ultra-low thermal sensitivity. |
| ADCs | 16-bit SAR, self-calibrating on every bootup. |
| Internal Samplerate | 33kHz burst-scanning w/ synthetic simultaneous sampling. |
| Onboard Filtering | Every analog channel has:<br>- Active analog filter.<br>- Digital anti-aliasing filter.<br>- Digital sample synchronization filter. |
| Power Supply | 24V DC 1A |
