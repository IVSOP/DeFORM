//! Opt-in deterministic rendering fixture. Uses the real simulation and renderer,
//! with a local channel in place of a backend thread so capture timing is repeatable.
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

use bevy::prelude::*;
use deform_core::{
    ChannelInputs, DeformClient, DeformSharedBackendState, DeformUserLogic, Pubkey, TickInfo,
    accounts::lobby::{LobbyState, ongoing::LobbyOngoing},
};
use shooter_airsoft::shooter_logic::*;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::client::{LocalPlayer, MultiplayerClient, SceneAssets};

#[derive(Resource)]
pub struct RoundSmoke {
    receiver: mpsc::UnboundedReceiver<ChannelInputs<ShooterGame>>,
    tick: u16,
    game: ShooterGame,
    victim: Pubkey,
    killer: Pubkey,
}

pub fn start(commands: &mut Commands) {
    let victim = Pubkey::new_from_array([1; 32]);
    let killer = Pubkey::new_from_array([255; 32]);
    let mut lobby = crate::client::make_backend_lobby(victim, killer);
    let LobbyState::NotStarted(not_started) = &lobby.state else {
        unreachable!()
    };
    let mut state = ShooterGame::new_game_from_lobby(&lobby.metadata, not_started).unwrap();
    state.phase = RoundPhase::Playing;
    state.players.get_mut(&victim).unwrap().pos = Vec3::new(0.0, PLAYER_FLOAT_HEIGHT, 14.4);
    state.players.get_mut(&victim).unwrap().vel = Vec3::X * 1.5;
    state.players.get_mut(&killer).unwrap().pos = Vec3::new(3.0, PLAYER_FLOAT_HEIGHT, 14.4);
    let mut game = ShooterGame::default();
    let mut shot = ShooterInputs {
        fire: true,
        ..default()
    };
    shot.set_look(std::f32::consts::FRAC_PI_2, 0.0);
    state = game
        .advance_frame(&state, &BTreeMap::from([(killer, shot)]))
        .unwrap();
    assert!(state.players[&victim].death.is_some());
    lobby.state = LobbyState::Ongoing(LobbyOngoing {
        slot: None,
        tick: 1,
        tick_info: TickInfo {
            game_state: state,
            inputs: BTreeMap::new(),
        },
        user_logic: ShooterGame::default(),
    });
    let (sender, receiver) = mpsc::unbounded_channel();
    let shared = DeformSharedBackendState::new_from_lobby(lobby).unwrap();
    commands.insert_resource(MultiplayerClient(DeformClient::new(
        sender,
        Arc::new(Mutex::new(shared)),
        CancellationToken::new(),
    )));
    commands.insert_resource(LocalPlayer(victim));
    commands.insert_resource(RoundSmoke {
        receiver,
        tick: 0,
        game,
        victim,
        killer,
    });
}

#[allow(clippy::too_many_arguments)]
pub fn advance(
    fixture: Option<ResMut<RoundSmoke>>,
    client: Option<Res<MultiplayerClient>>,
    scene: Res<SceneAssets>,
    assets: Res<AssetServer>,
    meshes: Query<&Mesh3d>,
    lightmaps: Query<&bevy::pbr::Lightmap>,
    time: Res<Time>,
    mut commands: Commands,
    mut exit: MessageWriter<AppExit>,
) {
    let (Some(mut fixture), Some(client)) = (fixture, client) else {
        return;
    };
    assert!(time.elapsed_secs() < 90.0, "round render smoke timed out");
    while fixture.receiver.try_recv().is_ok() {}
    if !assets.is_loaded_with_dependencies(scene.level.id()) || meshes.iter().count() < 500 {
        return;
    }
    if lightmaps.iter().count() < 700
        || lightmaps
            .iter()
            .any(|bake| !assets.is_loaded_with_dependencies(bake.image.id()))
    {
        return;
    }
    let mut shared = client.0.read_state().unwrap();
    let LobbyState::Ongoing(ongoing) = &mut shared.lobby.state else {
        unreachable!()
    };
    fixture.tick += 1;
    let tick = fixture.tick;
    let killer = fixture.killer;
    if tick == 90 {
        // Put the tracked enemy beyond solid plywood for the occlusion capture.
        ongoing
            .tick_info
            .game_state
            .players
            .get_mut(&killer)
            .unwrap()
            .pos = Vec3::new(3.0, PLAYER_FLOAT_HEIGHT, 10.5);
    }
    let inputs = BTreeMap::from([(killer, ShooterInputs::default())]);
    ongoing.tick_info.game_state = fixture
        .game
        .advance_frame(&ongoing.tick_info.game_state, &inputs)
        .unwrap();
    if tick == 120 {
        let state = &ongoing.tick_info.game_state;
        let eye = state.players[&fixture.victim]
            .death
            .as_ref()
            .unwrap()
            .camera_position;
        assert!(
            !shooter_airsoft::navigation::visible(eye, state.players[&killer].pos),
            "outline fixture must actually be behind a wall"
        );
    }
    let capture = match tick {
        65 => Some("/tmp/airsoft-death-camera.png"),
        140 => Some("/tmp/airsoft-killer-through-wall.png"),
        330 => Some("/tmp/airsoft-respawn-countdown.png"),
        _ => None,
    };
    if let Some(path) = capture {
        commands
            .spawn(bevy::render::view::screenshot::Screenshot::primary_window())
            .observe(bevy::render::view::screenshot::save_to_disk(path));
    }
    if tick > 500 {
        assert_eq!(ongoing.tick_info.game_state.phase, RoundPhase::Playing);
        info!("ROUND SMOKE: death camera, occluded killer outline, respawn and countdown rendered");
        exit.write(AppExit::Success);
    }
}

/// Extra test view of the corpse after verifying the real death camera.
pub fn preview_corpse(
    fixture: Option<Res<RoundSmoke>>,
    bodies: Query<(&crate::client::PlayerCapsule, &Transform), Without<Camera3d>>,
    mut cameras: Query<&mut Transform, With<Camera3d>>,
    mut commands: Commands,
) {
    let Some(fixture) = fixture else {
        return;
    };
    if !(190..=235).contains(&fixture.tick) {
        return;
    }
    let (_, body) = bodies
        .iter()
        .find(|(player, _)| player.0 == fixture.victim)
        .unwrap();
    for mut camera in &mut cameras {
        *camera = Transform::from_translation(body.translation + Vec3::new(1.3, 1.5, -1.8))
            .looking_at(body.translation, Vec3::Y);
    }
    if fixture.tick == 220 {
        commands
            .spawn(bevy::render::view::screenshot::Screenshot::primary_window())
            .observe(bevy::render::view::screenshot::save_to_disk(
                "/tmp/airsoft-corpse.png",
            ));
    }
}
