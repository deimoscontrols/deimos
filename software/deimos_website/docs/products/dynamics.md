---
hide:
- toc
---

## :material-chart-bell-curve:{ .lg .middle } Frontends

Each analog input has a filter pipeline that improves data quality while
accommodating bandwidth requirements for common applications.

Digital IIR filters are adapted internally to target a cutoff frequency of 0.4x the
cycle rate up to a 4500Hz cycle rate.

Above 4500Hz cycle rate, the digital antialiasing filter is disabled
and antialiasing relies on frontend analog cutoff.

See [the frontends page](./frontends.md) for dynamic response
charts for each frontend type for a given reporting rate.

| Kind | Frontend Cutoff | Target Use-Cases |
|------|----------|------------------|
| Board temperature | 100Hz | - Cold-junction correction.<br>- System health. |
| Bus voltage | No frontend | System health. |
| Bus current | No frontend | System health. |
| 0-2.5V | 3kHz | General-purpose |
| 0-15V | 3kHz | 0-10V sensor inputs. |
| ±35mV | 1kHz | Low-noise measurement of small signals. |
| 3-Wire Resistance | 3kHz | - Pt100 RTD temperature.<br>- 100-ohm strain gauges.|
| K-type Thermocouple | 1kHz | Fast responses and high maximum temperature. |
| 4-20mA | 3kHz | - Pressure transducers.<br>- Valve angle feedback.

  <div class="bode-plot-frame">
    <div class="bode-plot-loader" aria-label="Loading Bode plot"></div>
    <iframe
      class="bode-plot"
      src="../../assets/rev7_bode_100hz_frontend_light.html"
      data-theme-src-dark="../../assets/rev7_bode_100hz_frontend_dark.html"
      data-theme-src-light="../../assets/rev7_bode_100hz_frontend_light.html"
      title="Deimos DAQ Rev7 100Hz analog frontend Bode plot"
      loading="lazy"
      onload="this.closest('.bode-plot-frame').classList.add('bode-plot-frame--loaded')"
    ></iframe>
  </div>

  <div class="bode-plot-frame">
    <div class="bode-plot-loader" aria-label="Loading Bode plot"></div>
    <iframe
      class="bode-plot"
      src="../../assets/rev7_bode_1khz_frontend_light.html"
      data-theme-src-dark="../../assets/rev7_bode_1khz_frontend_dark.html"
      data-theme-src-light="../../assets/rev7_bode_1khz_frontend_light.html"
      title="Deimos DAQ Rev7 1kHz analog frontend Bode plot"
      loading="lazy"
      onload="this.closest('.bode-plot-frame').classList.add('bode-plot-frame--loaded')"
    ></iframe>
  </div>

  <div class="bode-plot-frame">
    <div class="bode-plot-loader" aria-label="Loading Bode plot"></div>
    <iframe
      class="bode-plot"
      src="../../assets/rev7_bode_3khz_frontend_light.html"
      data-theme-src-dark="../../assets/rev7_bode_3khz_frontend_dark.html"
      data-theme-src-light="../../assets/rev7_bode_3khz_frontend_light.html"
      title="Deimos DAQ Rev7 3kHz analog frontend Bode plot"
      loading="lazy"
      onload="this.closest('.bode-plot-frame').classList.add('bode-plot-frame--loaded')"
    ></iframe>
  </div>

  <div class="bode-plot-frame">
    <div class="bode-plot-loader" aria-label="Loading samplerate plot"></div>
    <iframe
      class="bode-plot"
      src="../../assets/rev7_samplerate_light.html"
      data-theme-src-dark="../../assets/rev7_samplerate_dark.html"
      data-theme-src-light="../../assets/rev7_samplerate_light.html"
      title="Deimos DAQ Rev7 internal samplerate versus reporting rate"
      loading="lazy"
      onload="this.closest('.bode-plot-frame').classList.add('bode-plot-frame--loaded')"
    ></iframe>
  </div>
