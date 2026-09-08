"""Rebuild the original BOMB HOUSE scene. Export separately to preserve hand edits.
blender --background -noaudio --python-exit-code 1 --python blender/build_level.py --python blender/export_level.py
All authored coordinates below are Bevy (X, up Y, Z), in metres.
"""
import bpy, math, random
import runpy
from pathlib import Path
ROOT = Path(__file__).resolve().parents[1]
rng = random.Random(812)
bpy.ops.wm.read_factory_settings(use_empty=True)
scene = bpy.context.scene
scene.name = 'BOMB HOUSE'
scene.unit_settings.system = 'METRIC'

def material(name, color, roughness=0.85, emission=0):
    m = bpy.data.materials.new(name); m.use_nodes = True
    bs = m.node_tree.nodes.get('Principled BSDF')
    bs.inputs['Base Color'].default_value = (*color,1)
    bs.inputs['Roughness'].default_value = roughness
    if emission:
        bs.inputs['Emission Color'].default_value = (*color,1)
        bs.inputs['Emission Strength'].default_value = emission
    return m

wood=material('Birch plywood | warm grain',(0.68,0.47,0.26))
stud=material('Sawn pine framing',(0.57,0.39,0.21))
steel=material('Graphite roof steel',(0.065,0.075,0.085),0.6)
concrete=material('Worn concrete',(0.25,0.26,0.25))
white=material('Warm white markings',(0.81,0.8,0.7))
blue=material('Team A | blue',(0.035,0.24,0.42))
red=material('Team B | orange',(0.67,0.18,0.045))
lightmat=material('Fluorescent diffuser',(0.82,0.91,1.0),0.4,5)

def box(name,pos,size,mat=wood,solid=True):
    x,y,z=pos; w,h,d=size
    bpy.ops.mesh.primitive_cube_add(size=1,location=(x,-z,y))
    ob=bpy.context.object; ob.name=name; ob.scale=(w,d,h)
    bpy.ops.object.transform_apply(location=False,rotation=False,scale=True)
    ob.data.materials.append(mat)
    # Metric face UVs, with grain following the upright dimension.
    for poly in ob.data.polygons:
        normal=poly.normal
        for li in poly.loop_indices:
            co=ob.data.vertices[ob.data.loops[li].vertex_index].co
            if abs(normal.z)>0.5: uv=(co.x/0.75,co.y/0.75)
            elif abs(normal.x)>0.5: uv=(co.y/0.75,co.z/0.75)
            else: uv=(co.x/0.75,co.z/0.75)
            ob.data.uv_layers.active.data[li].uv=uv
    ob['airsoft_solid']=solid
    return ob

def panel_x(name,x,z,length,height=2.8,base=0):
    # Panel seams and framing are geometry, not painted onto an uninterrupted box.
    n=math.ceil(length/1.22); span=length/n
    for i in range(n):
        xx=x-length/2+(i+0.5)*span
        box(name+' sheet',(xx,base+height/2,z),(span-0.008,height,0.12))
    for i in range(n+1):
        xx=x-length/2+i*span
        box(name+' upright',(xx,base+height/2,z+0.095),(0.075,height,0.075),stud)
    for yy in (base+0.075,base+height-0.075):box(name+' plate',(x,yy,z+0.095),(length,0.1,0.075),stud)

def panel_z(name,x,z,length,height=2.8,base=0):
    n=math.ceil(length/1.22); span=length/n
    for i in range(n):
        zz=z-length/2+(i+0.5)*span
        box(name+' sheet',(x,base+height/2,zz),(0.12,height,span-0.008))
    for i in range(n+1):
        zz=z-length/2+i*span
        box(name+' upright',(x+0.095,base+height/2,zz),(0.075,height,0.075),stud)
    for yy in (base+0.075,base+height-0.075):box(name+' plate',(x+0.095,yy,z),(0.075,0.1,length),stud)

def door_x(name,x,z,width=5.0,lintel=True):
    opening=1.6; side=(width-opening)/2
    for sign in (-1,1):panel_x(name,x+sign*(opening+side)/2,z,side)
    if lintel: panel_x(name+' lintel',x,z,opening,0.5,2.3)

def window_z(name,x,z,length=5):
    panel_z(name+' sill',x,z,length,1.15)
    panel_z(name+' header',x,z,length,0.6,2.2)
    for sign in (-1,1):panel_z(name+' pier',x,z+sign*(length/2-0.5),1,1.05,1.15)

def text(name,body,pos,mat,size=0.35,flip=False):
    # Text faces +Z in Bevy; flip signs on the +Z wall to face into the arena.
    bpy.ops.object.text_add(location=(pos[0],-pos[2],pos[1]),rotation=(math.pi/2,0,math.pi if flip else 0))
    ob=bpy.context.object;ob.name=name;ob.data.body=body;ob.data.align_x='CENTER';ob.data.size=size;ob.data.extrude=0.0005;ob.data.materials.append(mat)
    bpy.ops.object.convert(target='MESH')
    ob['airsoft_solid']=False

box('Warehouse slab',(0,-0.16,0),(24,0.32,36),concrete)
for x in (-12,12):box('Warehouse side',(x,4,0),(0.2,8,36),steel)
for z in (-18,18):box('Warehouse end',(0,4,z),(24,8,0.2),steel)
# A long central lane, rooms either side, and two independent wide flank routes.
for x in (-8.5,8.5):panel_z('Field perimeter',x,0,32)
for z in (-16,16):panel_x('Spawn backstop',0,z,17)
for z in (-10,-3,4,11):
    for x in (-4.8,4.8):
        door_x('Room doorway',x,z,5.8,lintel=not(x<0 and z==4))
for x in (-1.9,1.9):
    for z in (-6.5,0.5,7.5):window_z('Room firing window',x,z,5)
# Offset cover breaks the spawn-to-spawn sightline, leaving 1.8m passages.
for z,x in ((-11.8,-0.9),(0,0.9),(11.8,-0.9)):
    panel_x('Central offset barricade',x,z,2.0,2.4)
for z in (-6.5,6.5):
    for x in (-6.5,6.5):
        box('Plywood cover crate',(x,0.65,z),(1.3,1.3,1.3))
        for yy in (0.12,1.18):box('Crate strap',(x,yy,z+0.67),(1.35,0.09,0.05),stud)
# Raised observation/fighting deck, stairs within the west room strip.
box('Raised deck',(-5.2,2.75,0.2),(5.3,0.25,4.8))
for x in (-7.7,-2.7):
    for z in (-2,2.4):box('Deck support',(x,1.35,z),(0.16,2.7,0.16),stud)
for x in (-7.8,-2.6):
    for z in (-2.1,0.2,2.5):box('Balcony post',(x,3.4,z),(0.1,1.3,0.1),stud)
    for y in (3.2,3.9):box('Balcony rail',(x,y,0.2),(0.09,0.09,4.8),stud)
for i in range(15):
    h=(i+1)*2.875/15
    box('Stair tread',(-4.8,h/2,6.9-i*0.3),(1.6,h,0.3))
# High roof with open skylight bands and exposed structural trusses.
for x in (-10,-5,0,5,10):box('Roof sheet',(x,8.05,0),(3.7,0.13,36),steel)
for z in (-16,-8,0,8,16):
    box('Roof bottom chord',(0,6.8,z),(24,0.12,0.14),steel)
    box('Roof top chord',(0,7.8,z),(24,0.12,0.14),steel)
    for x in range(-10,12,2):
        ob=box('Truss diagonal',(x,7.3,z),(2.2,0.07,0.07),steel,False)
        ob.rotation_euler[1]=math.radians(27 if x%4 else -27)
    for x in (-11.6,11.6):box('Warehouse column',(x,3.4,z),(0.18,6.8,0.2),steel)
for z in (-12,-4,4,12):
    for x in (-5,5):
        box('Suspended light housing',(x,6.2,z),(0.35,0.12,1.8),steel,False)
        box('Fluorescent tube',(x,6.12,z),(0.24,0.035,1.65),lightmat,False)
        data=bpy.data.lights.new('Ceiling lamp','SPOT');data.energy=170;data.shadow_soft_size=0.35
        data.spot_size=math.radians(160);data.spot_blend=1-65/80
        ob=bpy.data.objects.new('Ceiling lamp',data);scene.collection.objects.link(ob);ob.location=(x,-z,5.8)
for sign,mat,label in ((1,blue,'A'),(-1,red,'B')):
    z=sign*15.91
    box('Spawn stripe '+label,(0,1.4,z),(16.8,0.22,0.015),mat,False)
    text('Spawn label '+label,label+'  /  READY ZONE',(4,1.9,z-sign*0.03),white,0.46,sign>0)
    empty=bpy.data.objects.new('Spawn '+label,None);scene.collection.objects.link(empty)
    empty.location=(0,-sign*14.4,0);empty['airsoft_spawn']=0 if sign>0 else 1;empty['yaw']=0.0 if sign>0 else math.pi
for z,label in ((-10,'01'),(-3,'02'),(4,'03'),(11,'04')):
    text('Room number',label,(-6.7,1.7,z+0.075),white,0.42)
text('Arena title','BOMB HOUSE  /  CQB TRAINING',(0,4.6,17.85),white,0.65,True)
scene.world=bpy.data.worlds.new('Warehouse daylight');scene.world.use_nodes=True
scene.world.node_tree.nodes['Background'].inputs[0].default_value=(0.3,0.4,0.55,1)
scene.world.node_tree.nodes['Background'].inputs[1].default_value=0.35
runpy.run_path(str(ROOT/'blender/apply_materials.py'))
bpy.ops.wm.save_as_mainfile(filepath=str(ROOT/'blender/bomb_house.blend'))
print('Built editable BOMB HOUSE')
