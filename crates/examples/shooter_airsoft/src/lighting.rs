//! Static diffuse lighting comes from Cycles. Only nearby lamps and the sun
//! render live shadows for moving players, guns, and specular highlights.
use bevy::{
    gltf::GltfExtras,
    light::{CascadeShadowConfigBuilder, DirectionalLightShadowMap},
    pbr::Lightmap,
    prelude::*,
};

pub struct ArenaLightingPlugin;

#[derive(Resource)]
pub struct LiveLampBudget(pub usize);

impl Default for LiveLampBudget {
    fn default() -> Self {
        Self(2)
    }
}

impl Plugin for ArenaLightingPlugin {
    fn build(&self, app: &mut App) {
        // Bevy shares this resolution between spotlights and sun cascades.
        app.insert_resource(DirectionalLightShadowMap { size: 1024 })
            .init_resource::<LiveLampBudget>()
            .add_systems(Update, (attach_lightmaps, configure_lights))
            .add_systems(
                PostUpdate,
                select_live_lamps
                    .after(TransformSystems::Propagate)
                    .before(bevy::camera::visibility::VisibilitySystems::VisibilityPropagate),
            );
    }
}

#[derive(serde::Deserialize)]
struct Bake {
    image: String,
    exposure: f32,
}

#[derive(serde::Deserialize)]
struct Extras {
    airsoft_lightmap: Option<Bake>,
}

fn attach_lightmaps(
    mut commands: Commands,
    assets: Res<AssetServer>,
    meshes: Res<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    extras: Query<&GltfExtras>,
    parents: Query<&ChildOf>,
    receivers: Query<(Entity, &Mesh3d, &MeshMaterial3d<StandardMaterial>), Without<Lightmap>>,
) {
    for (entity, mesh, material) in &receivers {
        let Some(bake) = std::iter::once(entity)
            .chain(parents.iter_ancestors(entity))
            .filter_map(|ancestor| extras.get(ancestor).ok())
            .filter_map(|extras| serde_json::from_str::<Extras>(&extras.value).ok())
            .find_map(|extras| extras.airsoft_lightmap)
        else {
            continue;
        };
        let (Some(mesh), Some(mut material)) = (meshes.get(mesh), materials.get_mut(material))
        else {
            continue;
        };
        assert!(
            mesh.contains_attribute(Mesh::ATTRIBUTE_UV_1),
            "baked mesh lacks UV1"
        );
        // One atlas/exposure for the arena; shared source materials are safe.
        material.lightmap_exposure = bake.exposure;
        commands.entity(entity).insert(Lightmap {
            image: assets.load(bake.image),
            uv_rect: Rect::new(0.0, 0.0, 1.0, 1.0),
            bicubic_sampling: false,
        });
    }
}

fn configure_lights(
    mut commands: Commands,
    mut lamps: Query<&mut SpotLight, Added<SpotLight>>,
    mut suns: Query<(Entity, &mut DirectionalLight), Added<DirectionalLight>>,
) {
    for mut lamp in &mut lamps {
        lamp.shadow_maps_enabled = true;
        lamp.affects_lightmapped_mesh_diffuse = false;
        lamp.radius = 0.35;
        lamp.range = 18.0;
    }
    for (entity, mut sun) in &mut suns {
        sun.shadow_maps_enabled = true;
        sun.affects_lightmapped_mesh_diffuse = false;
        commands.entity(entity).insert(
            CascadeShadowConfigBuilder {
                num_cascades: 2,
                maximum_distance: 45.0,
                first_cascade_far_bound: 12.0,
                ..default()
            }
            .build(),
        );
    }
}

fn select_live_lamps(
    budget: Res<LiveLampBudget>,
    players: Query<&GlobalTransform, With<crate::client::PlayerCapsule>>,
    cameras: Query<&GlobalTransform, With<Camera3d>>,
    mut lamps: Query<(Entity, &GlobalTransform, &mut Visibility), With<SpotLight>>,
) {
    // Each player gets their closest lamp. In the menu, use the preview camera.
    // All eight lamps remain in the bake, regardless of this runtime selection.
    let targets: Vec<_> = if players.is_empty() {
        cameras.iter().map(GlobalTransform::translation).collect()
    } else {
        players.iter().map(GlobalTransform::translation).collect()
    };
    let mut selected = Vec::with_capacity(2);
    let all_lamps_fit = lamps.iter().count() <= budget.0;
    for target in targets.iter().take(budget.0) {
        if let Some((entity, _, _)) = lamps.iter().min_by(|a, b| {
            a.1.translation()
                .distance_squared(*target)
                .total_cmp(&b.1.translation().distance_squared(*target))
        }) {
            selected.push(entity);
        }
    }
    for (entity, _, mut visibility) in &mut lamps {
        let desired = if all_lamps_fit || selected.contains(&entity) {
            Visibility::Inherited
        } else {
            Visibility::Hidden
        };
        if *visibility != desired {
            *visibility = desired;
        }
    }
}
