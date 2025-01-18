# Deimos

Realtime data acquisition and controls ecosystem, including hardware, firmware, and software.

The goals of this ecosystem are:

* Tightly-integrated sensor frontends
    * Amplifiers and filters for thermocouples, RTDs, 4-20mA, etc
    * Comparators for counters, encoders, frequency inputs, etc
* _Fully independent_ input and output channels
    * No overlapping resources; every advertised channel is available _at the same time_ .
* Full reassertion of state at each control cycle
    * A change in state is never missed permanently, regardless of packet loss
    * This eliminates network congestion storms, because packet retrying is not required
* Zero-calibration operation
    * NIST-traceable calibrations are provided, but not required
* Control program without required root/admin access or drivers
* Run on standard networking hardware
    * Plain ethernet comms; no PTP hardware required
    * Sub-microsecond distributed time-synchronization is achieved by application-level active control
* Semantic versioning for both hardware and software to prevent unexpected breaking changes
* 100% end-to-end open-source hardware, firmware, and software under permissive licenses
    * Do as you please. Hack, modify, copy, link, and extend without concern for retaliation
    * Implement your own hardware, plugins, or data integrations with full visibility into the system
    * A rising tide lifts all boats. Trust that a gift provided to the universe will find its way back to all of us

Notable anti-goals:

* Copyright mongering
    * Anyone with the capability and motivation to understand the design could copy it easily, with or without the design files
        * Electronics reverse-engineering labs are fast, cheap, and well-established
        * Decompilers, especially for simple firmware programs, are similarly well-established, diverse, and profoundly effective
    * Restricting information harms the community without providing any significant benefit to anyone
* "Internet-of-Things" (IoT) functionality
    * The devices in this ecosystem are _not_ IoT devices, despite using networked comms.
    * Security is taken as a physical concern: control networks must be physically isolated from unauthorized access, and unlike IoT devices, zero consideration is given to preventing or mitigating unauthorized access within the control network.
* Over-the-Air (OTA) firmware updates
    * The devices in this ecosystem are finished products as-shipped.
    * No mechanism is provided for performing firmware updates, except that every version of the firmware is provided as an open-source software package, the debug/flash port is populated on each device, and guidance is available for manually flashing updated or customized firmware.
    * Much of the data processing logic is performed in software on the control machine, and updates may be provided for that software.
* Performative complexity
    * Whenever possible, the complexity of a system will be reduced to the minimum level achievable in the available time. The final result should appear "obvious" and "trivial"; when this is not achievable, irreducible complexity should be accompanied by an appropriate body of documentation.
    * Instructions will be as simple as possible, avoiding the use of obfuscating or aggrandizing jargon.
    * Where complexity is found, it should be emergent complexity grown as a system from simple parts that can be understood easily on their own in order to understand the whole.
* Skimping on bits
    * A single unexpected integer overflow event can be more costly than a lifetime of savings from using a smaller type

# Hardware Peripherals

| Model | Revision | I/O Medium | Samplerate | Input Capabilities | Output Capabilities |
|------|----------|------------|------------|--------------------|---------------------|
| Analog I | 4.0.x | UDP over IPV4<br> on ethernet with LAN-only (non-routable) MAC address | 5Hz-5kHz roundtrip<br>5Hz-10kHz stream<br>Performance depends on network and host machine | 5x 4-20mA<br>5x Pt100 RTD<br>4x K-Type Thermocouple<br>3x 0-2.5V<br>1x Quadrature Encoder<br>1x Counter<br>1x Frequency<br>1x Frequency + Duty Cycle | 4x PWM (1Hz-1MHz) |

# Data Integrations

Data integration implementations perform I/O and database transactions on a separate core to avoid blocking the main control loop, unless no separate core is available.

| Target | I/O Medium | Notes |
|--------|--------|-------|
| CSV    | Disk | Fixed-width row format.<br>Wrap, split, or terminate at end of pre-sized file. |
| TimescaleDB (postgres) | TCP or unix socket | Create table & schema or reuse existing.<br>Insert individual rows or write buffered batches for increased total ingestion rate. |

# Calculation Functions

| Name | Description | Notes |
|------|-------------|-------|
| TcKtype | K-type thermocouple tables with cold-junction correction | Based on ITS-90 tables |
| RtdPt100 | 100-ohm platinum RTD temperature-resistance tables | Based on DIN-43-760 and ITS-90 |
| Pid | Simple proportion-integral-derivative controller with primitive saturation anti-windup protection | |
| InverseAffine | Function for inverting amplifier gain and offset to recover estimate of input voltage | |
| Constant | A constant value that takes no inputs | |
| Sin | Sine-in-time function for generating test outputs | |
| Affine | Slope-and-offset function | |

# Versioning

To prevent unexpected breaking changes, both software and hardware use semantic versioning and, whenever possible, use procedural semver linting to avoid unexpected or unintuitive breakage.

Semantic versioning of hardware is not yet a well-established practice. Here, it is taken to cover both hardware and accompanying firmware, and their interfaces:

* Major version: Potentially breaking changes
    * Examples: Removing a channel, increasing filter phase lag, narrowing samplerate range, or changing packet interchange format, reducing overvoltage tolerance
        * Adding a channel is also a breaking change if it affects packet interchange format
    * Notable non-examples: Changing connector types for sensors without a noise-level or thermoelectric voltage sensitivity, changing model number
* Minor version: Backward-compatible changes
    * Examples: Adding a channel, increasing samplerate range, decreasing filter phase lag, increasing overvoltage tolerance
* Patch version: Forward- and backward- compatible changes
    * Examples: Updating silkscreen, changing non-sensitive connector types, swapping non-sensitive components such as decoupling capacitors

Both major and minor hardware versions are accompanied by new hardware project files. Patch hardware versions are applied to the existing hardware project files.

# License

Licensed under either of

- 0-Clause BSD License ([LICENSE-0BSD](LICENSE-0BSD.txt))
- Apache License, Version 2.0, ([LICENSE-APACHE](LICENSE-APACHE.txt) or http://www.apache.org/licenses/LICENSE-2.0)

at your option.
