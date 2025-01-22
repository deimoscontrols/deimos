# Deimos - Shared Module

Packet formation and parsing, calc functions, and application-level 
peripheral definitions for the Deimos data acquisition ecosystem.

See the [project readme](https://github.com/deimoscontrols/deimos/blob/main/README.md) for contact details as well as commentary about
the goals and state of the project.

This library includes a `no-std` embedded portion which defines the interchange format between the
control machine and the hardware peripherals, as well as an application-level portion which defines
the control machine's representation of those hardware peripherals.

As the name suggests, the `no-std` embedded portion of the library is shared with both the peripheral firmware
and the control program so that critical data structures and constants maintain parity at all times.

Each peripheral state that involves communication with the control machine (binding, configuring, and operating)
has one input and one output packet format.

Each peripheral's application-level representation also includes a set of standard calcs
to be added to the control program's expression graph, which provide the minimum analysis
to convert measured voltages to intuitive physical values.
