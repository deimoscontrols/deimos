# Deimos - Shared Module

Packet formats and byte-serialization for the Deimos data acquisition ecosystem.

See the [project readme](https://github.com/deimoscontrols/deimos/blob/main/README.md) for contact details as well as commentary about
the goals and state of the project.

This library is shared with both the peripheral firmware and the control program so that critical
data structures and constants maintain parity at all times.

## Rev7 Bode Plot Export

The `rev7_bode` example writes interactive HTML and static SVG plots. Static SVG export uses
Plotly's WebDriver-backed exporter, so Chrome/Chromium and chromedriver must be discoverable before
Cargo builds `plotly_static`.

Use the wrapper when generating SVGs on machines where Chromium may be installed outside `PATH`,
including Flatpak or Snap installs:

```sh
software/deimos_shared/examples/export_rev7_bode.sh light target/rev7_bode_export
```

The wrapper sets `BROWSER_PATH` and, when available, `WEBDRIVER_PATH` before invoking Cargo.
