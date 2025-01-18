"""
Processing chain for DIN-43-760 / IEC-751 data:
                                                           table definitions for rust,
           pdf table       |       copy to CSV         |   with corrected misprint,
                           |   and correct misprint    |   interpolated on to regular resistance grid
Website => "RTD table.pdf" => pt100_rtd_din-43-760.csv => pt100_rtd_din-43-76_rust.txt
      manual             manual                     scripted

The resistance values at 706C and 798 degC are misprinted in the PDF I used, which is apparent from
the fact that their values are not monotonically increasing and do not match the `delta` column.
They have been corrected in the CSV document using the `delta` column to calculate the value
at those temperatures from the value at the previous temperature and the `delta` column.

References
* DIN-43-760 (IEC-751) PT100 RTD tables, acquired as PDF
 from https://web.mst.edu/cottrell/ME240/Resources/Temperature/RTD%20table.pdf
"""

from pathlib import Path

import pandas as pd
import numpy as np
from interpn import MulticubicRectilinear

import matplotlib.pyplot as plt

here = Path(__file__).parent

df = pd.read_csv(here / "pt100_rtd_din-43-760.csv", header=0, index_col=0)

c_offs = 273.15  # conversion from degC to K

temps_orig = df.index.values.astype(np.float64) + c_offs  # [K] from [degC]
resistances_orig = df["Ohms"].values  # [ohm]

# Interpolate on to regular resistance grid so that the in-the-loop interpolation is fast
#   Use more grid points in interpolated version to maintain accuracy
#   across regions with changing spacing in the original data
ngrid = 2 * len(temps_orig)

temp_interpolator = MulticubicRectilinear.new([resistances_orig], temps_orig, linearize_extrapolation=True)
resistances = np.linspace(np.min(resistances_orig), np.max(resistances_orig), ngrid, endpoint=True)  # [ohm]
temps = temp_interpolator.eval([resistances])  # [K]
temps = [float(x) for x in temps]

# Save data to disk in little-endian binary format
with open(here / "pt100_rtd_din-43-76_rust.txt", "w") as f:
    step = float(resistances[1] - resistances[0])
    start = float(resistances[0])
    n = len(resistances)
    f.write('// [ohm] probe resistance, regular grid\n')
    f.write(f"const PROBE_RESISTANCE_START_OHM: f64 = {start};  // [ohm]\n")
    f.write(f"const PROBE_RESISTANCE_STEP_OHM: f64 = {step};  // [ohm]\n")
    f.write(f"const PROBE_RESISTANCE_N: usize = {n};\n\n")

    f.write(
        '// [K] Temperature at the probe, 1d table mapping resistance => probe_temp.\n'
    )
    f.write("#[rustfmt::skip]\n")
    f.write(
        f"const PROBE_TEMP_K: [f64; {len(temps)}] = "
        + str(temps)
        + ";  // [K]\n"
    )

plt.plot(resistances, temps, color='k', linewidth=2)
plt.xlabel("resistance [ohm]")
plt.ylabel("temperature [K]")
plt.show()
