use std::collections::{HashMap, HashSet};

use anyhow::anyhow;
use bevy::prelude::*;
use deform_core::{DeformClient, Pubkey};
use deform_offline::new_offline_client;

mod pong_logic;

fn main() {
    let mut app = App::new();
    app.add_plugins((DefaultPlugins,))
        .add_systems(Startup, setup_offline);
    // TODO: figure out the best order
    app.add_systems(Update, (update_inputs, send_inputs).chain());
    app.add_systems(PostUpdate, update_state);
    app.run();
}

use pong_logic::*;

#[derive(Component)]
pub struct Ball;

#[derive(Component)]
#[repr(transparent)]
pub struct Player(Pubkey);

/// A wrapper struct is made for the multiplayer client. This makes it so that I don't have to have a bevy feature and/or dependency, and that would mean every time bevy updates, it would most likely be broken.
#[derive(Resource)]
#[repr(transparent)]
pub struct MultiplayerClient(DeformClient<PongGame>);

// It is hard to keep track of which player is which entity, so this structure will help
#[derive(Resource)]
#[repr(transparent)]
pub struct PlayerEntities(HashMap<Pubkey, Entity>);

fn setup_offline(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<ColorMaterial>>,
) -> Result<()> {
    commands.spawn((
        Camera2d,
        Projection::Orthographic(OrthographicProjection {
            scaling_mode: bevy::camera::ScalingMode::AutoMin {
                min_width: FIELD_W,
                min_height: FIELD_H,
            },
            ..OrthographicProjection::default_2d()
        }),
    ));

    let border_thickness = 4.0;
    let border_material = materials.add(Color::LinearRgba(LinearRgba::WHITE));
    let h_border = meshes.add(Rectangle::new(FIELD_W + border_thickness, border_thickness));
    let v_border = meshes.add(Rectangle::new(border_thickness, FIELD_H + border_thickness));
    for (mesh, x, y) in [
        (h_border.clone(), 0.0, FIELD_H / 2.0),
        (h_border, 0.0, -FIELD_H / 2.0),
        (v_border.clone(), -FIELD_W / 2.0, 0.0),
        (v_border, FIELD_W / 2.0, 0.0),
    ] {
        commands.spawn((
            Mesh2d(mesh),
            MeshMaterial2d(border_material.clone()),
            Transform::from_translation(Vec3::new(x, y, 0.0)),
        ));
    }

    let ball_shape = meshes.add(Circle::new(BALL_HALF));
    let player_shape = meshes.add(Rectangle::new(PADDLE_W, PADDLE_H));

    let material = materials.add(Color::LinearRgba(LinearRgba::GREEN));

    commands.spawn((
        Ball,
        Mesh2d(ball_shape),
        MeshMaterial2d(material.clone()),
        Transform::default(),
    ));

    let main_player_pubkey = Pubkey::from_str_const("CLi1od28M3eAGH54s7jsvvJXZJjoe6ELyuabiPvcwUym");
    let bot_player_pubkey = Pubkey::new_from_array([255; 32]);

    let main_player_entity = commands
        .spawn((
            Player(main_player_pubkey.clone()),
            PongInputs { direction: 0 },
            Mesh2d(player_shape.clone()),
            MeshMaterial2d(material.clone()),
            Transform::from_translation(Vec3::new(-PADDLE_X, 0.0, 0.0)),
        ))
        .id();

    let bot_player_entity = commands
        .spawn((
            Player(bot_player_pubkey.clone()),
            Mesh2d(player_shape),
            MeshMaterial2d(material),
            Transform::from_translation(Vec3::new(PADDLE_X, 0.0, 0.0)),
        ))
        .id();

    let players = HashSet::from([main_player_pubkey.clone(), bot_player_pubkey.clone()]);
    let client = new_offline_client::<PongGame>(main_player_pubkey.clone(), players, pong_bot)?;
    commands.insert_resource(MultiplayerClient(client));

    let mut player_entities = HashMap::new();
    player_entities.insert(main_player_pubkey, main_player_entity);
    player_entities.insert(bot_player_pubkey, bot_player_entity);
    commands.insert_resource(PlayerEntities(player_entities));

    Ok(())
}

fn update_inputs(inputs: Single<&mut PongInputs>, kb_input: Res<ButtonInput<KeyCode>>) {
    let mut new_direction: i8 = 0;

    if kb_input.pressed(KeyCode::KeyW) {
        new_direction += 100;
    }

    if kb_input.pressed(KeyCode::KeyS) {
        new_direction -= 100;
    }

    inputs.into_inner().direction = new_direction;
}

fn send_inputs(client: ResMut<MultiplayerClient>, inputs: Single<&PongInputs>) -> Result<()> {
    let inputs: PongInputs = inputs.clone();
    client.0.set_inputs(inputs)?;

    Ok(())
}

fn update_state(
    client: Res<MultiplayerClient>,
    mut players: Query<&mut Transform, (Without<Ball>, With<Player>)>,
    ball: Single<&mut Transform, (Without<Player>, With<Ball>)>,
    player_entities: Res<PlayerEntities>,
) -> Result<()> {
    // TODO: try_lock instead of lock???
    let state = {
        let shared = client.0.read_state()?;

        shared.tick_info.game_state.clone()
    };

    for (player, new_player_state) in state.players.iter() {
        let entity = player_entities
            .0
            .get(player)
            .ok_or(anyhow!("Player not found!"))?;

        let mut player_transform = players.get_mut(*entity)?;
        player_transform.translation.y = new_player_state.paddle_y;
    }

    ball.into_inner().translation = state.ball_pos.extend(0.0);

    Ok(())
}
