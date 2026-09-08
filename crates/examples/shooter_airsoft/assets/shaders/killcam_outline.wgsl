#import bevy_pbr::{forward_io::VertexOutput, mesh_view_bindings::view}

@fragment
fn fragment(in: VertexOutput) -> @location(0) vec4<f32> {
    let to_camera = normalize(view.world_position - in.world_position.xyz);
    let facing = abs(dot(normalize(in.world_normal), to_camera));
    // A narrow silhouette rim, with width measured in screen derivatives.
    let width = max(fwidth(facing) * 3.0, 0.035);
    if facing > width {
        discard;
    }
    return vec4(1.0, 0.015, 0.005, 1.0);
}
