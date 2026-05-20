use std::collections::HashMap;

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
#[smooth(decay = 0.9, max_offset = 200.0, min_offset_sq = 4.0)]
pub struct PongGameState {
    #[smooth]
    #[wincode(with = "PodVec2")]
    pub ball_pos: Vec2,
    #[wincode(with = "PodVec2")]
    pub ball_vel: Vec2,
    #[smooth]
    pub paddle_left_y: f32,
    #[smooth]
    pub paddle_right_y: f32,
    pub score_left: u32,
    pub score_right: u32,
}

impl PongGameState {
    pub fn reset_ball(&mut self, direction: f32) {
        self.ball_pos = Vec2::new(FIELD_W / 2.0, FIELD_H / 2.0);
        self.ball_vel =
            Vec2::new(direction * BALL_SPEED, 0.3 * BALL_SPEED).normalize() * BALL_SPEED;
    }
}

impl DeformGameState for PongGameState {}

impl MaxLen for PongGameState {
    fn max_len() -> DeformResult<usize> {
        Ok(size_of::<Self>())
    }
}

#[derive(Default, Clone, Eq, PartialEq, serde::Serialize, SchemaRead, SchemaWrite)]
pub struct PongInputs {
    /// -100 to +100
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

        // First frame: initialize positions
        if new.ball_vel == Vec2::ZERO {
            new.paddle_left_y = FIELD_H / 2.0;
            new.paddle_right_y = FIELD_H / 2.0;
            new.reset_ball(1.0);
            return Ok(new);
        }

        // Sort players by pubkey for deterministic left/right assignment
        let mut players: Vec<_> = inputs.keys().collect();
        players.sort();

        if let Some(input) = players.first().and_then(|k| inputs.get(k)) {
            new.paddle_left_y += (input.direction as f32 / 100.0) * PADDLE_SPEED;
        }
        if let Some(input) = players.get(1).and_then(|k| inputs.get(k)) {
            new.paddle_right_y += (input.direction as f32 / 100.0) * PADDLE_SPEED;
        }

        new.paddle_left_y = new
            .paddle_left_y
            .clamp(PADDLE_HALF_H, FIELD_H - PADDLE_HALF_H);
        new.paddle_right_y = new
            .paddle_right_y
            .clamp(PADDLE_HALF_H, FIELD_H - PADDLE_HALF_H);

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

        // Left paddle collision
        let left_x = PADDLE_MARGIN;
        if new.ball_vel.x < 0.0
            && new.ball_pos.x - BALL_HALF <= left_x + PADDLE_HALF_W
            && new.ball_pos.x + BALL_HALF >= left_x - PADDLE_HALF_W
            && new.ball_pos.y + BALL_HALF >= new.paddle_left_y - PADDLE_HALF_H
            && new.ball_pos.y - BALL_HALF <= new.paddle_left_y + PADDLE_HALF_H
        {
            new.ball_pos.x = left_x + PADDLE_HALF_W + BALL_HALF;
            let hit_offset = (new.ball_pos.y - new.paddle_left_y) / PADDLE_HALF_H;
            new.ball_vel = Vec2::new(1.0, hit_offset).normalize() * BALL_SPEED;
        }

        // Right paddle collision
        let right_x = FIELD_W - PADDLE_MARGIN;
        if new.ball_vel.x > 0.0
            && new.ball_pos.x + BALL_HALF >= right_x - PADDLE_HALF_W
            && new.ball_pos.x - BALL_HALF <= right_x + PADDLE_HALF_W
            && new.ball_pos.y + BALL_HALF >= new.paddle_right_y - PADDLE_HALF_H
            && new.ball_pos.y - BALL_HALF <= new.paddle_right_y + PADDLE_HALF_H
        {
            new.ball_pos.x = right_x - PADDLE_HALF_W - BALL_HALF;
            let hit_offset = (new.ball_pos.y - new.paddle_right_y) / PADDLE_HALF_H;
            new.ball_vel = Vec2::new(-1.0, hit_offset).normalize() * BALL_SPEED;
        }

        // Goals
        if new.ball_pos.x - BALL_HALF <= 0.0 {
            new.score_right += 1;
            new.reset_ball(-1.0);
        } else if new.ball_pos.x + BALL_HALF >= FIELD_W {
            new.score_left += 1;
            new.reset_ball(1.0);
        }

        Ok(new)
    }
}

fn main() {}
