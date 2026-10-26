"""Create an exploded display copy without moving the working geometry."""
import FreeCAD as App
import FreeCADGui as Gui
import Part


def capture_exploded(doc, root):
    old=doc.getObject('ExplodedView')
    if old:
        for obj in list(old.Group):doc.removeObject(obj.Name)
        doc.removeObject(old.Name)
    visible_names=[o.Name for o in doc.Objects if hasattr(o,'Shape') and o.ViewObject.Visibility]
    group=doc.addObject('App::DocumentObjectGroup','ExplodedView')
    group.addProperty('App::PropertyString','Description').Description='Display copies. Rebuild after geometry edits.'
    plug=Part.read(str(root.parent/'terminal_block/15EDGKM-3.81-12P-1Y-00A&Z(H).stp'))
    components=Part.makeCompound([s for s in doc.Rev8ComponentReference.Shape.Solids if abs(s.Volume-plug.Volume)>=.1])
    shift=App.Vector(0,20,65)

    def display(name, shape, color):
        obj=doc.addObject('Part::Feature',name)
        obj.Shape=shape;group.addObject(obj)
        obj.ViewObject.ShapeColor=color
        if shape.Faces:obj.ViewObject.DiffuseColor=[color]*len(shape.Faces)
        obj.ViewObject.LineColor=(.08,.08,.08)
        obj.ViewObject.DisplayMode='Flat Lines'
        return obj

    for name in ('BaseChassis','Rev8PCB','Rev8ComponentReference','Lid'):
        src=doc.getObject(name)
        shape=(components if name=='Rev8ComponentReference' else src.Shape).copy()
        if name=='Lid':shape.translate(shift)
        display(name+'Exploded',shape,src.ViewObject.ShapeColor)
    for side in ('Front','Rear','Lid'):
        if not doc.getObject(side+'Artwork'):continue
        shape=Part.makeCompound([o.Shape for o in doc.getObject(side+'Artwork').Group])
        if side in ('Rear','Lid'):shape.translate(shift)
        display(side+'ArtworkExploded',shape,(.9,.9,.9))
    for name in visible_names:doc.getObject(name).ViewObject.Visibility=False
    doc.recompute()
    group.ViewObject.Visibility=True
    for obj in group.Group:obj.ViewObject.Visibility=True
    Gui.updateGui()
    view=Gui.activeDocument().activeView()
    view.viewAxonometric();view.fitAll()
    Gui.updateGui()
    view.saveImage(str(root/'assembly_exploded.png'),1800,1400,'White')
    for obj in list(group.Group):doc.removeObject(obj.Name)
    doc.removeObject(group.Name)
    doc.recompute()
    for name in visible_names:
        obj=doc.getObject(name)
        if obj:obj.ViewObject.Visibility=True
    view.fitAll()
