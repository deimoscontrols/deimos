# Hardware

Deimos hardware is split into two parts: a **Base module** that holds the MCU, power regulation, ethernet, and a precision voltage reference, plus a swappable **Frontend module** that carries the sensor conditioning circuits. There is deliberately no backplane — the design favors one integrated pair over a fully modular chassis. *Modularity is a tool, not a goal.*

Everything (schematics, PCB files, enclosures, firmware, toolchain) is open-source under permissive licenses.

For a full list of available peripherals and channel counts, see the table in the [top-level README](../README.md#hardware-peripherals). This document focuses on **how the sensor frontends actually work**.

## The ADC and burst scanning

The analog-to-digital converter (ADC) is 16-bit with ~15 ppm resolution. Rather than sampling each channel at the user-requested rate directly, the ADC runs a **burst scan** at 33 kHz per channel — about 660 kS/s aggregate across all channels, with a ~330 ns sample-and-hold window divided into 8 batches. The firmware then filters and decimates down to whatever reporting rate the user asked for (5 Hz – 5 kHz).

Burst scanning is what makes the downstream signal pipeline work. See [`signal-pipeline.md`](signal-pipeline.md) for how the raw bursts become clean samples.

## Voltage frontend

Each voltage channel has its own **instrumentation amplifier (INAMP)** with a single precision gain-set resistor, a **Sallen-Key anti-aliasing filter** (second-order, contributing to an overall third-order response together with a first-order filter at the ADC input), and its own ground pin for noise rejection.

Two placement choices worth calling out:

- **Amplifier first, filter second.** If the filter went before the amplifier, it would pick up and amplify the amp's own offset error. Amplifying first, then filtering, keeps the offset contribution bounded.
- **Per-channel ground.** Sharing a single ground pin across channels lets return currents from one measurement corrupt another. A separate ground per channel removes that coupling.

Gain ranges supported depend on the peripheral model but typically include 0–2.5 V, 0–15 V, a ~27× range, and a ~660× range.

## Over-voltage protection (OVP)

The OVP circuit protects the ADC input from accidental connections of up to 40 V. Traditional protection diodes (like the BAV-199) leak current that scales poorly with temperature, which would corrupt precision measurements. Deimos uses **JFET gates as low-leakage diodes** instead, keeping leakage below 0.01 %.

**Known constraint:** the OVP rail currently must be ≤ 2.9 V because the ADC's absolute-maximum input is 3.6 V. A fix is planned in a future board revision.

## 4-20 mA current-loop frontend

Industrial sensors commonly report their measurement as a current between 4 mA and 20 mA — the current-loop standard is robust against noise over long wire runs. Deimos reads these by passing the loop current through a **precision 75 Ω shunt resistor** and measuring the voltage drop with the ADC.

The 24 V excitation supply to the sensor is routed through an **LDO regulator** that provides short-circuit protection — a damaged sensor cable can't take the whole peripheral down. Both 2-wire and 3-wire sensor connections are supported.

## Thermocouple frontend (K-type)

Thermocouples work by generating a small voltage that depends on the temperature *difference* between a "hot junction" (the sensor tip) and a "cold junction" (where the thermocouple wires meet the circuit board). To get an absolute temperature, the cold junction's temperature has to be measured and corrected for — this is **cold-junction compensation (CJC)**.

Deimos mounts an **RTD between the two thermocouple connectors on the PCB** to read the cold junction directly. A few details that matter in practice:

- Connector materials must match the thermocouple alloy, or the connector itself becomes a second unintended thermocouple.
- The amplifier needs an offset voltage to allow readings below 0 °C.
- Open-circuit behavior (what happens when a thermocouple is disconnected) must be handled gracefully.

See [`signal-pipeline.md`](signal-pipeline.md) for the software side of the CJC computation, and [`interpn.md`](interpn.md) for how the thermocouple reference tables (ITS-90) are interpolated.

## RTD (Pt100) frontend

A resistance temperature detector (RTD) changes its resistance predictably with temperature. The traditional way to measure small resistance changes is a **Wheatstone bridge**, which needs four precision resistors (plus an excitation source) and has error terms in the denominator of its transfer function.

Deimos instead uses **dual 250 µA current sinks**, which needs only three precision resistors and no calibrated pairs. Advantages:

- Less uncertainty (no error terms in the denominator).
- Better noise performance.
- Fewer components.
- Automatic compensation for wire resistance.

Tradeoff: slightly slower response than a bridge, so bridges still make sense for high-speed strain gauges. For steady-state temperature measurement the dual-sink approach is the better pick.

## Supported channel types

The current DAQ peripheral (see [root README](../README.md#hardware-peripherals) for exact counts per model) supports some combination of:

- Voltage (multiple ranges)
- 4-20 mA current loop
- K-type thermocouple with on-board CJC
- Pt100 RTD
- DAC output (analog)
- PWM output
- Frequency input
- Pulse counter
- Quadrature encoder

## Planned additions

Valve drivers, 4-20 mA *outputs*, and SPI/I²C interfaces are on the roadmap.

## Toolchain used to build the hardware

KiCad 8 (with Hierarchical PCB and replicate-layout plugins), Protocase Designer for enclosures, FreeCAD 1.0.2 for mechanical, LTSpice for circuit simulation. Firmware builds are bare-metal Rust 1.89+.
