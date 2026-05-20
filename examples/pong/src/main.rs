use std::collections::HashMap;

use deform_core::{
    DeformGameState, DeformInputs, DeformResult, DeformUserLogic, MaxLen, Pubkey, Smooth,
};
use wincode::{SchemaRead, SchemaWrite};

#[derive(Default, Clone, serde::Serialize, SchemaRead, SchemaWrite, Smooth)]
#[smooth(decay = 0.9, max_offset = 200.0, min_offset_sq = 4.0)]
pub struct PongGameState {
    #[smooth]
    pub ball_x: f32,
    #[smooth]
    pub ball_y: f32,
    pub ball_vel_x: f32,
    pub ball_vel_y: f32,
    #[smooth]
    pub paddle_left_y: f32,
    #[smooth]
    pub paddle_right_y: f32,
    pub score_left: u32,
    pub score_right: u32,
}

impl DeformGameState for PongGameState {}

impl MaxLen for PongGameState {
    fn max_len() -> DeformResult<usize> {
        Ok(256)
    }
}

#[derive(Default, Clone, Eq, PartialEq, serde::Serialize, SchemaRead, SchemaWrite)]
pub struct PongInputs {
    pub direction: i8,
}

impl DeformInputs for PongInputs {}

impl MaxLen for PongInputs {
    fn max_len() -> DeformResult<usize> {
        Ok(16)
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
        _inputs: &HashMap<Pubkey, Self::Inputs>,
    ) -> Result<Self::GameState, Self::Error> {
        let new = state.clone();
        // TODO: game logic
        Ok(new)
    }
}

fn main() {}
