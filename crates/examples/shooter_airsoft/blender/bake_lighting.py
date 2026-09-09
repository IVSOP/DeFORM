"""Bake static direct and indirect diffuse light into UV1; run through scripts/bake-lighting.sh.

The color pass is excluded: Bevy multiplies this lighting by the PBR albedo.
Runtime lamps light dynamic objects; their static diffuse contribution is disabled.
"""
import json
import math
import os
import sys
from pathlib import Path
import struct
import time
import zlib

import bpy
import numpy as np

ROOT = Path(__file__).resolve().parents[1]
SIZE = int(os.environ.get('AIRSOFT_BAKE_SIZE', '4096'))
SAMPLES = int(os.environ.get('AIRSOFT_BAKE_SAMPLES', '64'))
UV = 'LightmapUV'
IMAGE_PATH = 'lightmaps/bomb_house_diffuse.png'
sys.path.insert(0, str(ROOT / 'blender'))
from lightmap_filter import denoise
# Fail before the expensive bake if the offline filter is unavailable.
import shutil
assert shutil.which('oidnDenoise'), 'Install Open Image Denoise (oidnDenoise) first'
started = time.monotonic()
assert SIZE >= 256 and SAMPLES > 0

bpy.context.window.scene = bpy.data.scenes['BOMB HOUSE']
scene = bpy.context.scene

# Author the same sun direction/intensity as the runtime. It is exported too.
from mathutils import Vector
sun = scene.objects.get('Warehouse sun')
if sun is None:
    sun = bpy.data.objects.new('Warehouse sun', bpy.data.lights.new('Warehouse sun', 'SUN'))
    scene.collection.objects.link(sun)
    sun.data.energy = 12000.0 / 683.0
    sun.data.angle = math.radians(0.526)
    sun.location = (-8, -12, 30)
    sun.rotation_euler = (Vector((0, 0, 0)) - sun.location).to_track_quat('-Z', 'Y').to_euler()

def emissive(material):
    return material and material.use_nodes and any(
        n.type == 'EMISSION' or (n.type == 'BSDF_PRINCIPLED' and
        (n.inputs['Emission Strength'].is_linked or n.inputs['Emission Strength'].default_value > 0))
        for n in material.node_tree.nodes)

receivers = [o for o in scene.objects if o.type == 'MESH' and not o.hide_render
             and o.data.polygons and o.data.materials
             and not any(emissive(m) for m in o.data.materials)]
assert receivers, 'No lightmap receivers found'
for obj in scene.objects:
    if 'airsoft_lightmap' in obj:
        del obj['airsoft_lightmap']
if bpy.context.object and bpy.context.object.mode != 'OBJECT':
    bpy.ops.object.mode_set(mode='OBJECT')
bpy.ops.object.select_all(action='DESELECT')
for obj in receivers:
    obj.hide_set(False)
    obj.select_set(True)
    if obj.data.users > 1:
        obj.data = obj.data.copy()
    mesh = obj.data
    if not mesh.uv_layers:
        mesh.uv_layers.new(name='UVMap')
    if UV not in mesh.uv_layers:
        mesh.uv_layers.new(name=UV)
    assert mesh.uv_layers.find(UV) == 1, f'{obj.name}: LightmapUV must be the second UV layer'
    mesh.uv_layers.active_index = 1
    mesh.uv_layers[0].active_render = True
bpy.context.view_layer.objects.active = receivers[0]
# Multi-object edit mode packs ALL receivers into a single non-overlapping atlas.
bpy.ops.object.mode_set(mode='EDIT')
bpy.ops.mesh.select_all(action='SELECT')
bpy.ops.uv.smart_project(angle_limit=math.radians(66), island_margin=0.0)
bpy.ops.uv.pack_islands(rotate=True, scale=True, margin_method='FRACTION',
                        margin=10.0 / SIZE, shape_method='CONVEX')
bpy.ops.object.mode_set(mode='OBJECT')

target = bpy.data.images.new('Bomb house | indirect bake working', width=SIZE, height=SIZE,
                             alpha=True, float_buffer=True)
target.colorspace_settings.name = 'Non-Color'
target.generated_color = (0, 0, 0, 1)
materials = {m for obj in receivers for m in obj.data.materials if m}
targets = []
for material in materials:
    material.use_nodes = True
    nodes = material.node_tree.nodes
    for node in nodes:
        node.select = False
    node = nodes.new('ShaderNodeTexImage')
    node.name = 'Lightmap bake target'
    node.label = 'Indirect lighting → Bevy (not connected to base color)'
    node.image = target
    node.select = True
    nodes.active = node
    targets.append((material, node))

scene.render.engine = 'CYCLES'
scene.cycles.samples = SAMPLES
scene.cycles.diffuse_bounces = 4
scene.cycles.max_bounces = 8
scene.cycles.use_denoising = False
scene.render.bake.use_selected_to_active = False
scene.render.bake.margin = 8
scene.cycles.device = 'CPU'
device_name = 'CPU'
prefs = bpy.context.preferences.addons['cycles'].preferences
requested = os.environ.get('AIRSOFT_BAKE_DEVICE', 'AUTO').upper()
if requested != 'CPU':
    for backend in ([requested] if requested != 'AUTO' else ['OPTIX', 'CUDA', 'HIP', 'METAL', 'ONEAPI']):
        try:
            prefs.compute_device_type = backend
            prefs.get_devices()
            available = [d for d in prefs.devices if d.type == backend]
            if available:
                for d in prefs.devices:
                    d.use = d.type == backend
                scene.cycles.device = 'GPU'
                device_name = backend
                break
        except (TypeError, RuntimeError):
            continue
    if requested != 'AUTO' and device_name == 'CPU':
        raise RuntimeError(f'Requested Cycles backend {requested} is unavailable')

print(f'BAKE: {len(receivers)} receivers, {SIZE}², {SAMPLES} samples, {device_name}', flush=True)
bpy.ops.object.bake(type='DIFFUSE', pass_filter={'DIRECT', 'INDIRECT'}, uv_layer=UV,
                    margin=8, margin_type='EXTEND', use_clear=False)

pixels = np.empty(SIZE * SIZE * 4, dtype=np.float32)
target.pixels.foreach_get(pixels)
rgb = pixels.reshape(SIZE, SIZE, 4)[:, :, :3]
assert np.isfinite(rgb).all(), 'Bake contains invalid pixels'
rgb = denoise(rgb)
peak = float(rgb.max())
assert peak > 0.00001, 'Bake is black: check the scene lighting and UVs'
# Store normalized sRGB; the component restores the linear HDR range in Bevy.
# Write PNG bytes directly to avoid applying Blender's AgX display transform.
linear = np.clip(rgb / peak, 0, 1)
encoded = np.where(linear <= 0.0031308, linear * 12.92,
                   1.055 * np.power(linear, 1 / 2.4) - 0.055)
encoded = np.rint(encoded * 255).astype(np.uint8)[::-1]  # Blender pixels start at bottom.
def chunk(kind, data):
    return struct.pack('!I', len(data)) + kind + data + struct.pack('!I', zlib.crc32(kind + data))
png = b'\x89PNG\r\n\x1a\n'
png += chunk(b'IHDR', struct.pack('!2I5B', SIZE, SIZE, 8, 2, 0, 0, 0))
png += chunk(b'sRGB', b'\0')
png += chunk(b'IDAT', zlib.compress(b''.join(b'\0' + row.tobytes() for row in encoded)))
png += chunk(b'IEND', b'')
destination = ROOT / 'assets' / IMAGE_PATH
destination.parent.mkdir(parents=True, exist_ok=True)
destination.write_bytes(png)

# glTF's standard conversion from radiant to photometric light units.
exposure = peak * 683.0
for obj in receivers:
    obj['airsoft_lightmap'] = {'image': IMAGE_PATH, 'exposure': exposure}
    obj.data.uv_layers.active_index = 0
    obj.data.uv_layers[0].active_render = True
for material, node in targets:
    material.node_tree.nodes.remove(node)
bpy.data.images.remove(target)
report = {
    'image': IMAGE_PATH, 'resolution': SIZE, 'samples': SAMPLES,
    'denoiser': 'Open Image Denoise / RTLightmap',
    'device': device_name, 'pass': 'DIFFUSE / DIRECT + INDIRECT (no COLOR)',
    'linear_peak': peak, 'exposure': exposure,
    'lit_pixel_fraction': float(np.any(rgb > 0.00001, axis=2).mean()),
    'duration_seconds': round(time.monotonic() - started, 1),
    'receivers': sorted(o.name for o in receivers),
}
(destination.parent / 'bake.json').write_text(json.dumps(report, indent=2) + '\n')
bpy.ops.wm.save_as_mainfile(filepath=str(ROOT / 'blender/bomb_house.blend'))
print(f'BAKE COMPLETE: {destination}, peak={peak:.4f}, exposure={exposure:.2f}, '
      f'{report["duration_seconds"]} seconds', flush=True)
