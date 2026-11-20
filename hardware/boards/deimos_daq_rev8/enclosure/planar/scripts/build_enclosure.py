"""Build the Rev8 sheet-metal alternative in FreeCAD 1.1 with SheetMetal installed.

Run BuildEnclosure.FCMacro in FreeCAD. All source dimensions are millimeters.
The PCB datum is its center in XY and its lower surface in Z.
"""
from pathlib import Path
import json
import math
import sys
import traceback
import FreeCAD as App
import Part
import Draft
import Import
import importSVG

ROOT = Path(__file__).resolve().parents[1]
WORK = ROOT / 'reference/cache'
for base in [Path(App.getUserAppDataDir()) / 'Mod/sheetmetal', Path('/home/jlogan/snap/freecad/common/Mod/sheetmetal')]:
    if base.exists():
        sys.path.insert(0, str(base))
from SheetMetalBaseShapeCmd import SMBaseShape
from SheetMetalNewUnfolder import BendAllowanceCalculator, getUnfold

V = App.Vector
T = 0.063 * 25.4
R = 0.035 * 25.4
K = 0.42
INNER_W = 226.54
OUTER_W = INNER_W + 2*T
SIDE_GAP = .8
PANEL_W = OUTER_W + 2*SIDE_GAP + 2*T
FRONT_Y = -49.685060818343
REAR_Y = 49.574139181657
# Half the 0.472 in die width, plus 0.5 mm, sets the flat cutout clearance.
# The formed front tangent is BOTTOM + R + T. Half the bend allowance
# maps the tangent-to-cutout gap to the flat bend-centerline distance.
BOTTOM = -23.1
TOP = 19.5
DIE_WIDTH = .472 * 25.4
TEXT_TOP_MARGIN = 2.0
LID_SCREW_Z = 11.5
LID_CLEARANCE_DIAMETER = 4.5
# PEM F-632-1 installation hole.
ASSEMBLY_NUT_HOLE_DIAMETER = .213 * 25.4
MOUNT_SCREW_Z = BOTTOM + 10.5
MOUNT_SCREW_Y = (FRONT_Y + REAR_Y) / 2
MOUNT_FLANGE_DIAMETER = .328 * 25.4
PANEL_BOTTOM = BOTTOM + T
PANEL_H = TOP - PANEL_BOTTOM
WING = 20.0
WING_SCREW_OFFSET = 14.5
RELIEF = 1.0
RELIEF_DEPTH = 3.0
REAR_BOTTOM = BOTTOM + T + .3
# Coordinate registration from the PDA, enclosure STEP, and assembly PCB datum.
PDA_Z_OFFSET = -20.4070093366
PDA_FRONT_X = 104.234426721
PDA_REAR_X = -105.911173279
FONT = '/usr/share/fonts/truetype/liberation/LiberationSans-Regular.ttf'


def feature(name, shape, parent=None):
    obj = doc.addObject('Part::Feature', name)
    obj.Shape = shape
    if parent: parent.addObject(obj)
    return obj


def style(obj, color=(0.16, 0.17, 0.19), visible=True):
    if App.GuiUp:
        obj.ViewObject.ShapeColor = color
        obj.ViewObject.LineColor = (0.08, 0.08, 0.08)
        
        if 'Flat Lines' in obj.ViewObject.listDisplayModes():
            obj.ViewObject.DisplayMode = 'Flat Lines'
        obj.ViewObject.Visibility = visible


def group(name):
    return doc.addObject('App::DocumentObjectGroup', name)


def native_base(name, shape_type, length, width, height, placement):
    o = doc.addObject('PartDesign::FeaturePython', name)
    SMBaseShape(o)
    if App.GuiUp:
        from SheetMetalBaseShapeCmd import SMBaseShapeViewProviderFlat
        SMBaseShapeViewProviderFlat(o.ViewObject)
    o.shapeType = shape_type
    o.length = length
    o.width = width
    o.height = height
    o.thickness = T
    o.radius = R
    o.originLoc = '-X,-Y'
    o.Placement = placement
    o.setExpression('thickness', 'Dimensions.Thickness')
    o.setExpression('radius', 'Dimensions.BendRadius')
    doc.recompute()
    style(o)
    history.addObject(o)
    return o


def cylinder(center, radius, direction, length=16):
    return Part.makeCylinder(radius, length, center, direction)


def cut_feature(name, base, tools):
    tool = feature(name+'Tool', Part.makeCompound(tools), history)
    o = doc.addObject('Part::Cut', name)
    o.Base = base
    o.Tool = tool
    o.Refine = True
    metal.addObject(o)
    doc.recompute()
    style(base, visible=False)
    style(tool, visible=False)
    style(o)
    return o


def pda_wire(kind, d):
    x, y = float(d.get('OriginX',0))*25.4, float(d.get('OriginY',0))*25.4
    if kind == 'Circle':
        return Part.Wire([Part.makeCircle(float(d['radius'])*25.4, V(x,y,0))])
    point = V(x,y,0)
    edges = []
    for item in d['pathSegments']:
        if 'Line' in item:
            delta = V(float(item['Line'][0])*25.4,float(item['Line'][1])*25.4,0)
            end = point+delta
            if delta.Length > 1e-8: edges.append(Part.makeLine(point,end))
        else:
            arc = item['CircleArc3']
            mid = point+V(float(arc['mid'][0])*25.4,float(arc['mid'][1])*25.4,0)
            end = point+V(float(arc['end'][0])*25.4,float(arc['end'][1])*25.4,0)
            edges.append(Part.Arc(point,mid,end).toShape())
        point = end
    return Part.Wire(edges)


def outside_plane(side, offset=0):
    # Local X is right and local Y is up when viewed from outside.
    if side == 'front':
        return App.Placement(V(0,FRONT_Y-offset,0), App.Rotation(V(1,0,0),90))
    return App.Placement(V(0,REAR_Y+offset,0), App.Rotation(V(0,0,1),180).multiply(App.Rotation(V(1,0,0),90)))


def source_to_outside(side, shape):
    result=shape.copy()
    result.rotate(V(),V(0,1,0),180)
    result.translate(V(PDA_FRONT_X if side=='front' else -PDA_REAR_X,PDA_Z_OFFSET,0))
    return result



def make_panel(side, base, final_name, extra_tools):
    isfront = side=='front'
    base.addProperty('App::PropertyString','Material','Manufacturing').Material = '5052-H32 aluminum, black anodize'
    plane = outside_plane(side)
    anchors=[]; cutters=[]; refs=[]
    for entry in panel_data[side]:
        kind,d = next(iter(entry.items()))
        if d.get('type')!='CUTOUT': continue
        # Omit the original extrusion fastener holes.
        if kind=='Circle' and abs(float(d['radius'])-.08250004455)<1e-6: continue
        w=source_to_outside(side,pda_wire(kind,d))
        face=Part.Face(w)
        center=face.BoundBox.Center
        face.translate(-center)
        face=Part.makeCompound([face])
        anchor=feature(side.title()+'Cutout'+d['name'], face, history)
        anchor.Placement=plane.multiply(App.Placement(center,App.Rotation()))
        anchor.addProperty('App::PropertyString','PdaId','Source').PdaId=d['name']
        anchor.addProperty('App::PropertyString','Description','Source').Description='Connector opening from Rev8 PDA. Move this object to move its labels.'
        tool=doc.addObject('Part::Extrusion',side.title()+'Punch'+d['name'])
        tool.Base=anchor
        tool.DirMode='Custom'
        tool.Dir=V(0,1 if isfront else -1,0)
        tool.LengthFwd=T+0.2
        tool.LengthRev=0.2
        tool.Solid=True
        history.addObject(tool)
        anchors.append((anchor,center))
        cutters.append(tool)
        refs.append(w)
        style(anchor,visible=False)
    holetool=feature(side.title()+'MountingHoleTool',Part.makeCompound(extra_tools),history)
    cutters.append(holetool)
    fused=doc.addObject('Part::MultiFuse',side.title()+'AllPunches')
    fused.Shapes=cutters
    history.addObject(fused)
    final=doc.addObject('Part::Cut',final_name)
    final.Base=base;final.Tool=fused;final.Refine=True
    metal.addObject(final)
    doc.recompute()
    for o in [base,holetool,fused]+cutters:style(o,visible=False)
    style(final)
    return final,anchors,refs


def add_art_properties(o, anchor, side):
    o.addProperty('App::PropertyLink','ConnectorAnchor','Registration').ConnectorAnchor=anchor
    o.addProperty('App::PropertyDistance','OffsetX','Registration').OffsetX=o.Placement.Base.x-anchor.Placement.Base.x
    o.addProperty('App::PropertyDistance','OffsetZ','Registration').OffsetZ=o.Placement.Base.z-anchor.Placement.Base.z
    o.addProperty('App::PropertyString','Panel','Registration').Panel=side
    o.setExpression('Placement.Base.x',f'{anchor.Name}.Placement.Base.x + OffsetX')
    o.setExpression('Placement.Base.z',f'{anchor.Name}.Placement.Base.z + OffsetZ')


def art_transform(side, d):
    # PDA graphics carry reflection flags. Their combined exterior transform is a rotation.
    r=math.radians(float(d.get('rotation',0)))
    sx=float(d.get('scaleFactorX',1));sy=float(d.get('scaleFactorY',1))
    a,b,c,e=-math.cos(r)*sx, math.sin(r)*sy, math.sin(r)*sx, math.cos(r)*sy
    angle=math.degrees(math.atan2(c,a))
    d['_reflect_y'] = a*e-b*c < 0
    px=(PDA_FRONT_X if side=='front' else -PDA_REAR_X)-float(d['OriginX'])*25.4
    py=PDA_Z_OFFSET+float(d['OriginY'])*25.4
    return App.Placement(V(px,py,0),App.Rotation(V(0,0,1),angle))


def artwork(side, anchors):
    grp=group(side.title()+'Artwork')
    plane=outside_plane(side,.035)
    objects=[]
    for entry in panel_data[side]:
        kind,d=next(iter(entry.items()))
        if d.get('type')!='GRAPHIC':continue
        pl=art_transform(side,d)
        if kind=='Text2D':
            o=Draft.make_shapestring(d['content'],FONT,float(d['fontSize'])*25.4/72*.72,0)
            o.Label=side.title()+': '+d['content']
            o.ScaleToSize=False
            o.KeepLeftMargin=True
            o.Justification='Bottom-Left'
            o.MakeFace=True
            o.recompute()
            o.Placement=plane.multiply(pl)
            o.addProperty('App::PropertyFloat','SourcePointSize','Source').SourcePointSize=float(d['fontSize'])
            o.addProperty('App::PropertyString','SourceFont','Source').SourceFont=d['fontFamily']
        elif kind=='SVG':
            before=set(doc.Objects)
            import contextlib, io
            with contextlib.redirect_stdout(io.StringIO()):
                importSVG.insert(str(ROOT/d['svg_file']),doc.Name)
            imported=[o for o in doc.Objects if o not in before and hasattr(o,'Shape') and not o.Shape.isNull()]
            s=Part.makeCompound([o.Shape for o in imported])
            # Fit the graphic within its PDA bounds. ImportSVG places SVG Y downward.
            bb=s.BoundBox
            w=float(d['Bounds']['width'])*25.4;h=float(d['Bounds']['height'])*25.4
            # The QR source contains a four-module quiet zone on a 37 mm page.
            s.translate(V(-bb.XMin,-bb.YMin,0))
            s=Part.makeCompound([s])
            if 'symbol_1' in d['svg_file']:
                scale=w/37
                s.scale(scale)
                s.translate(V(4*scale,4*scale,0))
            else:
                m=App.Matrix();m.A11=w/bb.XLength;m.A22=h/bb.YLength
                s=s.transformGeometry(m)
            for o1 in reversed(imported):doc.removeObject(o1.Name)
            s=Part.makeCompound([s])
            o=feature(side.title()+'Symbol',s)
            o.Placement=plane.multiply(pl)
        else:
            if kind=='Rectangle':
                w=float(d['width'])*25.4;h=float(d['height'])*25.4
                s=Part.makePolygon([V(0,0,0),V(w,0,0),V(w,h,0),V(0,h,0),V(0,0,0)])
            elif kind=='Line':s=Part.makeLine(V(),V(*[float(x)*25.4 for x in d['end']],0))
            elif kind=='Arc':s=Part.Arc(V(),V(*[float(x)*25.4 for x in d['mid']],0),V(*[float(x)*25.4 for x in d['end']],0)).toShape()
            elif kind=='Circle':s=Part.makeCircle(.025*25.4)
            else:raise ValueError(kind)
            if d['_reflect_y']:
                reflection=App.Matrix();reflection.A22=-1
                s=s.transformGeometry(reflection)
            o=feature(side.title()+kind,s)
            o.Placement=plane.multiply(pl)
            o.addProperty('App::PropertyLength','MarkWidth','Engraving').MarkWidth=.18
        grp.addObject(o)
        # Match each graphic to the nearest opening in panel coordinates.
        anchor,_=min(anchors,key=lambda pair:(pair[1]-pl.Base).Length)
        add_art_properties(o,anchor,side)
        style(o,(0.92,0.92,0.92))
        if App.GuiUp:
            o.ViewObject.LineColor=(.92,.92,.92)
            o.ViewObject.LineWidth=1.0
            o.ViewObject.DisplayMode='Flat Lines'
        objects.append(o)
    return objects


def horizontal_indicator_labels():
    """Center horizontal 1.4 mm labels below the three indicator openings."""
    for text,cutout in [('PWR','FrontCutout15'),('CONN','FrontCutout16'),('OPER','FrontCutout17')]:
        obj=next(o for o in doc.FrontArtwork.Group if hasattr(o,'String') and o.String==text)
        anchor=doc.getObject(cutout)
        obj.Size=1.4
        obj.ScaleToSize=True
        obj.Placement.Rotation=outside_plane('front',.035).Rotation
        obj.ConnectorAnchor=anchor
        for axis in ('x','z'):
            obj.setExpression('Placement.Base.'+axis,f'{anchor.Name}.Placement.Base.{axis} + Offset{axis.upper()}')
        doc.recompute()
        bounds=obj.Shape.BoundBox
        obj.OffsetX=obj.OffsetX.Value+anchor.Shape.BoundBox.Center.x-bounds.Center.x
        obj.OffsetZ=obj.OffsetZ.Value+anchor.Shape.BoundBox.ZMin-1.5-bounds.ZMax
    doc.recompute()


def lid_plane():
    return App.Placement(V(0,0,TOP+.2+T+.035),App.Rotation())


def move_branding_to_lid():
    front=doc.getObject('FrontArtwork')
    names={'Deimos DAQ','Deimos Controls LLC','deimoscontrols.com'}
    branding=[o for o in front.Group if o.Name.startswith('FrontSymbol') or (hasattr(o,'String') and o.String in names)]
    assert len(branding)==5,'Expected three branding text objects, logo, and QR code.'
    grp=group('LidArtwork')
    inverse=outside_plane('front',.035).inverse()
    bounds=Part.makeCompound([o.Shape for o in branding]).transformed(inverse.toMatrix()).BoundBox
    anchor=feature('LidMarkingAnchor',Part.Vertex(V()),history)
    anchor.Label='Lid marking origin (front-left)'
    anchor.Placement=App.Placement(V(-PANEL_W/2+10,FRONT_Y+10,lid_plane().Base.z),App.Rotation())
    style(anchor,visible=False)

    def register(obj, placement):
        for axis in ('x','y','z'):obj.setExpression('Placement.Base.'+axis,None)
        if 'ConnectorAnchor' in obj.PropertiesList:obj.removeProperty('ConnectorAnchor')
        obj.Placement=placement
        for prop,typ in [('OffsetX','App::PropertyDistance'),('OffsetY','App::PropertyDistance'),('OffsetZ','App::PropertyDistance'),('Panel','App::PropertyString'),('RegistrationAnchor','App::PropertyLink')]:
            if prop not in obj.PropertiesList:obj.addProperty(typ,prop,'Registration')
        obj.Panel='lid';obj.RegistrationAnchor=anchor
        for axis in ('x','y','z'):
            prop='Offset'+axis.upper()
            setattr(obj,prop,getattr(placement.Base,axis)-getattr(anchor.Placement.Base,axis))
            obj.setExpression('Placement.Base.'+axis,f'{anchor.Name}.Placement.Base.{axis} + {prop}')
        grp.addObject(obj)
        style(obj,(.92,.92,.92))
        if App.GuiUp:
            obj.ViewObject.LineColor=(.92,.92,.92)
            if obj.Shape.Faces:obj.ViewObject.DiffuseColor=[(.92,.92,.92)]*len(obj.Shape.Faces)

    shift=App.Placement(V(-bounds.XMin,29-bounds.YMin,0),App.Rotation())
    for obj in branding:
        if hasattr(obj,'String') and obj.String=='Deimos DAQ':
            doc.removeObject(obj.Name)
            continue
        placement=anchor.Placement.multiply(shift).multiply(inverse).multiply(obj.Placement)
        front.removeObject(obj)
        if hasattr(obj,'String'):obj.Label='Lid: '+obj.String
        else:obj.Label='Lid: QR code' if obj.Name=='FrontSymbol' else 'Lid: Logo'
        register(obj,placement)
    doc.recompute()
    logo=next(o for o in grp.Group if o.Label=='Lid: Logo')
    company=next(o for o in grp.Group if hasattr(o,'String') and o.String=='Deimos Controls LLC')
    delta=logo.Shape.BoundBox.YMax-company.Shape.BoundBox.YMax
    for obj in grp.Group:
        if hasattr(obj,'String'):obj.OffsetY=obj.OffsetY.Value+delta
    qr=next(o for o in grp.Group if o.Label=='Lid: QR code')
    box_x=qr.Shape.BoundBox.XMax-anchor.Placement.Base.x-36
    for label,y in [('Model',18.),('Serial',9.),('Mac',0.)]:
        text=Draft.make_shapestring(label,FONT,2.2,0)
        text.Label='Lid: '+label
        text.ScaleToSize=True;text.MakeFace=True;text.KeepLeftMargin=True
        register(text,anchor.Placement.multiply(App.Placement(V(0,y+1.9,0),App.Rotation())))
        box=Draft.make_rectangle(36,6,face=False)
        box.Label='Lid: '+{'Model':'Model number box','Serial':'Serial number box','Mac':'Mac address box'}[label]
        box.addProperty('App::PropertyLength','MarkWidth','Engraving').MarkWidth=.18
        register(box,anchor.Placement.multiply(App.Placement(V(box_x,y,0),App.Rotation())))
    doc.recompute()
    if App.GuiUp:
        import FreeCADGui as Gui
        Gui.Selection.clearSelection()
    return list(grp.Group)


def export_lid_art(document):
    inverse=lid_plane().inverse().toMatrix()
    body=[svg_shape(o.Shape.copy().transformShape(inverse,True),width=o.MarkWidth.Value if hasattr(o,'MarkWidth') else .18)
          for o in document.LidArtwork.Group]
    width=PANEL_W;height=REAR_Y-FRONT_Y;left=-width/2;top=-REAR_Y
    header=f'<svg xmlns="http://www.w3.org/2000/svg" width="{width:.6f}mm" height="{height:.6f}mm" viewBox="{left:.6f} {top:.6f} {width:.6f} {height:.6f}">\n'
    (ROOT/'artwork/lid_engrave.svg').write_text(header+'\n'.join(body)+'\n</svg>')
    proof=[f'<rect x="{left}" y="{top}" width="{width}" height="{height}" fill="white" stroke="#999" stroke-width=".15"/>']+body
    roof=max((f for f in document.Lid.Shape.Faces if f.BoundBox.ZLength<1e-5 and abs(f.BoundBox.ZMax-(TOP+.2+T))<1e-5),key=lambda f:f.Area)
    proof.append(svg_shape(Part.makeCompound(roof.Wires),stroke='#0071bc',width=.12))
    b=R+T
    proof.append(f'<path d="M {left+b},{-(REAR_Y-b)} H {width/2-b}" stroke="#d66b00" stroke-width=".12" stroke-dasharray="1,1"/>')
    for x in (left+b,width/2-b):
        proof.append(f'<path d="M {x},{-(FRONT_Y+WING+.5)} L {x},{-(REAR_Y-WING-.5)}" stroke="#d66b00" stroke-width=".12" stroke-dasharray="1,1"/>')
    (ROOT/'artwork/lid_proof.svg').write_text(header+'\n'.join(proof)+'\n</svg>')


def wire_path(w):
    pts=w.discretize(Deflection=.002)
    if not pts:return ''
    return 'M '+' L '.join(f'{p.x:.5f},{-p.y:.5f}' for p in pts)+(' Z' if w.isClosed() else '')


def svg_shape(s, fill='black', stroke='none', width=.18):
    paths=[]
    if s.Faces:
        for face in s.Faces:
            paths.append('<path fill="'+fill+'" fill-rule="evenodd" stroke="none" d="'+' '.join(wire_path(w) for w in face.Wires)+'"/>')
    else:
        wires=s.Wires or [Part.Wire([e]) for e in s.Edges]
        for w in wires: paths.append(f'<path fill="none" stroke="{stroke if stroke!="none" else "black"}" stroke-width="{width}" d="{wire_path(w)}"/>')
    return '\n'.join(paths)


def export_art(side,objects,refs):
    plane=outside_plane(side,.035)
    inv=plane.inverse().toMatrix()
    body=[]
    for o in objects:
        s=o.Shape.copy().transformShape(inv,True)
        body.append(svg_shape(s,width=o.MarkWidth.Value if hasattr(o,'MarkWidth') else .18))
    width=PANEL_W
    top=TOP if side=='front' else TOP+.2+T
    bottom=BOTTOM if side=='front' else REAR_BOTTOM
    height=top-bottom;left=-width/2
    header=f'<svg xmlns="http://www.w3.org/2000/svg" width="{width:.6f}mm" height="{height:.6f}mm" viewBox="{left:.6f} {-top:.6f} {width:.6f} {height:.6f}">\n'
    (ROOT/'artwork'/f'{side}_engrave.svg').write_text(header+'\n'.join(body)+'\n</svg>')
    proof=[f'<rect x="{left}" y="{-top}" width="{width}" height="{height}" fill="white" stroke="#999" stroke-width=".15"/>']
    proof.extend(body)
    proof.extend(svg_shape(w,stroke='#0071bc',width=.12) for w in refs)
    tangent=BOTTOM+T+R if side=='front' else TOP+.2-R
    proof.append(f'<path d="M {left},{-tangent} H {width/2}" stroke="#d66b00" stroke-width=".12" stroke-dasharray="1,1"/>')
    for x in (-width/2+R+T,width/2-R-T):
        proof.append(f'<path d="M {x},{-top} L {x},{-bottom}" stroke="#d66b00" stroke-width=".12" stroke-dasharray="1,1"/>')
    (ROOT/'artwork'/f'{side}_proof.svg').write_text(header+'\n'.join(proof)+'\n</svg>')


def unfold_part(obj):
    # Select a broad planar face. The new unfolder returns the flat in that plane.
    faces=[(f.Area,i,f) for i,f in enumerate(obj.Shape.Faces) if isinstance(f.Surface,Part.Plane)]
    _,idx,f=max(faces,key=lambda item:item[0])
    bac=BendAllowanceCalculator.from_single_value(K,'ansi')
    _,flat,bends,normal,infos=getUnfold(bac,obj,'Face'+str(idx+1))
    # Put the unfolded part in XY for interchange and inspection.
    rot=App.Rotation(normal,V(0,0,1))
    flat.rotate(V(),rot.Axis,math.degrees(rot.Angle))
    bends.rotate(V(),rot.Axis,math.degrees(rot.Angle))
    bb=flat.BoundBox;delta=V(-bb.XMin,-bb.YMin,-bb.ZMax)
    flat.translate(delta);bends.translate(delta)
    o=feature(obj.Name+'Flat',flat,flats);style(o,visible=False)
    b=feature(obj.Name+'BendLines',bends,flats);style(b,visible=False)
    o.addProperty('App::PropertyFloat','KFactor','Manufacturing').KFactor=K
    topface=max((f for f in flat.Faces if f.BoundBox.ZLength<.001),key=lambda f:f.Area)
    header=f'<svg xmlns="http://www.w3.org/2000/svg" width="{flat.BoundBox.XLength}mm" height="{flat.BoundBox.YLength}mm" viewBox="0 {-flat.BoundBox.YLength} {flat.BoundBox.XLength} {flat.BoundBox.YLength}">'
    outline=Part.makeCompound(topface.Wires)
    (ROOT/'parts'/f'{obj.Name}_flat.svg').write_text(header+svg_shape(outline,stroke='black',width=.1)+svg_shape(bends,stroke='red',width=.1)+'</svg>')
    assert flat.isValid() and len(flat.Solids)==1,obj.Name+' flat'
    return dict(part=obj.Name,bends=len(infos),flat_mm=[flat.BoundBox.XLength,flat.BoundBox.YLength],valid=True,solids=len(flat.Solids))


def load_board_references():
    """Cache reference shapes from the existing assembly. Do not edit that document."""
    import hashlib
    source=ROOT.parent/'deimos_daq_assembly.FCStd'
    signature=hashlib.sha256(source.read_bytes()).hexdigest()
    WORK.mkdir(parents=True,exist_ok=True)
    stamp=WORK/'source.sha256'
    if stamp.exists() and stamp.read_text()==signature:
        return
    old=set(App.listDocuments())
    refdoc=App.openDocument(str(source))
    pcb=refdoc.getObject('Part__Feature1494').Shape.copy()
    pcb.exportBrep(str(WORK/'pcb.brep'))
    components=[]
    for child in refdoc.getObject('deimos_daq_board_1').Group:
        shape=Part.getShape(child)
        if not shape.isNull() and shape.Volume>30 and child.Name!='Part__Feature1494':
            components.append(shape)
    Part.makeCompound(components).exportBrep(str(WORK/'components.brep'))
    if refdoc.Name not in old:App.closeDocument(refdoc.Name)
    stamp.write_text(signature)


def joined(name, objects):
    o=doc.addObject('Part::MultiFuse',name)
    o.Shapes=objects;o.Refine=True;history.addObject(o)
    doc.recompute()
    for child in objects:style(child,visible=False)
    style(o)
    return o


def cross_bend(name, x0, length, cy, cz, sy, sz):
    # Quarter-annulus cross-section in YZ, extruded along X.
    def point(radius, angle):
        return V(x0,cy+sy*radius*math.cos(angle),cz+sz*radius*math.sin(angle))
    outer=R+T
    edges=[Part.Arc(point(outer,0),point(outer,math.pi/4),point(outer,math.pi/2)).toShape(),
           Part.makeLine(point(outer,math.pi/2),point(R,math.pi/2)),
           Part.Arc(point(R,math.pi/2),point(R,math.pi/4),point(R,0)).toShape(),
           Part.makeLine(point(R,0),point(outer,0))]
    section=feature(name+'Section',Part.Face(Part.Wire(edges)),history)
    o=doc.addObject('Part::Extrusion',name)
    o.Base=section;o.DirMode='Custom';o.Dir=V(1,0,0);o.LengthFwd=length;o.Solid=True
    history.addObject(o);doc.recompute();style(section,visible=False)
    return o


def sheet_blanks():
    b=R+T
    # Base side walls end at the front floor bend tangent.
    y0=FRONT_Y+b+RELIEF_DEPTH;y1=REAR_Y-T-.3
    bottom=native_base('FloorAndSideWalls','U-Shape',y1-y0,OUTER_W,TOP-BOTTOM,
        App.Placement(V(OUTER_W/2,y0,BOTTOM),App.Rotation(V(0,0,1),90)))
    # Local X follows world Z. The local U walls become inward corner wings.
    frontrot=App.Rotation(V(0,0,1),V(1,0,0),V(0,1,0),'ZXY')
    front=native_base('FrontAndWings','U-Shape',TOP-(BOTTOM+b),PANEL_W,WING,
        App.Placement(V(-PANEL_W/2,FRONT_Y,BOTTOM+b),frontrot))
    floorlip=Part.makeBox(OUTER_W-2*b,RELIEF_DEPTH,T,V(-OUTER_W/2+b,FRONT_Y+b,BOTTOM))
    for x in (-OUTER_W/2+b,OUTER_W/2-b-RELIEF):
        floorlip=floorlip.cut(Part.makeBox(RELIEF,.8,T+2,V(x,FRONT_Y+b,BOTTOM-1)))
    floorlip=feature('FloorFrontExtension',floorlip,history)
    frontbend=cross_bend('FrontFloorBend',-OUTER_W/2+b+RELIEF,OUTER_W-2*b-2*RELIEF,
        FRONT_Y+b,BOTTOM+b,-1,-1)
    base=joined('BaseBlank',[bottom,floorlip,front,frontbend])
    # A short U section supplies the lid side flanges. End extensions close the roof.
    roofbottom=TOP+.2
    mid0=FRONT_Y+WING+.5;mid1=REAR_Y-WING-.5
    lidrot=App.Rotation(V(1,1,0),180)
    roof=native_base('RoofAndSideFlanges','U-Shape',mid1-mid0,PANEL_W,16,
        App.Placement(V(-PANEL_W/2,mid0,roofbottom+T),lidrot))
    frontroof=Part.makeBox(PANEL_W,mid0-FRONT_Y,T,V(-PANEL_W/2,FRONT_Y,roofbottom))
    rearroof=Part.makeBox(PANEL_W,REAR_Y-b-mid1,T,V(-PANEL_W/2,mid1,roofbottom))
    for x in (-PANEL_W/2,PANEL_W/2-b-RELIEF):
        frontroof=frontroof.cut(Part.makeBox(b+RELIEF,RELIEF,T+2,V(x,mid0-RELIEF,roofbottom-1)))
        rearroof=rearroof.cut(Part.makeBox(b+RELIEF,RELIEF,T+2,V(x,mid1,roofbottom-1)))
        rearroof=rearroof.cut(Part.makeBox(b+RELIEF,.8,T+2,V(x,REAR_Y-b-.8,roofbottom-1)))
    frontroof=feature('FrontRoofExtension',frontroof,history)
    rearroof=feature('RearRoofExtension',rearroof,history)
    rearrot=App.Rotation(V(0,0,1),V(-1,0,0),V(0,-1,0),'ZXY')
    rear=native_base('RearAndWings','U-Shape',roofbottom-R-REAR_BOTTOM,PANEL_W,WING,
        App.Placement(V(PANEL_W/2,REAR_Y,REAR_BOTTOM),rearrot))
    rearbend=cross_bend('RearRoofBend',-PANEL_W/2+b+RELIEF,PANEL_W-2*b-2*RELIEF,
        REAR_Y-b,roofbottom-R,1,1)
    lid=joined('LidBlank',[roof,frontroof,rearroof,rear,rearbend])
    return base,lid


def round_corners(base, lid=False):
    radius=R+T
    indices=[]
    for i,edge in enumerate(base.Shape.Edges):
        if abs(edge.Length-T)>1e-5 or not isinstance(edge.Curve,Part.Line):continue
        p=edge.CenterOfGravity
        close=lambda a,b:abs(a-b)<1e-5
        if lid:
            roof_corner=close(abs(p.x),PANEL_W/2) and close(p.z,TOP+.2+T/2)
            wing_corner=close(abs(p.x),PANEL_W/2-T/2) and close(p.y,REAR_Y-WING) and (close(p.z,REAR_BOTTOM) or close(p.z,TOP+.2-R))
            flange_corner=close(abs(p.x),PANEL_W/2-T/2) and close(p.z,TOP+.2+T-16) and (close(p.y,FRONT_Y+WING+.5) or close(p.y,REAR_Y-WING-.5))
            selected=roof_corner or wing_corner or flange_corner
        else:
            wall_corner=close(abs(p.x),INNER_W/2+T/2) and close(p.z,TOP) and (close(p.y,FRONT_Y+R+T+RELIEF_DEPTH) or close(p.y,REAR_Y-T-.3))
            wing_corner=close(abs(p.x),PANEL_W/2-T/2) and close(p.y,FRONT_Y+WING) and (close(p.z,TOP) or close(p.z,BOTTOM+R+T))
            selected=wall_corner or wing_corner
        if selected:indices.append(i+1)
    assert len(indices)==(16 if lid else 8),(base.Name,indices)
    obj=doc.addObject('Part::Fillet','LidCornerRounds' if lid else 'BaseCornerRounds')
    obj.Base=base
    obj.Edges=[(i,radius,radius) for i in indices]
    obj.addProperty('App::PropertyLength','CornerRadius','Design').CornerRadius=radius
    obj.addProperty('App::PropertyString','Description','Design').Description='Exposed outline corners. Radius equals inside bend radius plus sheet thickness.'
    history.addObject(obj)
    doc.recompute()
    assert obj.Shape.isValid() and len(obj.Shape.Solids)==1,obj.Name
    style(base,visible=False);style(obj)
    return obj


def build():
    global doc,history,metal,reference,flats,panel_data
    load_board_references()
    doc=App.newDocument('DeimosRev8SheetMetal')
    history=group('Construction');metal=group('MetalParts');reference=group('BoardReferences');flats=group('FlatPatterns')
    dims=doc.addObject('Spreadsheet::Sheet','Dimensions')
    for row,(name,value) in enumerate([('BottomZ',BOTTOM),('FrontTopZ',TOP),('LidScrewZ',LID_SCREW_Z),('LidClearanceDiameter',LID_CLEARANCE_DIAMETER),('AssemblyNutHoleDiameter',ASSEMBLY_NUT_HOLE_DIAMETER),('MountScrewZ',MOUNT_SCREW_Z),('MountScrewY',MOUNT_SCREW_Y),('MountFlangeDiameter',MOUNT_FLANGE_DIAMETER),('WingDepth',WING),('WingScrewOffset',WING_SCREW_OFFSET),('Thickness',T),('BendRadius',R),('KFactor',K),('InnerWidth',INNER_W),('BoardWidth',223.),('BoardDepth',95.025)],1):
        dims.set(f'A{row}',name);dims.set(f'B{row}',str(value));dims.setAlias(f'B{row}',name)
    panel_data=json.loads((ROOT/'reference/pda_panels.json').read_text())
    base,lid=sheet_blanks()
    base=round_corners(base)
    lid=round_corners(lid,lid=True)
    baseholes=[];lidholes=[]
    for left in [True,False]:
        wall=-INNER_W/2 if left else INNER_W/2
        direction=V(1,0,0)
        # One accessory screw at the center of each base side wall.
        baseholes.append(cylinder(V(wall-8,MOUNT_SCREW_Y,MOUNT_SCREW_Z),ASSEMBLY_NUT_HOLE_DIAMETER/2,direction))
        # Nut installation holes stop within the gap before the outer wings.
        sign=-1 if left else 1
        outward=V(sign,0,0)
        for y in (FRONT_Y+WING_SCREW_OFFSET,MOUNT_SCREW_Y,REAR_Y-WING_SCREW_OFFSET):
            baseholes.append(cylinder(V(sign*(INNER_W/2-.1),y,LID_SCREW_Z),ASSEMBLY_NUT_HOLE_DIAMETER/2,outward,T+.2))
        # Front wings are part of the base. Rear wings and center flanges are part of the lid.
        for y,isfront in ((FRONT_Y+WING_SCREW_OFFSET,True),(MOUNT_SCREW_Y,False),(REAR_Y-WING_SCREW_OFFSET,False)):
            tool=cylinder(V(sign*(PANEL_W/2-T-.1),y,LID_SCREW_Z),LID_CLEARANCE_DIAMETER/2,outward,T+.2)
            (baseholes if isfront else lidholes).append(tool)
    chassis,front_anchors,front_refs=make_panel('front',base,'BaseChassis',baseholes)
    lid_final,rear_anchors,rear_refs=make_panel('rear',lid,'Lid',lidholes)
    pcbshape=Part.Shape();pcbshape.read(str(WORK/'pcb.brep'))
    pcb=feature('Rev8PCB',pcbshape,reference);style(pcb,(.08,.40,.17))
    cs=Part.Shape();cs.read(str(WORK/'components.brep'))
    cs=Part.makeCompound([s for s in cs.Solids if abs(s.Volume-pcbshape.Volume)>.01])
    comp=feature('Rev8ComponentReference',cs,reference);style(comp,(.28,.32,.30))
    artfront=artwork('front',front_anchors);artrear=artwork('rear',rear_anchors)
    horizontal_indicator_labels()
    artlid=move_branding_to_lid()
    artfront=list(doc.FrontArtwork.Group)
    doc.recompute()
    export_art('front',artfront,front_refs);export_art('rear',artrear,rear_refs)
    export_lid_art(doc)
    report={'units':'mm','parts':[],'intersections_mm3':{},'unfolds':[],
            'construction':'Two sheets: base with front; lid with rear; overlapping corner wings',
            'corner_radius_mm':R+T,'rounded_corners':{'base':8,'lid':16},
            'wing_depth':WING,'wing_sidewall_clearance':SIDE_GAP,
            'board_support':'Connector seats at front and rear panels; no card guides',
            'assembly_screws':{'thread':'6-32','type':'machine screw','count':6,
                               'nut':'PEM F-632-1','nut_count':6,
                               'nut_hole_diameter_mm':ASSEMBLY_NUT_HOLE_DIAMETER,
                               'nut_hole_tolerance_mm':[0,.003*25.4],
                               'hardware_modeled':False,
                               'positions_y_mm':[FRONT_Y+WING_SCREW_OFFSET,MOUNT_SCREW_Y,REAR_Y-WING_SCREW_OFFSET],
                               'center_z_mm':LID_SCREW_Z,
                               'outer_clearance_diameter_mm':LID_CLEARANCE_DIAMETER,
                               'lid_clearance_diameter_mm':LID_CLEARANCE_DIAMETER},
            'text_count':sum(hasattr(o,'String') for o in artfront+artrear+artlid)}
    low=min(a.Shape.BoundBox.ZMin for a,_ in front_anchors)
    high=max(o.Shape.BoundBox.ZMax for o in artfront if hasattr(o,'String'))
    half_ba=math.pi/4*(R+K*T)
    report['accessory_mounts']={
        'count':2,'nut':'PEM F-632-1','thread':'6-32','screw_type':'machine screw',
        'nut_hole_diameter_mm':ASSEMBLY_NUT_HOLE_DIAMETER,
        'center_y_mm':MOUNT_SCREW_Y,'center_z_mm':MOUNT_SCREW_Z,
        'center_above_base_mm':MOUNT_SCREW_Z-BOTTOM,
        'flange_diameter_mm':MOUNT_FLANGE_DIAMETER,
        'flange_to_bend_tangent_mm':MOUNT_SCREW_Z-MOUNT_FLANGE_DIAMETER/2-(BOTTOM+R+T),
        'hole_edge_to_flat_bend_center_mm':MOUNT_SCREW_Z-ASSEMBLY_NUT_HOLE_DIAMETER/2-(BOTTOM+R+T)+half_ba,
        'flange_diameter_source':'User supplied for McMaster 93882A146',
        'nut_center_to_bend_tangent_mm':MOUNT_SCREW_Z-(BOTTOM+R+T),
        'nut_minimum_bend_clearance_mm':.270*25.4}
    assert report['accessory_mounts']['flange_to_bend_tangent_mm']>.5
    assert report['accessory_mounts']['hole_edge_to_flat_bend_center_mm']>=DIE_WIDTH/2+.5
    assert report['accessory_mounts']['nut_center_to_bend_tangent_mm']>=.270*25.4+.5
    report['height_design']={
        'bottom_z':BOTTOM,'front_top_z':TOP,'overall_height_mm':TOP+.2+T-BOTTOM,
        'front_cutout_min_z':low,'front_text_max_z':high,
        'front_cutout_to_bend_tangent_mm':low-(BOTTOM+T+R),
        'front_cutout_to_flat_bend_center_mm':low-(BOTTOM+T+R)+half_ba,
        'die_half_width_mm':DIE_WIDTH/2,'front_text_top_margin_mm':TOP-high,
        'rear_text_to_bend_tangent_mm':TOP+.2-R-max(o.Shape.BoundBox.ZMax for o in artrear),
        'lid_screw_z':LID_SCREW_Z,'indicator_cap_height_mm':1.4}
    assert report['height_design']['front_cutout_to_flat_bend_center_mm']>=DIE_WIDTH/2+.5
    assert TOP-high>=TEXT_TOP_MARGIN
    allmetal=[chassis,lid_final]
    for o in allmetal:
        assert o.Shape.isValid() and len(o.Shape.Solids)==1,o.Name
        report['parts'].append({'name':o.Name,'valid':True,'volume_mm3':o.Shape.Volume})
        Import.export([o],str(ROOT/'parts'/f'{o.Name}.step'))
        report['unfolds'].append(unfold_part(o))
    for i,a in enumerate(allmetal):
        for b in allmetal[i+1:]+[pcb]:report['intersections_mm3'][a.Name+' / '+b.Name]=a.Shape.common(b.Shape).Volume
    for o in allmetal:
        report['intersections_mm3'][o.Name+' / component reference']=o.Shape.common(comp.Shape).Volume
    report['connector_cutouts']={'front':len(front_anchors),'rear':len(rear_anchors)}
    errors=[]
    for side,anchors,refs in [('front',front_anchors,front_refs),('rear',rear_anchors,rear_refs)]:
        for (anchor,_),wire in zip(anchors,refs):
            expected=wire.transformed(outside_plane(side).toMatrix()).BoundBox
            actual=anchor.Shape.BoundBox
            errors.extend(abs(getattr(actual,k)-getattr(expected,k)) for k in ['XMin','YMin','ZMin','XMax','YMax','ZMax'])
    report['source_cutout_bounds_max_error_mm']=max(errors)
    (ROOT/'validation.json').write_text(json.dumps(report,indent=2))
    # Construction tools must remain hidden after boolean recomputes.
    for o in history.Group:style(o,visible=False)
    for o in flats.Group:style(o,visible=False)
    if App.GuiUp:
        import FreeCADGui as Gui
        Gui.activeDocument().activeView().viewAxonometric()
        lid_final.ViewObject.Visibility=False
        for o in artrear+artlid:style(o,visible=False)
        Gui.activeDocument().activeView().fitAll()
        Gui.activeDocument().activeView().saveImage(str(ROOT/'assembly_open.png'),1800,1100,'White')
        lid_final.ViewObject.Visibility=True
        for o in artrear+artlid:
            style(o,(.9,.9,.9))
            o.ViewObject.LineColor=(.9,.9,.9)
            if o.Shape.Faces:o.ViewObject.DiffuseColor=[(.9,.9,.9)]*len(o.Shape.Faces)
        doc.recompute();Gui.updateGui()
        Gui.activeDocument().activeView().fitAll()
        Gui.activeDocument().activeView().saveImage(str(ROOT/'assembly_closed.png'),1800,1100,'White')
        Gui.Selection.clearSelection()
        import runpy
        runpy.run_path(str(ROOT/'scripts/views.py'))['capture_exploded'](doc,ROOT)
    doc.recompute()
    doc.saveAs(str(ROOT/'DeimosDaqRev8_SheetMetal.FCStd'))
    for obsolete in ('FrontPanel.step','RearPanel.step','FrontPanel_flat.svg','RearPanel_flat.svg',
                     'BaseBlank_flat.svg','LidBlank_flat.svg'):
        (ROOT/'parts'/obsolete).unlink(missing_ok=True)
    (ROOT/'build_error.txt').unlink(missing_ok=True)
    print('BUILD COMPLETE',ROOT)
    return doc

if __name__=='__main__':
    try:
        build()
    except Exception:
        (ROOT/'build_error.txt').write_text(traceback.format_exc())
        raise
