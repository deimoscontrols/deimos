# Deimos documentation

Deimos is an open-source **data acquisition and controls ecosystem** — hardware modules that measure physical quantities (temperature, voltage, current, etc.), firmware that digitizes and filters those measurements in real time, and a host-side control program that records data and closes control loops.

The repo's top-level [`README.md`](../README.md) is the reference index: it lists every hardware peripheral, communication medium, dispatcher, and calc function. These docs are the narrative counterpart — they explain **how the pieces work** and **how to get started**.

## Where to read next

| If you want to… | Read |
| --- | --- |
| Get a runnable example going in a few minutes | [`sdk-quickstart.md`](sdk-quickstart.md) |
| Understand the boards, sensor frontends, and ADC | [`hardware.md`](hardware.md) |
| Understand how a raw sample becomes a clean measurement | [`signal-pipeline.md`](signal-pipeline.md) |
| Understand how sensor correction tables are interpolated | [`interpn.md`](interpn.md) |

## Contributor notes

Some areas have in-repo notes aimed at contributors (or at Claude, when used with Claude Code):

- [`CLAUDE.md`](../CLAUDE.md) — build/test commands and HOOTL (Hardware-Out-Of-The-Loop) invocations
- [`.claude/rules/signal-pipeline.md`](../.claude/rules/signal-pipeline.md) — firmware-level filter conventions, file paths, and invariants
