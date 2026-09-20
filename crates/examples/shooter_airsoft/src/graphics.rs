//! Live rendering preferences, shared by the lobby and in-game menus.
use bevy::{post_process::bloom::Bloom, prelude::*, window::PresentMode};
use bevy_egui::egui;

#[derive(Resource, Clone, Copy, PartialEq)]
pub struct GraphicsSettings {
    pub msaa: bool,
    pub bloom: bool,
    pub vsync: bool,
}

impl Default for GraphicsSettings {
    fn default() -> Self {
        Self {
            msaa: true,
            bloom: true,
            vsync: true,
        }
    }
}

pub struct GraphicsPlugin;
impl Plugin for GraphicsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<GraphicsSettings>().add_systems(
            Update,
            apply_settings.run_if(resource_changed::<GraphicsSettings>),
        );
    }
}

pub fn controls(ui: &mut egui::Ui, settings: &mut ResMut<GraphicsSettings>) {
    let mut next = **settings;
    ui.collapsing("Graphics", |ui| {
        ui.checkbox(&mut next.msaa, "MSAA (4×)");
        ui.checkbox(&mut next.bloom, "Bloom");
        ui.checkbox(&mut next.vsync, "VSync")
            .on_hover_text("Your desktop compositor may still limit presentation when disabled.");
    });
    if next != **settings {
        **settings = next;
    }
}

fn apply_settings(
    mut commands: Commands,
    settings: Res<GraphicsSettings>,
    cameras: Query<Entity, With<Camera3d>>,
    mut windows: Query<&mut Window>,
) {
    for camera in &cameras {
        let mut entity = commands.entity(camera);
        entity.insert(if settings.msaa {
            Msaa::Sample4
        } else {
            Msaa::Off
        });
        if settings.bloom {
            entity.insert(Bloom::NATURAL);
        } else {
            entity.remove::<Bloom>();
        }
    }
    for mut window in &mut windows {
        window.present_mode = if settings.vsync {
            PresentMode::AutoVsync
        } else {
            PresentMode::AutoNoVsync
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_toggle_camera_and_presentation_in_both_directions() {
        let mut app = App::new();
        app.add_plugins(GraphicsPlugin);
        let camera = app.world_mut().spawn(Camera3d::default()).id();
        let window = app.world_mut().spawn(Window::default()).id();
        app.update();
        for enabled in [false, true] {
            *app.world_mut().resource_mut::<GraphicsSettings>() = GraphicsSettings {
                msaa: enabled,
                bloom: enabled,
                vsync: enabled,
            };
            app.update();
            let world = app.world();
            assert_eq!(
                world.get::<Msaa>(camera),
                Some(&if enabled { Msaa::Sample4 } else { Msaa::Off })
            );
            assert_eq!(world.get::<Bloom>(camera).is_some(), enabled);
            assert_eq!(
                world.get::<Window>(window).unwrap().present_mode,
                if enabled {
                    PresentMode::AutoVsync
                } else {
                    PresentMode::AutoNoVsync
                }
            );
        }
    }
}
