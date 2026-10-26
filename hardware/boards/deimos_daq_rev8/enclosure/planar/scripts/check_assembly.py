"""Check sampled assembly motion with the removable terminal plugs detached.

Run in FreeCAD after building the enclosure. This is a nominal geometry check.
It does not simulate contact, fasteners, cable routing, or manufacturing variation.
"""
from pathlib import Path
import json
import FreeCAD as App
import Part

ROOT=Path(__file__).resolve().parents[1]


def check(doc):
    plug_source=ROOT.parent/'terminal_block/15EDGKM-3.81-12P-1Y-00A&Z(H).stp'
    plug=Part.read(str(plug_source))
    solids=doc.Rev8ComponentReference.Shape.Solids
    # The assembly stores one solid per mating plug. Match the source STEP volume.
    plugs=[s for s in solids if abs(s.Volume-plug.Volume)<.1]
    assert len(plugs)==6,'Expected six removable 15EDGKM terminal plugs.'
    fixed_components=Part.makeCompound([s for s in solids if abs(s.Volume-plug.Volume)>=.1])
    base=doc.BaseChassis.Shape;lid=doc.Lid.Shape
    fixed=Part.makeCompound([base,doc.Rev8PCB.Shape,fixed_components])
    paths=[(y,0) for y in (0,.5,1,2,4,8,12,20)]+[(20,z) for z in (1,5,15,30,60)]
    lid_checks=[]
    for y,z in paths:
        s=lid.copy();s.translate(App.Vector(0,y,z))
        lid_checks.append({'rearward_mm':y,'up_mm':z,'intersection_mm3':s.common(fixed).Volume})
    board=Part.makeCompound([doc.Rev8PCB.Shape,fixed_components])
    fixed=Part.makeCompound([base])
    board_checks=[]
    for y in (0,.5,1,2,4,8,12,20,40,80,110):
        s=board.copy();s.translate(App.Vector(0,y,0))
        board_checks.append({'rearward_mm':y,'intersection_mm3':s.common(fixed).Volume})
    report=json.loads((ROOT/'validation.json').read_text())
    report.update(lid_removal_samples=lid_checks,board_insertion_samples=board_checks,
                  assembly_condition='All six 15EDGKM terminal plugs removed. PCB headers remain.',
                  removable_plug_source=str(plug_source.relative_to(ROOT.parent)),
                  removable_plug_count=len(plugs),intersection_tolerance_mm3=.001)
    report['assembly_motion_passed']=all(v['intersection_mm3']<.001 for v in lid_checks+board_checks)
    (ROOT/'validation.json').write_text(json.dumps(report,indent=2))
    assert report['assembly_motion_passed'],'Inspect assembly interference results in validation.json.'
    return fixed_components,plugs


if __name__=='__main__':
    check(App.ActiveDocument)
