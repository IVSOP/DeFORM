//! Opt-in comparison of render costs. Never runs in normal play.
use std::collections::BTreeMap;

use bevy::{diagnostic::DiagnosticsStore, prelude::*, window::PresentMode};

#[derive(Default, Resource)]
pub struct PerfProbe {
    stage: usize,
    start: Option<f64>,
    samples: Vec<f64>,
    gpu: BTreeMap<String, (f64, usize)>,
}

const STAGES: &[&str] = &["baked-msaa", "baked-all-lights-2048"];

#[allow(clippy::too_many_arguments)]
pub fn sample(
    mut commands: Commands,
    time: Res<Time>,
    mut probe: ResMut<PerfProbe>,
    diagnostics: Res<DiagnosticsStore>,
    mut windows: Query<&mut Window>,
    mut budget: ResMut<crate::lighting::LiveLampBudget>,
    mut shadow_map: ResMut<bevy::light::DirectionalLightShadowMap>,
    lightmaps: Query<&bevy::pbr::Lightmap>,
    assets: Res<AssetServer>,
    scene: Res<crate::client::SceneAssets>,
    mut exit: MessageWriter<AppExit>,
) {
    assert!(
        time.elapsed_secs_f64() < 90.0,
        "render performance probe timed out"
    );
    if probe.stage == STAGES.len() {
        if time.elapsed_secs_f64() - probe.start.unwrap() > 1.0 {
            exit.write(AppExit::Success);
        }
        return;
    }
    if !assets.is_loaded_with_dependencies(scene.level.id()) {
        return;
    }
    if lightmaps.iter().count() < 700
        || lightmaps
            .iter()
            .any(|bake| !assets.is_loaded_with_dependencies(bake.image.id()))
    {
        return;
    }
    if probe.start.is_none() {
        info!("PERF requests AutoNoVsync; compositor pacing may still limit measured FPS");
        for mut window in &mut windows {
            window.present_mode = PresentMode::AutoNoVsync;
        }
    }
    let start = *probe.start.get_or_insert(time.elapsed_secs_f64());
    let elapsed = time.elapsed_secs_f64() - start;
    if elapsed < 4.0 {
        return;
    } // allow compilation/pipelines/frame pacing to settle
    probe.samples.push(time.delta_secs_f64() * 1000.0);
    for diagnostic in diagnostics.iter() {
        let path = diagnostic.path().as_str();
        if path.ends_with("elapsed_gpu")
            && let Some(value) = diagnostic.value()
        {
            let sum = probe.gpu.entry(path.to_owned()).or_default();
            sum.0 += value;
            sum.1 += 1;
        }
    }
    if elapsed < 10.0 {
        return;
    }
    let total = probe.samples.iter().sum::<f64>();
    probe.samples.sort_by(f64::total_cmp);
    let count = probe.samples.len();
    let window = windows.iter().next().unwrap();
    info!(
        "PERF {}: {:.1} FPS, {:.2} ms mean, {:.2} ms p95; {}x{} physical pixels",
        STAGES[probe.stage],
        count as f64 * 1000.0 / total,
        total / count as f64,
        probe.samples[count * 95 / 100],
        window.physical_width(),
        window.physical_height()
    );
    let mut gpu: Vec<_> = probe
        .gpu
        .iter()
        .map(|(path, (total, count))| (path, total / *count as f64))
        .collect();
    gpu.sort_by(|a, b| b.1.total_cmp(&a.1));
    for (path, ms) in gpu.into_iter().take(8) {
        info!("PERF GPU {path}: {ms:.3} ms");
    }
    commands
        .spawn(bevy::render::view::screenshot::Screenshot::primary_window())
        .observe(bevy::render::view::screenshot::save_to_disk(format!(
            "/tmp/airsoft-perf-{}.png",
            STAGES[probe.stage]
        )));
    probe.stage += 1;
    if probe.stage == STAGES.len() {
        probe.start = Some(time.elapsed_secs_f64());
        return;
    }
    // Isolate the live shadow budget; both stages keep the same bake and MSAA.
    budget.0 = 8;
    shadow_map.size = 2048;
    probe.samples.clear();
    probe.gpu.clear();
    probe.start = Some(time.elapsed_secs_f64());
}
