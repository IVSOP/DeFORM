use std::collections::{HashMap, HashSet};

use bevy::math::Vec2;
use deform_core::{
    DeformGameState, DeformInputs, DeformResult, DeformUserLogic, MaxLen, Pubkey, Smooth,
};
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
pub const PADDLE_MARGIN: f32 = 30.0;
pub const PADDLE_SPEED: f32 = 8.0;
pub const BALL_SIZE: f32 = 20.0;
pub const BALL_HALF: f32 = BALL_SIZE / 2.0;
pub const BALL_SPEED: f32 = 10.0;

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
        self.ball_pos = Vec2::new(FIELD_W / 2.0, FIELD_H / 2.0);
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
            ball_pos: Vec2::new(FIELD_W / 2.0, FIELD_H / 2.0),
            ball_vel: Vec2::ZERO,
            players: HashMap::new(),
        }
    }

    fn add_player(&mut self, player: Pubkey) {
        self.players.entry(player).or_insert(PlayerState {
            paddle_y: FIELD_H / 2.0,
            score: 0,
        });
    }
}

impl MaxLen for PongGameState {
    fn max_len() -> DeformResult<usize> {
        Ok(size_of::<Vec2>() * 2 + 4 + 2 * (size_of::<Pubkey>() + size_of::<PlayerState>()))
    }
}

#[derive(Default, Clone, Eq, PartialEq, serde::Serialize, SchemaRead, SchemaWrite)]
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
                    ps.paddle_y = ps.paddle_y.clamp(PADDLE_HALF_H, FIELD_H - PADDLE_HALF_H);
                }
            }
        }

        // Move ball
        new.ball_pos += new.ball_vel;

        // Bounce off top/bottom walls
        if new.ball_pos.y - BALL_HALF <= 0.0 {
            new.ball_pos.y = BALL_HALF;
            new.ball_vel.y = new.ball_vel.y.abs();
        } else if new.ball_pos.y + BALL_HALF >= FIELD_H {
            new.ball_pos.y = FIELD_H - BALL_HALF;
            new.ball_vel.y = -new.ball_vel.y.abs();
        }

        let left_paddle_y = left_pk
            .and_then(|pk| new.players.get(pk))
            .map(|ps| ps.paddle_y)
            .unwrap_or(FIELD_H / 2.0);
        let right_paddle_y = right_pk
            .and_then(|pk| new.players.get(pk))
            .map(|ps| ps.paddle_y)
            .unwrap_or(FIELD_H / 2.0);

        // Left paddle collision
        let left_x = PADDLE_MARGIN;
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
        let right_x = FIELD_W - PADDLE_MARGIN;
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
        if new.ball_pos.x - BALL_HALF <= 0.0 {
            if let Some(pk) = right_pk {
                if let Some(ps) = new.players.get_mut(pk) {
                    ps.score += 1;
                }
            }
            new.reset_ball(-1.0);
        } else if new.ball_pos.x + BALL_HALF >= FIELD_W {
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

fn main() {}
