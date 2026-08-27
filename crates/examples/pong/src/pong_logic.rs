use std::collections::{BTreeMap, HashMap};

use deform_core::{
    DeformGameState, DeformInputs, DeformUserLogic, Pubkey, Smooth,
    accounts::lobby::{LobbyMetadata, not_started::LobbyNotStarted},
};
use glam::Vec2;
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
pub const PADDLE_SPEED: f32 = 480.0;
pub const BALL_SIZE: f32 = 20.0;
pub const BALL_HALF: f32 = BALL_SIZE / 2.0;
pub const BALL_SPEED: f32 = 1050.0;
pub const BALL_SPAWN_X: f32 = PADDLE_X - 50.0;

#[derive(
    Default, Debug, Clone, serde::Serialize, serde::Deserialize, SchemaRead, SchemaWrite, Smooth,
)]
#[cfg_attr(
    feature = "anchor",
    derive(anchor_lang::AnchorSerialize, anchor_lang::AnchorDeserialize)
)]
pub struct PlayerState {
    #[smooth]
    pub paddle_y: f32,
    pub score: u32,
}

#[derive(
    Default, Debug, Clone, serde::Serialize, serde::Deserialize, SchemaRead, SchemaWrite, Smooth,
)]
// Per-tick correction is `offset * (1 - decay) + max_correction`: `decay` gives the fast
// start, `max_correction` floors the finish so it never crawls (200 units clears in 5 ticks
// at 60/48/38/31/23). Keep it under the paddle's own 24 units/tick or corrections read as
// jumps. `max_offset` sits above the ball's 52.5 units/tick and below `reset_round`'s ~850,
// so a round reset snaps instead of sweeping.
#[smooth(
    decay = 0.8,
    max_offset = 200.0,
    min_offset_sq = 4.0,
    max_correction = 20.0
)]
pub struct PongGameState {
    #[smooth]
    #[wincode(with = "PodVec2")]
    pub ball_pos: Vec2,
    #[wincode(with = "PodVec2")]
    pub ball_vel: Vec2,
    pub creator: Pubkey,
    #[smooth(map)]
    #[serde(serialize_with = "deform_core::pubkey_map::serialize")]
    pub players: HashMap<Pubkey, PlayerState>,
}

impl PongGameState {
    pub fn reset_round(&mut self, direction: f32) {
        // Serve from the scorer's side instead of the middle, straight at the
        // opponent so they have time to react
        self.ball_pos = Vec2::new(-direction * BALL_SPAWN_X, 0.0);
        self.ball_vel = Vec2::new(direction * BALL_SPEED, 0.0);
        // Both paddles back to the middle
        for ps in self.players.values_mut() {
            ps.paddle_y = 0.0;
        }
    }

    pub fn add_user(&mut self, pubkey: Pubkey) {
        self.players.insert(pubkey, PlayerState::default());
    }
}

impl DeformGameState for PongGameState {
    fn has_ended(&self) -> bool {
        self.players.values().any(|ps| ps.score >= 10)
    }
}

#[derive(
    Default,
    Clone,
    Debug,
    Eq,
    PartialEq,
    serde::Serialize,
    serde::Deserialize,
    SchemaRead,
    SchemaWrite,
)]
#[cfg_attr(
    feature = "anchor",
    derive(anchor_lang::AnchorSerialize, anchor_lang::AnchorDeserialize)
)]
#[cfg_attr(feature = "client", derive(bevy::prelude::Component))]
pub struct PongInputs {
    pub direction: i8,
}

// Deliberately keeps the default `predict` (repeat the last input verbatim)
impl DeformInputs for PongInputs {}

#[derive(Debug, Clone, SchemaRead, SchemaWrite, serde::Serialize, serde::Deserialize)]
pub struct PongGame;

#[derive(Debug, Clone, serde::Serialize, SchemaRead, SchemaWrite, thiserror::Error)]
pub enum PongError {
    // needed otherwise SchemaRead will warn about unreachable code
    #[error("unreachable")]
    Never,
    #[error("Lobby should be started")]
    LobbyNotStarted,
    #[error("Error serializing inputs: {0}")]
    SerializeInputs(String),
    #[error("Error scheduling crank: {0}")]
    ScheduleCrank(String),
}

// the ephemeral validators run at 20Hz
#[cfg(feature = "20hz")]
pub const TICK_RATE_MICROS: u64 = 50000;
#[cfg(feature = "60hz")]
pub const TICK_RATE_MICROS: u64 = 16667;

impl DeformUserLogic for PongGame {
    type Inputs = PongInputs;
    type GameState = PongGameState;
    type Smoother = PongGameStateSmoother;
    type Error = PongError;

    const TICK_RATE_MICROS: u64 = TICK_RATE_MICROS;

    fn new_from_lobby(
        _lobby_metadata: &LobbyMetadata,
        _not_started: &LobbyNotStarted,
    ) -> Result<Self, PongError> {
        Ok(PongGame)
    }

    fn new_game_from_lobby(
        lobby_metadata: &LobbyMetadata,
        not_started: &LobbyNotStarted,
    ) -> Result<PongGameState, PongError> {
        let mut players = HashMap::new();
        for player in not_started.player_status.keys() {
            players.insert(
                *player,
                PlayerState {
                    paddle_y: 0.0,
                    score: 0,
                },
            );
        }

        Ok(PongGameState {
            ball_pos: Vec2::ZERO,
            ball_vel: Vec2::ZERO,
            creator: lobby_metadata.creator,
            players,
        })
    }

    fn advance_frame(
        &mut self,
        state: &Self::GameState,
        inputs: &BTreeMap<Pubkey, Self::Inputs>,
    ) -> Result<Self::GameState, Self::Error> {
        let dt = Self::TICK_RATE_MICROS as f32 / 1_000_000.0;
        let mut new = state.clone();

        if new.ball_vel == Vec2::ZERO {
            new.reset_round(1.0);
            return Ok(new);
        }

        // Creator is always on the left, the other player on the right
        let left_pk = Some(&new.creator);
        let right_pk = inputs.keys().find(|pk| **pk != new.creator);

        // Apply inputs
        for pk in [left_pk, right_pk].into_iter().flatten() {
            if let Some(input) = inputs.get(pk) {
                if let Some(ps) = new.players.get_mut(pk) {
                    ps.paddle_y += (input.direction as f32 / 100.0) * PADDLE_SPEED * dt;
                    ps.paddle_y = ps.paddle_y.clamp(
                        -FIELD_H / 2.0 + PADDLE_HALF_H,
                        FIELD_H / 2.0 - PADDLE_HALF_H,
                    );
                }
            }
        }

        // Move ball
        new.ball_pos += new.ball_vel * dt;

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
            new.reset_round(-1.0);
        } else if new.ball_pos.x + BALL_HALF >= FIELD_W / 2.0 {
            if let Some(pk) = left_pk {
                if let Some(ps) = new.players.get_mut(pk) {
                    ps.score += 1;
                }
            }
            new.reset_round(1.0);
        }

        Ok(new)
    }
}

#[cfg(feature = "bin")]
mod server_logic {
    use deform_quic::{DeformQuicLogic, UserIdentification};
    use wincode::{SchemaRead, SchemaWrite};

    use super::*;
    use crate::solana::anchor_client::PongAnchorClient;

    #[derive(Clone, Debug, SchemaRead, SchemaWrite)]
    pub enum NoCustomMessage {
        // so compiler does not complain about wincode
        Never,
    }

    #[derive(Clone, Debug, SchemaRead, SchemaWrite)]
    pub struct NoAuth;

    #[derive(Clone, Debug)]
    pub struct PongQuicLogic;

    impl DeformQuicLogic for PongQuicLogic {
        type CustomReliableMessage = NoCustomMessage;
        type Auth = NoAuth;
        type UserLogic = PongGame;
        type ProgramClient = PongAnchorClient;

        fn authorize_connection(
            _identification: &UserIdentification<Self>,
        ) -> Result<(), PongError> {
            Ok(())
        }
    }

    /// Fully-on-chain backend binding: same game, same instruction builder as the
    /// Web2 backend, but state comes from the ER instead of a QUIC server.
    #[cfg(feature = "foc")]
    #[derive(Clone, Debug)]
    pub struct PongFocLogic;

    #[cfg(feature = "foc")]
    impl deform_foc::DeformFocLogic for PongFocLogic {
        type UserLogic = PongGame;
        type ProgramClient = PongAnchorClient;
    }
}

#[cfg(feature = "foc")]
pub use server_logic::PongFocLogic;
#[cfg(feature = "bin")]
pub use server_logic::{NoAuth, PongQuicLogic};

pub fn pong_bot(state: &PongGameState, bot: &Pubkey, prev_inputs: &PongInputs) -> PongInputs {
    // Creator is always the left paddle, same as `advance_frame`.
    let is_left = *bot == state.creator;
    let paddle_x = if is_left { -PADDLE_X } else { PADDLE_X };

    // Idle while the ball is heading away from us.
    let incoming = if is_left {
        state.ball_vel.x < 0.0
    } else {
        state.ball_vel.x > 0.0
    };
    if !incoming {
        return PongInputs::default();
    }

    let paddle_y = state.players.get(bot).map(|p| p.paddle_y).unwrap_or(0.0);

    // Predict where the ball will be when it reaches our paddle
    let t = (paddle_x - state.ball_pos.x) / state.ball_vel.x;
    // Small deterministic offset so the bot doesn't always return the ball dead-center
    let offset = (state.ball_vel.y * 100.0).sin() * PADDLE_HALF_H * 0.4;
    let target_y = state.ball_pos.y + state.ball_vel.y * t + offset;

    let diff = target_y - paddle_y;
    let prev = prev_inputs.direction;

    let paddle_per_tick = PADDLE_SPEED / 60.0;
    let threshold = if (prev > 0 && diff > 0.0) || (prev < 0 && diff < 0.0) {
        paddle_per_tick * 0.25
    } else {
        paddle_per_tick * 1.5
    };

    let direction = if diff.abs() < threshold {
        0
    } else if diff > 0.0 {
        100
    } else {
        -100
    };

    PongInputs { direction }
}
