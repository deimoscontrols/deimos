//
// Final enclosure interface model
// 12-position, 3.81 mm pitch pluggable terminal block
//
// Correct shape:
//   • full outer silhouette used for enclosure cutout
//   • lowered top sections on the OUTER 5 mm of each side
//   • raised full-height center section
//   • R1 fillets on all four OUTER corners
//   • optional 3D reference model
//

// ---------------- Parameters ----------------

pins = 12;
pitch = 3.81;

// From drawing
body_w = pins * pitch + 0.88;
ear_each = 4.60;

// Measured
overall_h = 6.00;      // full center height
outer_notch_w = 5.00;  // lowered region width from each outer edge
notch_d = 1.25;        // amount lowered from the top
corner_r = 1.00;

// Simplified 3D reference depth
body_depth   = 7.0;
flange_thick = 3.0;

// User-adjustable
clearance = 0.20;


// ---------------- Derived ----------------

overall_w = body_w + 2 * ear_each;


// ---------------- Helpers ----------------

function arc_pts(cx, cy, r, a1, a2, n=12) =
    [ for (i = [0:n])
        let(a = a1 + (a2 - a1) * i / n)
        [cx + r * cos(a), cy + r * sin(a)]
    ];


// ---------------- 2D Profile ----------------

module panel_cutout_profile(extra = 0) {
    w  = overall_w + 2*extra;
    h  = overall_h + 2*extra;
    nw = outer_notch_w + extra;
    nd = notch_d + extra;
    r  = corner_r + extra;

    xL = -w/2;
    xR =  w/2;
    yB = -h/2;
    yT =  h/2;        // full-height center top
    yN = yT - nd;     // lowered outer-top level

    pts = concat(
        // bottom edge
        [[xL + r, yB]],
        [[xR - r, yB]],

        // bottom-right radius
        arc_pts(xR - r, yB + r, r, -90, 0),

        // right side up to lowered-top radius start
        [[xR, yN - r]],

        // top-right outer radius (to lowered top)
        arc_pts(xR - r, yN - r, r, 0, 90),

        // lowered right outer section
        [[xR - nw, yN]],

        // step up to full-height center
        [[xR - nw, yT]],

        // full-height center top
        [[xL + nw, yT]],

        // step down to lowered left outer section
        [[xL + nw, yN]],

        // lowered left outer section
        [[xL + r, yN]],

        // top-left outer radius (from lowered top to left side)
        arc_pts(xL + r, yN - r, r, 90, 180),

        // left side down to bottom-left radius start
        [[xL, yB + r]],

        // bottom-left radius
        arc_pts(xL + r, yB + r, r, 180, 270)
    );

    polygon(points = pts);
}


// ---------------- 3D Reference ----------------

module connector_reference_3d() {
    union() {
        linear_extrude(height = flange_thick)
            panel_cutout_profile();

        translate([0, 0, -body_depth])
            linear_extrude(height = body_depth)
                panel_cutout_profile();
    }
}


// ---------------- Output ----------------

// 3D reference model
connector_reference_3d();

// 2D enclosure cutout instead:
// panel_cutout_profile(clearance);