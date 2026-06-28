---
hide:
- toc
---

# Use-Case: Turbine Engine R&D

In this example, two Deimos Analog I units provide simultaneous remote monitoring and actuation for both the fluid supply
and turbine engine test article from a test control room at a third location.

The **broad set of inputs and outputs** of each DAQ support measurement of temperatures, pressures, flowrates, and speeds
along with PWM and voltage outputs in a single module that is **simple, inexpensive, and alleviates configuration burden**
compared to conventional fragmented backplane systems.

![Turbine engine R&D use-case diagram](../assets/usecase_turbine.svg){ .zoomable-image }

This allows respecting a generous minimum safe distance between three mutually-incompatible areas

* Stored flammables and pressurant (the tank farm)
* Stored energy and ignition sources (the test article)
* Personnel and data storage (the control room)

within a single time-synchronization domain that supports both **precise performance measurements** under nominal operating conditions
and **rapid resolution of anomaly investigations**.
