"""Verify the shipped GLB and headless collision data describe the same geometry."""
import json,struct,itertools
from pathlib import Path
import numpy as np
root=Path(__file__).resolve().parents[1]
data=(root/'assets/levels/bomb_house.glb').read_bytes()
magic,version,total=struct.unpack_from('<4sII',data)
assert (magic,version,total)==(b'glTF',2,len(data))
length,kind=struct.unpack_from('<I4s',data,12);assert kind==b'JSON'
gltf=json.loads(data[20:20+length])
manifest=json.loads((root/'assets/levels/collision.json').read_text())

def rotation(q):
 x,y,z,w=q
 return np.array([[1-2*y*y-2*z*z,2*x*y-2*z*w,2*x*z+2*y*w],
 [2*x*y+2*z*w,1-2*x*x-2*z*z,2*y*z-2*x*w],
 [2*x*z-2*y*w,2*y*z+2*x*w,1-2*x*x-2*y*y]])
def matrix(n):
 if 'matrix' in n:return np.array(n['matrix']).reshape((4,4)).T
 m=np.eye(4);m[:3,:3]=rotation(n.get('rotation',[0,0,0,1]))@np.diag(n.get('scale',[1,1,1]));m[:3,3]=n.get('translation',[0,0,0]);return m
world={}
def visit(i,parent):
 n=gltf['nodes'][i];world[i]=parent@matrix(n)
 for child in n.get('children',[]):visit(child,world[i])
for i in gltf['scenes'][gltf.get('scene',0)]['nodes']:visit(i,np.eye(4))
solids={n['name']:i for i,n in enumerate(gltf['nodes']) if n.get('extras',{}).get('airsoft_solid')}
assert len(solids)==len(manifest['solids'])>500
for s in manifest['solids']:
 i=solids[s['name']];n=gltf['nodes'][i]
 primitive=gltf['meshes'][n['mesh']]['primitives'][0]
 accessor=gltf['accessors'][primitive['attributes']['POSITION']]
 corners=np.array(list(itertools.product(*zip(accessor['min'],accessor['max']))))
 actual=(world[i]@np.column_stack([corners,np.ones(8)]).T).T[:,:3]
 expected=np.array(list(itertools.product(*[(-v/2,v/2) for v in s['size']])))@rotation(s['rotation']).T+np.array(s['center'])
 assert np.allclose(actual.min(0),expected.min(0),atol=0.0001),s['name']
 assert np.allclose(actual.max(0),expected.max(0),atol=0.0001),s['name']
spawns=[(i,n) for i,n in enumerate(gltf['nodes']) if 'airsoft_spawn' in n.get('extras',{})]
assert len(spawns)==2
for i,n in spawns:
 s=manifest['spawns'][n['extras']['airsoft_spawn']]
 assert np.allclose(world[i][:3,3],s['position'],atol=0.0001)
assert manifest['spawns'][0]['position'][2]>14 and manifest['spawns'][1]['position'][2]<-14
assert len(gltf['images'])>=1 and all('bufferView' in im for im in gltf['images'])
lights=gltf['extensions']['KHR_lights_punctual']['lights']
assert len(lights)==8
assert all(light['type']=='spot' for light in lights), 'Ceiling lamps must use one shadow view each'
for i,n in enumerate(gltf['nodes']):
 if 'KHR_lights_punctual' in n.get('extensions',{}):
  direction=world[i][:3,:3]@np.array([0,0,-1])
  assert np.allclose(direction,[0,-1,0],atol=0.0001), 'Ceiling lamps must face down'
# The committed Rust output must still match the manifest after rustfmt.
import re
rust=(root/'src/arena_data.rs').read_text()
body=rust.split('= &[',1)[1].split('];',1)[0]
values=[float(v) for v in re.findall(r'-?\d+\.\d+',body)]
expected=[v for s in manifest['solids'] for key in ('center','size','rotation') for v in s[key]]
assert np.allclose(values,expected,atol=0.000001)
print(f'PASS: {len(solids)} visual/collision bounds agree; two opposite spawns; packed textures; eight lights; Rust data matches.')
