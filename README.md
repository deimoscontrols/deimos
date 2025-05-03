# Deimos

| [Control Program](https://github.com/deimoscontrols/deimos/tree/main/software/deimos) |\|| [Packet Formats](https://github.com/deimoscontrols/deimos/tree/main/software/deimos_shared) |\|| [Firmware](https://github.com/deimoscontrols/deimos/tree/main/firmware) |\|| [Hardware Designs](https://github.com/deimoscontrols/deimos/tree/main/hardware) |
|-----------------|-|--------------|-|----------|-|----------|

Realtime data acquisition and controls ecosystem, including hardware, firmware, and software.

# Contact

| Email | Purpose |
|-------|---------|
| support@deimoscontrols.com | Questions about the hardware or software |

# Hardware Peripherals

| Model | I/O Medium | Sample/Control Rate | Input Capabilities | Output Capabilities |
|------|------------|------------|--------------------|---------------------|
| [Analog I 4.0.x](https://github.com/deimoscontrols/deimos/tree/main/hardware/boards/analog_i_rev4) | UDP over IPV4<br> on ethernet with LAN-only (non-routeable) MAC address | 5Hz-5kHz roundtrip<br><br>Performance depends on network and host machine | External:<br>5x 4-20mA (24V)<br>5x Pt100 RTD<br>4x K-Type TC<br>3x 0-2.5V<br>1x Encoder<br>1x Counter<br>2x Freq<br><br>Internal:<br>- Cold-junction RTD<br>- Bus current<br>- Bus voltage | 4x PWM (1Hz-1MHz, 3.3V) |

# Controller Comm. Media

Communication with peripherals is done via message passing on an arbitrary socket. New communication media can be used by implementing the `SuperSocket` trait.

While the controller can accommodate multiple communication media, a peripheral will typically only implement one method for reliability reasons.

| I/O Medium | Notes |
|------------|-------|
| UDP/IPV4 | Nominal peripheral I/O; compared to TCP, eliminates packet storm network instability.<br><br>The controller reasserts the full peripheral state on each cycle, so while UDP allows from some packet loss (typically 1e-4 or less), a change in peripheral state is never missed permanently. |
| Unix socket | Inter-process communication option for software peripheral mockups |

Several more socket implementations are planned, including TCP, UDP/IPV6, and thread channel message passing.

# Data Integrations

Data integration implementations perform I/O and database transactions on a separate core to avoid blocking the main control loop, unless no separate core is available.

| Target | I/O Medium | Notes |
|--------|--------|-------|
| CSV    | Disk | Fixed-width row format.<br>Wrap, split, or terminate at end of pre-sized file. |
| Polars<br>DataFrame | RAM | Wrap or terminate at end of preallocated columns.<br>Convert to dataframe at the end of run. |
| TimescaleDB (postgres) | TCP or unix socket | Create table & schema or reuse existing.<br>Insert individual rows or write buffered batches for increased total ingestion rate.<br>Set fixed retention duration. |

# Calculation Functions

| Name | Description | Notes |
|------|-------------|-------|
| SequenceMachine | A flexible state-machine where each state is defined by a time-dependent lookup table sequence with user-defined transition criteria | Allows implementation of essentially arbitrary scheduling and operational logic |
| TcKtype | K-type thermocouple tables with cold-junction correction | Based on ITS-90 tables |
| RtdPt100 | 100-ohm platinum RTD temperature-resistance tables | Based on DIN-43-760 and ITS-90 |
| Pid | Simple proportion-integral-derivative controller with primitive saturation anti-windup protection | |

... in addition to a variety of unremarkable math functions.

# Goals and Anti-Goals

The goals of this ecosystem are:

* Tightly-integrated sensor frontends
* _Fully independent_ input and output channels; every advertised channel is available _at the same time_
* Full reassertion of state at each control cycle
* Zero-calibration operation; NIST-traceable calibrations available, but not required
* Control program without required root/admin access or drivers
* Run on standard networking hardware with sub-microsecond time sync
* Semantic versioning for both hardware and software to prevent unexpected breaking changes
* 100% end-to-end open-source hardware, firmware, and software under permissive licenses

Notable anti-goals:

* Copyright mongering
* "Internet-of-Things" (IoT) functionality
* Over-the-Air (OTA) firmware updates
* Performative complexity
* Skimping on bits

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

* Apache License, Version 2.0, ([LICENSE-APACHE](LICENSE-APACHE.txt) or http://www.apache.org/licenses/LICENSE-2.0)
* MIT license ([LICENSE-MIT](LICENSE-MIT.txt) or http://opensource.org/licenses/MIT)

at your option.
