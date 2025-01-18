"""
Processing chain for ITS-90 data:

           raw text    |     raw text converted to     | truncated data, w/ gaps  | symmetrized-ish     | table definitions for rust,
           format      |     bad csv and left-aligned  |   in actual CSV fmt      |                     | inverted via cubic interpolation
Website => type_k.tab => type_k_its90_csv_from_tab.csv => type_k_its90_trunc.csv => type_k_its90_mod.csv => its90*.txt
      manual        manual                           manual                  scripted                scripted

The first and last rows of the raw tables, which have incomplete data, are truncated.

For hot junction temperatures < 0C and > 0C, the cold junction error to the 0C-cold-junction
reading is mirrored about 0C cold junction temp in order to fill missing data with the best
available guess. At hot junction temp == 0C, both high and low cold junction temp table entries
are present, and are used directly.

This gives tables that are symmetric about 0C cold junction temperature everywhere _except_
at hot junction temp == 0C, where there is a very slight asymmetry present.

References
* [1] “ITS-90 Thermocouple Database, How to Use the Database.”
      Accessed: May 19, 2024. [Online]. Available:
      https://srdata.nist.gov/its90/useofdatabase/use_of_database.html#Coefficients%20Tables
"""

from pathlib import Path

import pandas as pd
import numpy as np
from interpn import MulticubicRectilinear

import matplotlib.pyplot as plt

here = Path(__file__).parent

df = pd.read_csv(here / "type_k_its90_trunc.csv", header=0, index_col=0)

# Symmetrize everything except hot junc temp == 0C
hjtemps_C = df.index.values
cjtemps_C = df.columns.values

for hjtemp_C in hjtemps_C:
    if int(hjtemp_C) < 0:
        # symmetrize low-temperature part
        for cjtemp_C in cjtemps_C:
            if int(cjtemp_C) < 0:
                cjerr = df.loc[hjtemp_C, cjtemp_C] - df.loc[hjtemp_C, "0"]
                df.loc[hjtemp_C, str(-int(cjtemp_C))] = df.loc[hjtemp_C, "0"] - cjerr
    elif int(hjtemp_C) == 0:
        pass  # Already populated
    elif int(hjtemp_C) > 0:
        # symmetrize high-temperature part
        for cjtemp_C in cjtemps_C:
            if int(cjtemp_C) > 0:
                cjerr = df.loc[hjtemp_C, cjtemp_C] - df.loc[hjtemp_C, "0"]
                df.loc[hjtemp_C, str(-int(cjtemp_C))] = df.loc[hjtemp_C, "0"] - cjerr
    else:
        raise NotImplementedError  # Just in case

# Save symmetrized-ish version for reference
df.to_csv(here / "type_k_its90_mod.csv")

# Generate rust table inputs
# ITS-90 cold junction temps are inverted,
# so we have to negate the celsius delta
# and then reverse the list so that it's sorted ascending
c_offs = 273.15  # conversion from degC to K
cjtemps = np.array(
    [(-float(x)) + c_offs for x in df.columns.values]
)[::-1]  # [K] from degC, cold junc. temp
hjtemps = np.array(
    [float(x) + c_offs for x in df.index.values]
)  # [K] from degC, hot junc. temp
voltages = df.values[:, ::-1] / 1e3  # [V] from mV, sensed voltages

# The tables map two temperatures to a voltage,
# but we need to map the two things we know (voltage and cold junction temp)
# to the one that we don't (hot junction temp) by inverting the tables.
#
# Since the voltage is monotonic in hot junction temperature for a given
# cold junction temperature, we can do this inversion by interpolating the
# hot junction temperatures on to a consistent voltage grid.

ngrid = 200
vmin = np.min(voltages)
vmax = np.max(voltages)
vgrid = np.linspace(
    vmin, vmax, ngrid
)  # Original tables have 163 hot junction temp points
hjtemps_interpolated = np.zeros((ngrid, len(cjtemps)))
for i, cjtemp in enumerate(cjtemps):
    v = voltages[:, i]
    assert len(v) == 163
    hjinterpolator = MulticubicRectilinear.new(
        [v], hjtemps, linearize_extrapolation=True
    )
    hjtemps_interpolated[:, i] = hjinterpolator.eval([vgrid])

# Save data to disk in little-endian binary format
with open(here / "its90_cold_junc_tables_rust.txt", "w") as f:
    step = cjtemps[1] - cjtemps[0]
    start = cjtemps[0]
    n = len(cjtemps)
    f.write('// [K] temperature at the connector "cold junction", regular grid\n')
    f.write(f"const COLD_JUNCTION_TEMP_START_K: f64 = {start};  // [K]\n")
    f.write(f"const COLD_JUNCTION_TEMP_STEP_K: f64 = {step};  // [K]\n")
    f.write(f"const COLD_JUNCTION_N: usize = {n};\n\n")

    step = vgrid[1] - vgrid[0]  # this is less float error than taking the mean diff
    start = vgrid[0]
    n = ngrid
    f.write(
        "// [V] sensed voltage at (or, hopefully, close to) the connector, regular grid\n"
    )
    f.write(f"const SENSED_VOLTAGE_START_V: f64 = {start}; // [V]\n")
    f.write(f"const SENSED_VOLTAGE_STEP_V: f64 = {step};  // [V]\n")
    f.write(f"const SENSED_VOLTAGE_N: usize = {n};\n\n")

    f.write(
        '// [K] Temperature at the probe ("hot junction"), 2d table mapping (sensed_voltage, cold_junction_temp) => hot_junction_temp.\n'
    )
    f.write("#[rustfmt::skip]\n")
    f.write(
        f"const HOT_JUNCTION_TEMP_K: [f64; {hjtemps_interpolated.size}] = "
        + str([float(x) for x in hjtemps_interpolated.flatten()])
        + ";  // [K]\n"
    )


plt.imshow(hjtemps_interpolated.T, aspect="equal", extent=[0, 1, 0, 1])
cs = plt.contour(hjtemps_interpolated.T, colors="k", extent=[0, 1, 0, 1])
plt.show()
