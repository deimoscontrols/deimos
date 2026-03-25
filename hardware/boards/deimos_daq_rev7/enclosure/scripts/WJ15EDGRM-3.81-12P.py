import math
import cadquery as cq
from cadquery import exporters

# ------------------------------------------------------------
# Parameters
# ------------------------------------------------------------

pins = 12
pitch = 3.81

# From drawing
body_w = pins * pitch + 0.88
ear_each = 4.60

# Measured from physical part
overall_h = 6.00
lowered_outer_w = 5.00   # lowered top section width from each outer edge
notch_d = 1.25           # amount those outer top sections are lowered
corner_r = 1.00          # outer corner radius

# Simplified depth for 3D model
body_depth = 7.0         # behind panel
flange_thick = 3.0       # in front of panel

# Clearance for enclosure cutout
cutout_padding = 0.25

# Output filenames
STEP_FILE = "terminal_block_12pos.step"
DXF_FILE = "terminal_block_12pos_cutout_clearance.dxf"


# ------------------------------------------------------------
# Derived dimensions
# ------------------------------------------------------------

overall_w = body_w + 2 * ear_each
total_depth = body_depth + flange_thick
SQRT2 = math.sqrt(2.0)


# ------------------------------------------------------------
# Arc helper
# Returns the midpoint on a quarter-circle arc.
#
# center: (cx, cy)
# quadrant midpoint angles used:
#   br = -45 deg
#   tr = +45 deg
#   tl = 135 deg
#   bl = 225 deg
# ------------------------------------------------------------

def quarter_arc_midpoint(cx, cy, r, which):
    if which == "br":
        a = -math.pi / 4.0
    elif which == "tr":
        a = math.pi / 4.0
    elif which == "tl":
        a = 3.0 * math.pi / 4.0
    elif which == "bl":
        a = 5.0 * math.pi / 4.0
    else:
        raise ValueError(f"Unknown arc type: {which}")

    return (cx + r * math.cos(a), cy + r * math.sin(a))


# ------------------------------------------------------------
# 2D nominal front profile
#
# Correct interpretation:
# - full outer silhouette including ears
# - outer top sections are lowered by notch_d
# - each lowered outer section extends lowered_outer_w inward
# - all four OUTER corners have radius corner_r
# - notch shoulder corners remain sharp
# ------------------------------------------------------------

def nominal_profile():
    xL = -overall_w / 2.0
    xR =  overall_w / 2.0
    yB = -overall_h / 2.0
    yT =  overall_h / 2.0
    yN = yT - notch_d
    r = corner_r
    w = lowered_outer_w

    # Arc centers
    c_br = (xR - r, yB + r)
    c_tr = (xR - r, yN - r)
    c_tl = (xL + r, yN - r)
    c_bl = (xL + r, yB + r)

    # Arc midpoints
    m_br = quarter_arc_midpoint(*c_br, r, "br")
    m_tr = quarter_arc_midpoint(*c_tr, r, "tr")
    m_tl = quarter_arc_midpoint(*c_tl, r, "tl")
    m_bl = quarter_arc_midpoint(*c_bl, r, "bl")

    return (
        cq.Workplane("XY")
        # start at bottom edge, just right of BL fillet
        .moveTo(xL + r, yB)
        .lineTo(xR - r, yB)

        # bottom-right outer fillet
        .threePointArc(m_br, (xR, yB + r))

        # right side up to top-right fillet start
        .lineTo(xR, yN - r)

        # top-right outer fillet
        .threePointArc(m_tr, (xR - r, yN))

        # lowered right outer section
        .lineTo(xR - w, yN)

        # step up to center top
        .lineTo(xR - w, yT)

        # center top
        .lineTo(xL + w, yT)

        # step down to lowered left outer section
        .lineTo(xL + w, yN)

        # lowered left outer section
        .lineTo(xL + r, yN)

        # top-left outer fillet
        .threePointArc(m_tl, (xL, yN - r))

        # left side down to bottom-left fillet start
        .lineTo(xL, yB + r)

        # bottom-left outer fillet
        .threePointArc(m_bl, (xL + r, yB))

        .close()
    )


# ------------------------------------------------------------
# Build nominal 3D model
# ------------------------------------------------------------

nominal_2d = nominal_profile()

model_3d = (
    nominal_2d
    .extrude(total_depth)
    .translate((0, 0, -body_depth))
)


# ------------------------------------------------------------
# Build padded clearance cutout
# ------------------------------------------------------------

cutout_2d = nominal_profile().offset2D(cutout_padding, kind="arc")


# ------------------------------------------------------------
# Export
# ------------------------------------------------------------

exporters.export(model_3d, STEP_FILE)
exporters.export(cutout_2d, DXF_FILE)

print(f"Exported STEP: {STEP_FILE}")
print(f"Exported DXF : {DXF_FILE}")