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
from interpn import interpn

import matplotlib.pyplot as plt
import seaborn as sns

sns.set_palette("crest", 21)
sns.set_style("white")
sns.set_context("paper", font_scale=1.4)

here = Path(__file__).parent

df = pd.read_csv(here / "type_k_its90_trunc.csv", header=0, index_col=0)

# Symmetrize everything except hot junc temp == 0C
hjtemps_C = df.index.values
cjtemps_C = df.columns.values

# Plot initial error values
ref_tiled = np.repeat(np.atleast_2d(df.values[:, 10]).T, len(cjtemps_C), axis=1)  # [mV]
err = df.values - ref_tiled

plt.figure(figsize=(8,4))
for i, x in enumerate(err.T):
    plt.plot(hjtemps_C, x, label=f"{cjtemps_C[i]} (C)")
plt.title("Cold Junction Error - Original Tables")
plt.xlabel("Hot Junction Temperature (C)")
plt.ylabel("Error [mV]")
plt.legend(bbox_to_anchor=[1.0, 1.0], ncol=2, title="Cold Junc. Temp")
plt.tight_layout()
plt.savefig("./type_k_orig.svg")

# Symmetrize
err = np.nan_to_num(err, nan=0.0)
err_sym = err - np.fliplr(err)
#   Preserve hot junction temperature == 0.0, the only temperature
#   that is already fully populated in the tables
good_index = np.argwhere(hjtemps_C == 0.0)[0]
err_sym[good_index, :] = err[good_index, :]
symmetrized = ref_tiled + err_sym  # [mV]

# Plot symmetrized error values
plt.figure(figsize=(8,4))
for i, x in enumerate(err_sym.T):
    plt.plot(hjtemps_C, x, label=f"{cjtemps_C[i]} (C)")
plt.title("Cold Junction Error - Anti-Symmetrized Tables")
plt.xlabel("Hot Junction Temperature (C)")
plt.ylabel("Error [mV]")
plt.legend(bbox_to_anchor=[1.0, 1.0], ncol=2, title="Cold Junc. Temp")
plt.tight_layout()
plt.savefig("./type_k_antisymmetrized.svg")

# Update dataframe with symmetrized values
df = pd.DataFrame(symmetrized, columns=cjtemps_C, index=hjtemps_C)

# Save symmetrized-ish version for reference
df.to_csv(here / "type_k_its90_mod.csv")

# Generate rust table inputs
# ITS-90 cold junction temps are inverted,
# so we have to negate the celsius delta
# and then reverse the list so that it's sorted ascending
c_offs = 273.15  # conversion from degC to K
#   (K) from degC, cold junc. temp
cjtemps = np.array([(-float(x)) + c_offs for x in df.columns.values])[::-1]
hjtemps = np.array(
    [float(x) + c_offs for x in df.index.values]
)  # (K) from degC, hot junc. temp
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
vgrid = np.linspace(vmin, vmax, ngrid)
assert np.allclose(hjtemps, sorted(hjtemps))
hjtemps_interpolated = np.zeros((ngrid, len(cjtemps)))
for i, cjtemp in enumerate(cjtemps):
    v = voltages[:, i]
    assert np.allclose(v, sorted(v))
    assert len(v) == 163  # Original tables have 163 hot junction temp points
    hjtemps_interpolated[:, i] = interpn([vgrid], [v], hjtemps, method="cubic")

# Save data to disk in little-endian binary format
with open(here / "its90_cold_junc_tables_rust.txt", "w") as f:
    step = cjtemps[1] - cjtemps[0]
    start = cjtemps[0]
    n = len(cjtemps)
    f.write('// (K) temperature at the connector "cold junction", regular grid\n')
    f.write(f"const COLD_JUNCTION_TEMP_START_K: f64 = {start};  // (K)\n")
    f.write(f"const COLD_JUNCTION_TEMP_STEP_K: f64 = {step};  // (K)\n")
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
        '// (K) Temperature at the probe ("hot junction"), 2d table mapping (sensed_voltage, cold_junction_temp) => hot_junction_temp.\n'
    )
    f.write("#[rustfmt::skip]\n")
    f.write(
        f"const HOT_JUNCTION_TEMP_K: [f64; {hjtemps_interpolated.size}] = "
        + str([float(x) for x in hjtemps_interpolated.flatten()])
        + ";  // (K)\n"
    )

plt.figure(figsize=(8,4))
print(cjtemps[10])
ref = hjtemps_interpolated[:, 10]
for i in range(len(cjtemps)):
    vals = hjtemps_interpolated[:, i]
    err = vals - ref
    plt.plot(vals, err, label=f"{cjtemps[i] - 273.15} (C)")
plt.title("Final Inverted Table")
plt.xlabel("Hot Junction Temp (K)")
plt.ylabel("Error (K)")
plt.legend(bbox_to_anchor=[1.0, 1.0], ncol=2, title="Cold Junc. Temp")
plt.tight_layout()
plt.savefig("./type_k_inverted.svg")
plt.show()
