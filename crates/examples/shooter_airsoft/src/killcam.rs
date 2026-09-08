//! Render-only death camera and depth-independent silhouette material.
use bevy::{
    mesh::MeshVertexBufferLayoutRef,
    pbr::{MaterialPipeline, MaterialPipelineKey},
    prelude::*,
    render::render_resource::{
        AsBindGroup, CompareFunction, RenderPipelineDescriptor, SpecializedMeshPipelineError,
    },
    shader::ShaderRef,
};
use deform_core::Pubkey;
use shooter_airsoft::shooter_logic::{PLAYER_EYE_HEIGHT, PlayerState};

#[derive(Resource, Default)]
pub struct PlayerView {
    pub dead: bool,
    pub round: Option<u32>,
}

#[derive(Component)]
pub struct KillerOutline(pub Pubkey);

#[derive(Asset, TypePath, AsBindGroup, Debug, Clone)]
pub struct OutlineMaterial {}

impl Material for OutlineMaterial {
    fn fragment_shader() -> ShaderRef {
        "shaders/killcam_outline.wgsl".into()
    }

    fn enable_prepass() -> bool {
        false
    }
    fn enable_shadows() -> bool {
        false
    }

    fn alpha_mode(&self) -> AlphaMode {
        AlphaMode::Blend
    }

    fn specialize(
        _pipeline: &MaterialPipeline,
        descriptor: &mut RenderPipelineDescriptor,
        _layout: &MeshVertexBufferLayoutRef,
        _key: MaterialPipelineKey<Self>,
    ) -> Result<(), SpecializedMeshPipelineError> {
        if let Some(depth) = &mut descriptor.depth_stencil {
            depth.depth_compare = Some(CompareFunction::Always);
            depth.depth_write_enabled = Some(false);
        }
        Ok(())
    }
}

pub fn death_camera(victim: &PlayerState, killer: &PlayerState) -> Transform {
    let death = victim.death.as_ref().expect("death camera needs a victim");
    Transform::from_translation(death.camera_position)
        .looking_at(killer.pos + Vec3::Y * (PLAYER_EYE_HEIGHT * 0.5), Vec3::Y)
}

#[cfg(test)]
mod tests {
    use shooter_airsoft::shooter_logic::DeathState;

    use super::*;

    #[test]
    fn camera_stays_at_death_position_while_tracking_moving_killer() {
        let eye = Vec3::new(1.0, 1.7, 4.0);
        let mut victim = PlayerState {
            death: Some(DeathState {
                killer: Pubkey::default(),
                camera_position: eye,
            }),
            ..default()
        };
        for position in [Vec3::new(3.0, 1.0, -4.0), Vec3::new(-2.0, 2.0, 1.0)] {
            victim.pos += Vec3::new(0.5, -0.3, 0.0);
            let killer = PlayerState {
                pos: position,
                ..default()
            };
            let camera = death_camera(&victim, &killer);
            assert_eq!(camera.translation, eye);
            let aim = (position + Vec3::Y * (PLAYER_EYE_HEIGHT * 0.5) - eye).normalize();
            assert!(camera.forward().dot(aim) > 0.99999);
        }
    }
}
