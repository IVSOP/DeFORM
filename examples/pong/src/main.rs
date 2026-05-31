use std::collections::{HashMap, HashSet};

use anyhow::anyhow;
use bevy::prelude::*;
use deform_core::{
    DeformClient, DeformGameState, DeformInputs, DeformResult, DeformUserLogic, MaxLen, Pubkey,
    Smooth,
};
use deform_offline::new_offline_client;
use wincode::{SchemaRead, SchemaWrite};

wincode::pod_wrapper! {
    unsafe struct PodVec2(Vec2);
}

pub const FIELD_W: f32 = 1000.0;
pub const FIELD_H: f32 = 1000.0;
pub const PADDLE_W: f32 = 20.0;
pub const PADDLE_H: f32 = 120.0;
pub const PADDLE_HALF_W: f32 = PADDLE_W / 2.0;
pub const PADDLE_HALF_H: f32 = PADDLE_H / 2.0;
pub const PADDLE_X: f32 = 400.0;
pub const PADDLE_SPEED: f32 = 8.0;
pub const BALL_SIZE: f32 = 20.0;
pub const BALL_HALF: f32 = BALL_SIZE / 2.0;
pub const BALL_SPEED: f32 = 7.5;

#[derive(Default, Clone, serde::Serialize, SchemaRead, SchemaWrite, Smooth)]
pub struct PlayerState {
    #[smooth]
    pub paddle_y: f32,
    pub score: u32,
}

impl MaxLen for PlayerState {
    fn max_len() -> DeformResult<usize> {
        Ok(size_of::<Self>())
    }
}

#[derive(Default, Clone, serde::Serialize, SchemaRead, SchemaWrite, Smooth)]
#[smooth(decay = 0.9, max_offset = 200.0, min_offset_sq = 4.0)]
pub struct PongGameState {
    #[smooth]
    #[wincode(with = "PodVec2")]
    pub ball_pos: Vec2,
    #[wincode(with = "PodVec2")]
    pub ball_vel: Vec2,
    #[smooth(map)]
    pub players: HashMap<Pubkey, PlayerState>,
}

impl PongGameState {
    pub fn reset_ball(&mut self, direction: f32) {
        self.ball_pos = Vec2::ZERO;
        self.ball_vel =
            Vec2::new(direction * BALL_SPEED, 0.3 * BALL_SPEED).normalize() * BALL_SPEED;
    }
}

impl DeformGameState for PongGameState {
    fn new(players: &HashSet<Pubkey>) -> Self {
        let mut state = Self::new_empty();
        for player in players {
            state.add_player(player.clone());
        }
        state
    }

    fn new_empty() -> Self {
        Self {
            ball_pos: Vec2::ZERO,
            ball_vel: Vec2::ZERO,
            players: HashMap::new(),
        }
    }

    fn add_player(&mut self, player: Pubkey) {
        self.players.entry(player).or_insert(PlayerState {
            paddle_y: 0.0,
            score: 0,
        });
    }
}

impl MaxLen for PongGameState {
    fn max_len() -> DeformResult<usize> {
        Ok(size_of::<Vec2>() * 2 + 4 + 2 * (size_of::<Pubkey>() + size_of::<PlayerState>()))
    }
}

#[derive(Default, Clone, Eq, PartialEq, serde::Serialize, SchemaRead, SchemaWrite, Component)]
pub struct PongInputs {
    pub direction: i8,
}

impl DeformInputs for PongInputs {}

impl MaxLen for PongInputs {
    fn max_len() -> DeformResult<usize> {
        Ok(size_of::<Self>())
    }
}

#[derive(Clone, Default)]
pub struct PongGame;

impl DeformUserLogic for PongGame {
    type Inputs = PongInputs;
    type GameState = PongGameState;
    type Smoother = PongGameStateSmoother;
    type Error = std::convert::Infallible;

    fn advance_frame(
        &mut self,
        state: &Self::GameState,
        inputs: &HashMap<Pubkey, Self::Inputs>,
    ) -> Result<Self::GameState, Self::Error> {
        let mut new = state.clone();

        if new.ball_vel == Vec2::ZERO {
            new.reset_ball(1.0);
            return Ok(new);
        }

        // Sort players by pubkey for deterministic left/right assignment
        let mut sorted: Vec<_> = inputs.keys().collect();
        sorted.sort();

        let left_pk = sorted.first().copied();
        let right_pk = sorted.get(1).copied();

        // Apply inputs
        for pk in [left_pk, right_pk].into_iter().flatten() {
            if let Some(input) = inputs.get(pk) {
                if let Some(ps) = new.players.get_mut(pk) {
                    ps.paddle_y += (input.direction as f32 / 100.0) * PADDLE_SPEED;
                    ps.paddle_y = ps.paddle_y.clamp(
                        -FIELD_H / 2.0 + PADDLE_HALF_H,
                        FIELD_H / 2.0 - PADDLE_HALF_H,
                    );
                }
            }
        }

        // Move ball
        new.ball_pos += new.ball_vel;

        // Bounce off top/bottom walls
        if new.ball_pos.y - BALL_HALF <= -FIELD_H / 2.0 {
            new.ball_pos.y = -FIELD_H / 2.0 + BALL_HALF;
            new.ball_vel.y = new.ball_vel.y.abs();
        } else if new.ball_pos.y + BALL_HALF >= FIELD_H / 2.0 {
            new.ball_pos.y = FIELD_H / 2.0 - BALL_HALF;
            new.ball_vel.y = -new.ball_vel.y.abs();
        }

        let left_paddle_y = left_pk
            .and_then(|pk| new.players.get(pk))
            .map(|ps| ps.paddle_y)
            .unwrap_or(0.0);
        let right_paddle_y = right_pk
            .and_then(|pk| new.players.get(pk))
            .map(|ps| ps.paddle_y)
            .unwrap_or(0.0);

        // Left paddle collision
        let left_x = -PADDLE_X;
        if new.ball_vel.x < 0.0
            && new.ball_pos.x - BALL_HALF <= left_x + PADDLE_HALF_W
            && new.ball_pos.x + BALL_HALF >= left_x - PADDLE_HALF_W
            && new.ball_pos.y + BALL_HALF >= left_paddle_y - PADDLE_HALF_H
            && new.ball_pos.y - BALL_HALF <= left_paddle_y + PADDLE_HALF_H
        {
            new.ball_pos.x = left_x + PADDLE_HALF_W + BALL_HALF;
            let hit_offset = (new.ball_pos.y - left_paddle_y) / PADDLE_HALF_H;
            new.ball_vel = Vec2::new(1.0, hit_offset).normalize() * BALL_SPEED;
        }

        // Right paddle collision
        let right_x = PADDLE_X;
        if new.ball_vel.x > 0.0
            && new.ball_pos.x + BALL_HALF >= right_x - PADDLE_HALF_W
            && new.ball_pos.x - BALL_HALF <= right_x + PADDLE_HALF_W
            && new.ball_pos.y + BALL_HALF >= right_paddle_y - PADDLE_HALF_H
            && new.ball_pos.y - BALL_HALF <= right_paddle_y + PADDLE_HALF_H
        {
            new.ball_pos.x = right_x - PADDLE_HALF_W - BALL_HALF;
            let hit_offset = (new.ball_pos.y - right_paddle_y) / PADDLE_HALF_H;
            new.ball_vel = Vec2::new(-1.0, hit_offset).normalize() * BALL_SPEED;
        }

        // Goals
        if new.ball_pos.x - BALL_HALF <= -FIELD_W / 2.0 {
            if let Some(pk) = right_pk {
                if let Some(ps) = new.players.get_mut(pk) {
                    ps.score += 1;
                }
            }
            new.reset_ball(-1.0);
        } else if new.ball_pos.x + BALL_HALF >= FIELD_W / 2.0 {
            if let Some(pk) = left_pk {
                if let Some(ps) = new.players.get_mut(pk) {
                    ps.score += 1;
                }
            }
            new.reset_ball(1.0);
        }

        Ok(new)
    }
}

fn main() {
    let mut app = App::new();
    app.add_plugins((DefaultPlugins,))
        .add_systems(Startup, setup_offline);
    // TODO: figure out the best order
    app.add_systems(Update, (update_inputs, send_inputs).chain());
    app.add_systems(PostUpdate, update_state);
    app.run();
}

#[derive(Component)]
pub struct Ball;

#[derive(Component)]
#[repr(transparent)]
pub struct Player(Pubkey);

// It is hard to keep track of which player is which entity, so this structure will help
#[derive(Resource)]
#[repr(transparent)]
pub struct PlayerEntities(HashMap<Pubkey, Entity>);

fn pong_bot() -> impl Fn(&PongGameState, &Pubkey) -> PongInputs + Send + Sync {
    let prev_direction = std::sync::Mutex::new(0i8);

    move |state: &PongGameState, bot: &Pubkey| -> PongInputs {
        let prev = *prev_direction.lock().unwrap();

        if state.ball_vel.x <= 0.0 {
            *prev_direction.lock().unwrap() = 0;
            return PongInputs::default();
        }

        let paddle_y = state.players.get(bot).map(|p| p.paddle_y).unwrap_or(0.0);
        let diff = state.ball_pos.y - paddle_y;

        // Larger dead zone to release a held direction, smaller to start moving
        let threshold = if (prev > 0 && diff > 0.0) || (prev < 0 && diff < 0.0) {
            PADDLE_SPEED * 0.25
        } else {
            PADDLE_SPEED * 1.5
        };

        let direction = if diff.abs() < threshold {
            0
        } else if diff > 0.0 {
            100
        } else {
            -100
        };

        *prev_direction.lock().unwrap() = direction;
        PongInputs { direction }
    }
}

/// A wrapper struct is made for the multiplayer client. This makes it so that I don't have to have a bevy feature and/or dependency, and that would mean every time bevy updates, it would most likely be broken.
#[derive(Resource)]
#[repr(transparent)]
pub struct MultiplayerClient(DeformClient<PongGame>);

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
    let client = new_offline_client::<PongGame>(main_player_pubkey.clone(), players, pong_bot())?;
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
