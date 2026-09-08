"""Apply packed, glTF-supported Poly Haven PBR materials. Does not rebuild geometry."""
import bpy
from pathlib import Path
ROOT=Path(__file__).resolve().parents[1]

def apply(material_name,asset,rough_suffix):
    mat=bpy.data.materials[material_name];mat.use_nodes=True
    nodes=mat.node_tree.nodes;links=mat.node_tree.links;nodes.clear()
    bs=nodes.new('ShaderNodeBsdfPrincipled');out=nodes.new('ShaderNodeOutputMaterial');links.new(bs.outputs['BSDF'],out.inputs['Surface'])
    def texture(suffix,linear=False):
        image=bpy.data.images.load(str(ROOT/'assets/textures'/f'{asset}_{suffix}_1k.jpg'),check_existing=True)
        if linear:image.colorspace_settings.name='Non-Color'
        image.pack();node=nodes.new('ShaderNodeTexImage');node.image=image;return node
    links.new(texture('diff').outputs['Color'],bs.inputs['Base Color'])
    if rough_suffix=='arm':
        channels=nodes.new('ShaderNodeSeparateColor');links.new(texture('arm',True).outputs['Color'],channels.inputs['Color'])
        links.new(channels.outputs['Green'],bs.inputs['Roughness'])
    else:links.new(texture('rough',True).outputs['Color'],bs.inputs['Roughness'])
    normal=nodes.new('ShaderNodeNormalMap');normal.inputs['Strength'].default_value=0.45
    links.new(texture('nor_gl',True).outputs['Color'],normal.inputs['Color']);links.new(normal.outputs['Normal'],bs.inputs['Normal'])
apply('Birch plywood | warm grain','plywood','rough')
apply('Worn concrete','concrete_floor','arm')
print('Applied and packed plywood and concrete PBR materials')
