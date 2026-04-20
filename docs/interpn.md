# InterpN in Deimos

[InterpN](https://crates.io/crates/interpn) is a separate Rust library (also available on [PyPI](https://pypi.org/project/interpn/)) for **interpolation** — estimating values between entries of a lookup table. Deimos uses it whenever a raw measurement has to be converted through a sensor reference table.

## Why Deimos needs interpolation

Two of Deimos's sensor types rely on standardized reference tables:

- **K-type thermocouples** convert a voltage to a temperature via the **ITS-90** reference tables. The conversion also depends on the cold-junction temperature, so the effective table is two-dimensional.
- **Pt100 RTDs** convert resistance to temperature via the **DIN-43-760** tables — a simpler one-dimensional lookup.

In both cases a measurement rarely lands exactly on a table entry, so the firmware (or the host-side calc) interpolates between the nearest entries to produce a continuous output.

A few table-handling details matter for thermocouples specifically:

- The ITS-90 tables as published are **ill-posed and have missing data**. The build pipeline parses them, *anti-symmetrizes* them to fill gaps, inverts them, and produces a clean 2D correction table.
- Cold-junction correction is **not optional** — the measurement is just as sensitive to the cold-junction temperature as it is to the hot-junction temperature.

Pt100 interpolation is much simpler: a straightforward 1D lookup that you can validate against a handheld calibrator.

## What makes InterpN a good fit

- **No heap allocation.** InterpN runs in `no_std` / `no_alloc` Rust, which is required to use it from bare-metal firmware. The same library runs on microcontrollers and on the host.
- **Fast.** The core algorithm (Cube-to-Tree-to-Loop) flattens what would be an exponentially-growing cube of neighbor lookups into a linear sequence of operations, with a fixed stack footprint.
- **Accurate.** Careful ordering of floating-point operations yields far less roundoff than naive implementations.

Headline benchmarks versus SciPy (from the InterpN talk): up to **~320× faster** for linear interpolation, ~25× for cubic.

## Related calcs in Deimos

The [top-level README](../README.md#calculation-functions) lists the calcs that use these tables directly:

- `TcKtype` — K-type thermocouple with cold-junction correction (ITS-90)
- `RtdPt100` — Pt100 RTD temperature-resistance lookup (DIN-43-760 / ITS-90)

## Links

- **Crate:** [`interpn` on crates.io](https://crates.io/crates/interpn)
- **Python package:** [`interpn` on PyPI](https://pypi.org/project/interpn/)
- **Author's blog:** [jlogan.dev](https://jlogan.dev/)
