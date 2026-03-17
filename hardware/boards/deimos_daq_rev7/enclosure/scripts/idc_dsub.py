from pathlib import Path
import ezdxf
from pint import UnitRegistry

ureg = UnitRegistry()

# conversion factor: mm → inches
scale = (1 * ureg.mm).to(ureg.inch).magnitude

there = Path("__file__").absolute().parent.parent / "dsub_idc"

# load DXF
doc = ezdxf.readfile(there / "connector_cutout_0p5mm_clearance_inner_screw.dxf")
msp = doc.modelspace()

# scale everything about origin
for e in msp:
    try:
        e.scale_uniform(scale)
    except AttributeError:
        pass  # some entities don't support scaling directly

# save new file
doc.saveas(there / "connector_cutout_inches.dxf")

print("Scaled by:", scale)