# Rev8 sheet-metal enclosure

Open `DeimosDaqRev8_SheetMetal.FCStd` in FreeCAD with the SheetMetal workbench installed.
The model uses the board center as its XY origin. Z = 0 is the PCB lower surface.

## Construction

- Two parts in 0.063 in (1.6002 mm) 5052-H32 aluminum, with black anodizing.
- The base includes the floor, front panel, side walls, and front corner wings.
- The lid includes the roof, rear panel, rear corner wings, and two short side flanges.
- Each panel has 12 mm inward wings. They overlap the side walls with 0.8 mm nominal clearance.
- The lid side flanges stop short of the wings. The base walls back these seams.
- Four 6-32 thread-forming screws join the lid side flanges to the base walls.
- Base pilot holes are 3.05 mm diameter, approximately a No. 31 drill (0.120 in).
- Lid clearance holes are 4.5 mm diameter. They allow about 0.5 mm radial adjustment around the nominal screw diameter.
- Use a head with a flat bearing face, such as a pan head, to preserve that adjustment. No countersinks or nuts are required.
- Fasteners and formed threads are not modeled.

Each sheet has five 90-degree bends. Relief cuts separate adjacent bend ends.
Exposed outline corners have a 2.4892 mm radius, equal to the outer bend radius.
The connector contours and small bend-relief notches retain their original geometry.
The wings cover the vertical corner seams. This is not a sealed enclosure.
The closed envelope is approximately 234.54 × 99.26 × 44.40 mm, excluding fasteners.

The floor outer face is at Z = -23.1 mm. The front top edge is at Z = 19.5 mm.
The lowest front cutout is 5.3622 mm above the bend tangent.
In the flat pattern, this is 6.5883 mm from the bend centerline.
[SendCutSend specifies a 0.472 in die width for this material](https://sendcutsend.com/materials/5052-aluminum/).
Its [feature-placement rule](https://sendcutsend.com/blog/guide-to-planning-features-around-bend-lines/) requires at least half that width: 5.9944 mm.
The design adds 0.5939 mm to that minimum.
The highest front text has 2.0412 mm clearance to the top edge. This margin is a layout choice.
Rear text stays 1.2894 mm below the roof bend tangent.
The lid screw centers are at Z = 11.5 mm to clear the lowered roof bends.
The enlarged lid holes remain 6.2871 mm from the bend centerline in the flat pattern.

The 6-32 size was confirmed from the original enclosure by the user.
The pilot follows the [TAPTITE II guide, page 9](https://duo-taptite.com/assets/files/taptite-ii-compressed.pdf):
0.1197 in nominal, with a 0.120 in drill, for 0.028–0.066 in material.
This is a starting size for the selected thread-forming screw family, not a conventional tap-drill diameter.
Check forming and stripping torque with the actual screw in a finished 1.6 mm 5052 sample before production.
Laser-cut diameter tolerance can affect the fit. Specify the screw supplier's finished-hole tolerance or finish-drill the pilots as needed.


The board reference is 223 × 95.025 × 1.6 mm.
The front and rear connector seats locate and support the board at the panels.
The enclosure has no card guides or guide mounting holes.

### Assembly

1. Remove all six terminal plugs from the board.
2. Support the board by hand. Slide it forward from the open rear until its front connectors seat in the front cutouts.
3. Lower the lid behind its final position. Slide it forward so the rear cutouts pass over the rear connectors.
4. Install the four side screws. Reconnect the terminal plugs and cables.

To remove the lid, detach the terminal plugs and cables. Remove the four side screws.
Slide the lid rearward by 20 mm to clear the rear connectors. Then lift it off.
Do not lift it straight up while the rear connectors remain in the cutouts.
The model checks sampled assembly positions with the PCB and larger component references. It excludes the six removable terminal plugs and retains the PCB headers. Cables and fasteners are not checked.
Run `scripts/CheckAssembly.FCMacro` in FreeCAD to repeat the motion checks.

## Graphics

`artwork/front_engrave.svg`, `artwork/rear_engrave.svg`, and `artwork/lid_engrave.svg` contain only the marking geometry.
The matching PDFs contain the same artwork at full scale. Black areas indicate the areas to mark.
The artwork is viewed from outside each panel. Do not mirror it.
The lid artwork reads from the front of the enclosure. The front edge is at the bottom of the lid page.

The front artwork page is 234.5408 × 42.6 mm. The rear page is 234.5408 × 42.5 mm.
The lid page is 234.5408 × 99.2592 mm and covers the projected roof outline.
Align each upper-left page corner to the projected upper-left corner of its panel.
The page width includes the corner bends, so use the outer projected edges as the datum.
Use millimeters and 100% scale. Disable page fitting and automatic cropping in the engraving software.
The front page includes the floor bend at the bottom. The rear page includes the roof bend at the top.
The pages include the side bends for the wings. These regions contain no engraving.
Fixture each formed half with its marked face horizontal. Keep the other flanges clear of the laser head and bed.
Check fixture height and head travel before marking.

The `*_proof` files add blue connector outlines and orange bend-tangent lines.
Use these to check alignment. Do not send proof files to the engraver.

The PDA supplies the 111 original labels, logo, QR code, and connector contours.
The logo, company text, website, and QR code are on the lid near its front-left corner.
The company text aligns with the top of the logo. The splash omits "Deimos DAQ".
Three 36 × 6 mm blank fields sit below the branding. Their labels are "Model", "Serial", and "Mac".
The right edges of all three fields align with the right edge of the QR code.
The marking layout starts 10 mm from the left and front edges of the lid.
Text uses Liberation Sans as an Arial substitute. Its size was adjusted to fit the original label groups.
PWR, CONN, and OPER read horizontally below their indicator openings. Their nominal capital height is 1.4 mm.
Text remains editable as Draft ShapeString objects. The exported files contain outlined glyphs.
Curves in SVG paths use a maximum tessellation deflection of 0.002 mm.

### Edit and export

1. Select a label in `FrontArtwork`, `RearArtwork`, or `LidArtwork`.
2. Edit `String`, `Size`, or its registration offsets.
3. Move its `ConnectorAnchor` object when the opening and its labels must move together.
4. Run `scripts/ExportArtwork.FCMacro` to export the active model as SVG and PDF.

Move `LidMarkingAnchor` in X or Y to reposition the lid branding and all three fields together.
Edit a lid object's registration offsets to move it within that layout.
The blank fields are Draft rectangles. Edit `Length` and `Height` to resize them.

The nearest opening supplies each front or rear label's initial anchor. Check that assignment before moving a label group.
Artwork and cutouts share the same panel coordinates. Large changes to panel size or placement require updating the build parameters and rebuilding.

## Fabrication files

`parts/` contains `BaseChassis.step`, `Lid.step`, and their flat-pattern SVG previews.
The former separate front and rear sheets are obsolete and are not exported.
Use the STEP files for quoting. The SVG previews show black cuts and red bend lines.
Graphics are not included in the metal STEP files.
After manual geometry edits, export each object in `MetalParts` to STEP again. Existing STEP files do not update automatically.

The model uses SendCutSend's published values for 0.063 in 5052 aluminum:
inside bend radius 0.035 in (0.889 mm), ANSI K-factor 0.42.
The published minimum formed flange is 0.303 in (7.6962 mm). The wings are 12 mm deep.
These values were checked on 2026-10-25.
Confirm the bend sequence and tooling access with SendCutSend. A successful CAD unfold does not establish press-brake access.
Check the quote preview before ordering. Confirm the connector seating with actual parts before ordering a batch.

## Regenerate

Run `python scripts/prepare_inputs.py` to extract the PDA data. This command requires PyYAML.
Then run `scripts/BuildEnclosure.FCMacro` in FreeCAD.
The macro creates a new document and writes the model, STEP files, SVG files, and preview images beside this README.
Run the artwork exporter afterward to update the PDFs.
The exploded preview uses temporary display copies. These copies are removed before the model is saved.
It reads the existing assembly for the PCB and component references. It does not edit that assembly.
The reference cache is local and excluded from Git.
The Dimensions sheet records design values. Change the script parameters and rebuild to change thickness, radii, or overall size.
The model uses native SheetMetal U shapes, extruded bend sections, unions, and connector cuts.

For command-line PDF export, run `python scripts/export_pdfs.py`. This command uses only the Python standard library.
For SVG files edited in Inkscape, export the PDF from Inkscape at page size instead.
Save manual model edits before rebuilding. Rebuilding replaces the generated file with the scripted design.

## Checks and sources

`validation.json` records solid validity, unfolding, cutout registration, and interference checks.
The component reference includes larger components. Small components and cable routing are not fully checked.
The supplied board assembly is a reference, not a physical fit test.

- [SendCutSend bend values](https://sendcutsend.com/bending-calculator/)
- [SendCutSend bend relief guidance](https://sendcutsend.com/blog/guide-to-designing-bend-reliefs/)
- [SendCutSend bend deformation guidance](https://sendcutsend.com/guidelines/bend-deformation/)
