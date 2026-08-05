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

// --- Arena geometry (from Eggy League, units are game-pixels) ---
pub const LEFT_WALL: f32 = -620.0;
pub const RIGHT_WALL: f32 = 620.0;
pub const CEILING: f32 = 864.0;
pub const FIELD_W: f32 = RIGHT_WALL - LEFT_WALL; // 1240
pub const FIELD_H: f32 = CEILING; // 864

// --- Player constants ---
pub const PLAYER_RADIUS: f32 = 43.75;
pub const PLAYER_SPEED: f32 = 250.0;
pub const JUMP_VELOCITY: f32 = 750.0;
pub const GRAVITY: f32 = -1200.0;
pub const GRAVITY_RELEASE: f32 = -2000.0;
pub const MAX_FALL_SPEED: f32 = -750.0;

// --- Ball constants ---
pub const BALL_RADIUS: f32 = 21.875;
pub const BOUNCE_COEFF: f32 = 0.9;
pub const MAX_BALL_SPEED: f32 = 1000.0;

// --- Goal geometry ---
pub const GOAL_HEIGHT: f32 = 165.0;
pub const GOAL_THICKNESS: f32 = 30.0;
pub const GOAL_WIDTH: f32 = 85.0;
pub const HALF_GOAL_WIDTH: f32 = GOAL_WIDTH / 2.0;

// --- Game rules ---
pub const WIN_SCORE: u32 = 5;
pub const FPS: f32 = 20.0;
pub const KICKOFF_TICKS: u64 = 60; // 3 seconds at 20Hz
pub const GOAL_TICKS: u64 = 40; // 2 seconds at 20Hz

// the ephemeral validators run at 20Hz
#[cfg(feature = "20hz")]
pub const TICK_RATE_MICROS: u64 = 50000;
#[cfg(feature = "60hz")]
pub const TICK_RATE_MICROS: u64 = 16667;

// ─── Goal collision sides ───────────────────────────────────────

struct RectangleSide {
    center: Vec2,
    normal: Vec2,
    half_len: f32,
}

impl RectangleSide {
    fn closest_point(&self, point: Vec2) -> Vec2 {
        let center_to_point = point - self.center;
        let proj = center_to_point.dot(self.normal);
        point - (proj * self.normal)
    }

    fn point_belongs_by_distance(&self, point: Vec2) -> bool {
        self.center.distance(point) <= self.half_len
    }
}

const LEFT_GOAL: [RectangleSide; 3] = [
    // crossbar top
    RectangleSide {
        center: Vec2 {
            x: LEFT_WALL + (GOAL_WIDTH / 2.0),
            y: GOAL_HEIGHT + (GOAL_THICKNESS / 2.0),
        },
        normal: Vec2::Y,
        half_len: GOAL_WIDTH / 2.0,
    },
    // crossbar bottom
    RectangleSide {
        center: Vec2 {
            x: LEFT_WALL + (GOAL_WIDTH / 2.0),
            y: GOAL_HEIGHT - (GOAL_THICKNESS / 2.0),
        },
        normal: Vec2::NEG_Y,
        half_len: GOAL_WIDTH / 2.0,
    },
    // goalpost inner edge
    RectangleSide {
        center: Vec2 {
            x: LEFT_WALL + GOAL_WIDTH,
            y: GOAL_HEIGHT,
        },
        normal: Vec2::X,
        half_len: GOAL_THICKNESS / 2.0,
    },
];

const RIGHT_GOAL: [RectangleSide; 3] = [
    RectangleSide {
        center: Vec2 {
            x: RIGHT_WALL - (GOAL_WIDTH / 2.0),
            y: GOAL_HEIGHT + (GOAL_THICKNESS / 2.0),
        },
        normal: Vec2::Y,
        half_len: GOAL_WIDTH / 2.0,
    },
    RectangleSide {
        center: Vec2 {
            x: RIGHT_WALL - (GOAL_WIDTH / 2.0),
            y: GOAL_HEIGHT - (GOAL_THICKNESS / 2.0),
        },
        normal: Vec2::NEG_Y,
        half_len: GOAL_WIDTH / 2.0,
    },
    RectangleSide {
        center: Vec2 {
            x: RIGHT_WALL - GOAL_WIDTH,
            y: GOAL_HEIGHT,
        },
        normal: Vec2::NEG_X,
        half_len: GOAL_THICKNESS / 2.0,
    },
];

// ─── Data types ─────────────────────────────────────────────────

#[derive(
    Default,
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    serde::Serialize,
    serde::Deserialize,
    SchemaRead,
    SchemaWrite,
)]
pub enum PlayerDir {
    #[default]
    Right,
    Left,
}

#[derive(
    Default,
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    serde::Serialize,
    serde::Deserialize,
    SchemaRead,
    SchemaWrite,
)]
#[cfg_attr(
    feature = "anchor",
    derive(anchor_lang::AnchorSerialize, anchor_lang::AnchorDeserialize)
)]
pub enum GamePhase {
    #[default]
    KickOff,
    Playing,
    Goal,
}

#[derive(
    Default, Debug, Clone, serde::Serialize, serde::Deserialize, SchemaRead, SchemaWrite, Smooth,
)]
pub struct PlayerState {
    #[smooth]
    #[wincode(with = "PodVec2")]
    pub pos: Vec2,
    #[wincode(with = "PodVec2")]
    pub vel: Vec2,
    pub dir: PlayerDir,
    pub score: u32,
    pub grounded: bool,
    pub prev_jump: bool,
}

#[derive(
    Default, Debug, Clone, serde::Serialize, serde::Deserialize, SchemaRead, SchemaWrite, Smooth,
)]
#[smooth(
    decay = 0.8,
    max_offset = 200.0,
    min_offset_sq = 4.0,
    max_correction = 20.0
)]
pub struct SoccerGameState {
    #[smooth]
    #[wincode(with = "PodVec2")]
    pub ball_pos: Vec2,
    #[wincode(with = "PodVec2")]
    pub ball_vel: Vec2,
    pub last_player_contact: Option<Pubkey>,
    pub creator: Pubkey,
    #[smooth(map)]
    pub players: HashMap<Pubkey, PlayerState>,
    pub phase: GamePhase,
    pub phase_ticks: u64,
}

impl SoccerGameState {
    pub fn reset_round(&mut self) {
        self.ball_pos = Vec2::new(0.0, 300.0);
        self.ball_vel = Vec2::ZERO;
        self.last_player_contact = None;

        let creator = self.creator;
        for (pk, ps) in self.players.iter_mut() {
            let x = if *pk == creator { -300.0 } else { 300.0 };
            ps.pos = Vec2::new(x, PLAYER_RADIUS);
            ps.vel = Vec2::ZERO;
            ps.dir = if *pk == creator {
                PlayerDir::Right
            } else {
                PlayerDir::Left
            };
            ps.grounded = true;
            ps.prev_jump = false;
        }
    }
}

impl DeformGameState for SoccerGameState {
    fn has_ended(&self) -> bool {
        self.players.values().any(|ps| ps.score >= WIN_SCORE)
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
pub struct SoccerInputs {
    pub horizontal: i8,
    pub jump: bool,
}

impl DeformInputs for SoccerInputs {
    fn merge(&mut self, newer: &Self) {
        let jump = self.jump || newer.jump;
        *self = newer.clone();
        self.jump = jump;
    }
}

#[derive(Debug, Clone, SchemaRead, SchemaWrite, serde::Serialize, serde::Deserialize)]
pub struct SoccerGame;

#[derive(Debug, Clone, serde::Serialize, SchemaRead, SchemaWrite, thiserror::Error)]
pub enum SoccerError {
    #[error("unreachable")]
    Never,
    #[error("Lobby should be started")]
    LobbyNotStarted,
    #[error("Error serializing inputs: {0}")]
    SerializeInputs(String),
    #[error("Error scheduling crank: {0}")]
    ScheduleCrank(String),
}

impl DeformUserLogic for SoccerGame {
    type Inputs = SoccerInputs;
    type GameState = SoccerGameState;
    type Smoother = SoccerGameStateSmoother;
    type Error = SoccerError;

    const TICK_RATE_MICROS: u64 = TICK_RATE_MICROS;

    fn new_from_lobby(
        _lobby_metadata: &LobbyMetadata,
        _not_started: &LobbyNotStarted,
    ) -> Result<Self, SoccerError> {
        Ok(SoccerGame)
    }

    fn new_game_from_lobby(
        lobby_metadata: &LobbyMetadata,
        not_started: &LobbyNotStarted,
    ) -> Result<SoccerGameState, SoccerError> {
        let creator = lobby_metadata.creator;
        let mut players = HashMap::new();
        for player in not_started.player_status.keys() {
            let x = if *player == creator { -300.0 } else { 300.0 };
            let dir = if *player == creator {
                PlayerDir::Right
            } else {
                PlayerDir::Left
            };
            players.insert(
                *player,
                PlayerState {
                    pos: Vec2::new(x, PLAYER_RADIUS),
                    vel: Vec2::ZERO,
                    dir,
                    score: 0,
                    grounded: true,
                    prev_jump: false,
                },
            );
        }

        Ok(SoccerGameState {
            ball_pos: Vec2::new(0.0, 300.0),
            ball_vel: Vec2::ZERO,
            last_player_contact: None,
            creator,
            players,
            phase: GamePhase::KickOff,
            phase_ticks: 0,
        })
    }

    fn advance_frame(
        &mut self,
        state: &Self::GameState,
        inputs: &BTreeMap<Pubkey, Self::Inputs>,
    ) -> Result<Self::GameState, Self::Error> {
        let mut new = state.clone();
        advance(&mut new, inputs);
        Ok(new)
    }
}

// ─── Physics / game logic ───────────────────────────────────────

fn advance(state: &mut SoccerGameState, inputs: &BTreeMap<Pubkey, SoccerInputs>) {
    let dt = 1.0 / FPS;

    state.phase_ticks += 1;

    // --- state machine ---
    match state.phase {
        GamePhase::KickOff => {
            if state.phase_ticks >= KICKOFF_TICKS {
                state.phase = GamePhase::Playing;
                state.phase_ticks = 0;
            } else {
                return;
            }
        }
        GamePhase::Goal => {
            if state.phase_ticks >= GOAL_TICKS {
                state.phase = GamePhase::KickOff;
                state.phase_ticks = 0;
                state.reset_round();
                return;
            }
            // during Goal, physics still runs but no goal detection
        }
        GamePhase::Playing => {}
    }

    // --- process inputs ---
    for (pk, input) in inputs {
        if let Some(ps) = state.players.get_mut(pk) {
            ps.vel.x = (input.horizontal as f32 / 100.0) * PLAYER_SPEED;

            if input.horizontal > 0 {
                ps.dir = PlayerDir::Right;
            } else if input.horizontal < 0 {
                ps.dir = PlayerDir::Left;
            }

            let jump_just_pressed = !ps.prev_jump && input.jump;
            if jump_just_pressed && ps.grounded {
                ps.vel.y = JUMP_VELOCITY;
                ps.grounded = false;
            }
            ps.prev_jump = input.jump;
        }
    }

    // --- gravity on players ---
    for (pk, ps) in state.players.iter_mut() {
        if !ps.grounded {
            let input = inputs.get(pk);
            let holding_jump = input.map_or(false, |i| i.jump);
            if ps.vel.y > 0.0 && !holding_jump {
                ps.vel.y += GRAVITY_RELEASE * dt;
            } else {
                ps.vel.y += GRAVITY * dt;
            }
            if ps.vel.y < MAX_FALL_SPEED {
                ps.vel.y = MAX_FALL_SPEED;
            }
        }
    }

    // --- gravity on ball ---
    state.ball_vel.y += GRAVITY * dt;
    if state.ball_vel.y < MAX_FALL_SPEED {
        state.ball_vel.y = MAX_FALL_SPEED;
    }

    // --- integrate positions ---
    for ps in state.players.values_mut() {
        ps.pos += ps.vel * dt;
    }
    state.ball_pos += state.ball_vel * dt;

    // --- ball-geometry collisions ---
    ball_geometry_collisions(state);

    // --- player-geometry collisions ---
    player_geometry_collisions(state);

    // --- ball-player collisions ---
    ball_player_collisions(state);

    // --- player-player collisions ---
    player_player_collisions(state);

    // --- ball-goal frame collisions ---
    ball_goal_collisions(state);

    // --- ball-geometry again (cleanup after kick) ---
    ball_geometry_collisions(state);

    // --- push players out of ball ---
    push_players_out_of_ball(state);

    // --- detect goals (only during Playing) ---
    if matches!(state.phase, GamePhase::Playing) {
        detect_goal(state);
    }

    // --- clamp ball speed ---
    state.ball_vel = state.ball_vel.clamp(
        Vec2::new(-MAX_BALL_SPEED, -MAX_BALL_SPEED),
        Vec2::new(MAX_BALL_SPEED, MAX_BALL_SPEED),
    );
}

fn ball_geometry_collisions(state: &mut SoccerGameState) {
    let ball = &mut state.ball_pos;
    let vel = &mut state.ball_vel;

    // floor
    if ball.y <= BALL_RADIUS {
        ball.y = BALL_RADIUS;
        if vel.y < 0.0 {
            *vel = vel.reflect(Vec2::Y) * BOUNCE_COEFF;
        }
    }

    // ceiling
    if ball.y >= CEILING - BALL_RADIUS {
        ball.y = CEILING - BALL_RADIUS;
        if vel.y > 0.0 {
            *vel = vel.reflect(Vec2::NEG_Y) * BOUNCE_COEFF;
        }
    }

    // right wall
    if ball.x >= RIGHT_WALL - BALL_RADIUS {
        ball.x = RIGHT_WALL - BALL_RADIUS;
        if vel.x > 0.0 {
            *vel = vel.reflect(Vec2::NEG_X) * BOUNCE_COEFF;
        }
    }

    // left wall
    if ball.x <= LEFT_WALL + BALL_RADIUS {
        ball.x = LEFT_WALL + BALL_RADIUS;
        if vel.x < 0.0 {
            *vel = vel.reflect(Vec2::X) * BOUNCE_COEFF;
        }
    }
}

fn player_geometry_collisions(state: &mut SoccerGameState) {
    for ps in state.players.values_mut() {
        ps.pos.x = ps
            .pos
            .x
            .clamp(LEFT_WALL + PLAYER_RADIUS, RIGHT_WALL - PLAYER_RADIUS);

        if ps.pos.y <= PLAYER_RADIUS {
            ps.pos.y = PLAYER_RADIUS;
            ps.grounded = true;
        } else {
            ps.grounded = false;
        }
    }
}

fn ball_player_collisions(state: &mut SoccerGameState) {
    let ball_pos = &mut state.ball_pos;
    let ball_vel = &mut state.ball_vel;

    // collect player data to avoid borrow issues
    let player_data: Vec<(Pubkey, Vec2, Vec2, PlayerDir)> = state
        .players
        .iter()
        .map(|(pk, ps)| (*pk, ps.pos, ps.vel, ps.dir))
        .collect();

    for (pk, player_pos, player_vel, player_dir) in &player_data {
        let player_to_ball = *ball_pos - *player_pos;
        let distance = player_to_ball.length();
        let penetration = distance - (PLAYER_RADIUS + BALL_RADIUS);

        if penetration <= 0.0 {
            state.last_player_contact = Some(*pk);

            let penetration_distance = penetration.abs();
            let mut collision_normal = (*ball_pos - *player_pos).normalize_or_zero();

            if collision_normal == Vec2::ZERO {
                collision_normal = Vec2::Y;
            }

            // ground assist: if ball is near ground and player faces ball, nudge normal upward
            if ball_pos.y <= BALL_RADIUS * 1.5 {
                let facing_ball = matches!(player_dir, PlayerDir::Right) && player_to_ball.x > 0.0
                    || matches!(player_dir, PlayerDir::Left) && player_to_ball.x < 0.0;
                if facing_ball {
                    let min_y = f32::sin(45.0_f32.to_radians());
                    if collision_normal.y < min_y {
                        collision_normal.y = min_y;
                        collision_normal.x =
                            f32::sqrt(1.0 - min_y * min_y) * collision_normal.x.signum();
                        collision_normal = collision_normal.normalize_or_zero();
                    }
                }
            }

            // push ball out
            *ball_pos += penetration_distance * collision_normal;

            // redirect velocity along normal, losing energy
            let vel_len = ball_vel.length();
            *ball_vel = vel_len * collision_normal * BOUNCE_COEFF;

            // constant upward + horizontal nudge
            *ball_vel += Vec2::new(ball_vel.x.signum() * 20.0, 150.0);

            // add player velocity contribution
            if let Some(player_vel_norm) = player_vel.try_normalize() {
                if player_vel_norm.dot(collision_normal) >= f32::cos(45.0_f32.to_radians()) {
                    let mag = player_vel.length();
                    *ball_vel += collision_normal * mag * 0.75;
                }
            }
        }
    }
}

fn player_player_collisions(state: &mut SoccerGameState) {
    let keys: Vec<Pubkey> = state.players.keys().copied().collect();
    for i in 0..keys.len() {
        for j in (i + 1)..keys.len() {
            let pos_a = state.players[&keys[i]].pos;
            let pos_b = state.players[&keys[j]].pos;
            let a_to_b = pos_b - pos_a;
            let distance = a_to_b.length();
            let target = PLAYER_RADIUS * 2.0;
            let overstep = target - distance;
            if overstep > 0.0 {
                let dir = a_to_b.normalize_or_zero();
                let half = overstep / 2.0;
                state.players.get_mut(&keys[i]).unwrap().pos -= dir * half;
                state.players.get_mut(&keys[j]).unwrap().pos += dir * half;
            }
        }
    }
}

fn collide_ball_with_side(
    ball_pos: &mut Vec2,
    ball_vel: &mut Vec2,
    side: &RectangleSide,
    closest_point: Vec2,
) {
    let inverse_normal = -side.normal;
    let max_penetration_point = *ball_pos + (inverse_normal * BALL_RADIUS);
    let penetration = max_penetration_point.distance(closest_point);
    *ball_pos += side.normal * penetration;

    let reflect = ball_vel.reflect(side.normal);
    *ball_vel = reflect * BOUNCE_COEFF;
}

fn ball_goal_collisions(state: &mut SoccerGameState) {
    for goals in [&LEFT_GOAL, &RIGHT_GOAL] {
        for side in goals.iter() {
            let closest_point = side.closest_point(state.ball_pos);
            let distance = closest_point.distance(state.ball_pos);
            if distance < BALL_RADIUS && side.point_belongs_by_distance(closest_point) {
                collide_ball_with_side(
                    &mut state.ball_pos,
                    &mut state.ball_vel,
                    side,
                    closest_point,
                );
                return;
            }
        }
    }
}

fn push_players_out_of_ball(state: &mut SoccerGameState) {
    let ball_pos = state.ball_pos;
    for ps in state.players.values_mut() {
        let player_to_ball = ball_pos - ps.pos;
        let distance = player_to_ball.length();
        let min_distance = PLAYER_RADIUS + BALL_RADIUS;
        if distance < min_distance {
            if let Some(dir) = player_to_ball.try_normalize() {
                let overlap = min_distance - distance;
                ps.pos -= dir * overlap;
            }
        }
    }
}

fn detect_goal(state: &mut SoccerGameState) {
    if state.ball_pos.y > GOAL_HEIGHT {
        return;
    }

    let touching_left = state.ball_pos.x <= LEFT_WALL + BALL_RADIUS;
    let past_left = state.ball_pos.x <= LEFT_WALL + HALF_GOAL_WIDTH + BALL_RADIUS;
    let touching_right = state.ball_pos.x >= RIGHT_WALL - BALL_RADIUS;
    let past_right = state.ball_pos.x >= RIGHT_WALL - HALF_GOAL_WIDTH - BALL_RADIUS;

    let creator = state.creator;

    if touching_left || past_left {
        // goal on the left side — the right player (non-creator) scores
        for (pk, ps) in state.players.iter_mut() {
            if *pk != creator {
                ps.score += 1;
                break;
            }
        }
        state.phase = GamePhase::Goal;
        state.phase_ticks = 0;
    } else if touching_right || past_right {
        // goal on the right side — the left player (creator) scores
        if let Some(ps) = state.players.get_mut(&creator) {
            ps.score += 1;
        }
        state.phase = GamePhase::Goal;
        state.phase_ticks = 0;
    }
}

// ─── Server / QUIC logic ───────────────────────────────────────

#[cfg(feature = "bin")]
mod server_logic {
    use deform_quic::{DeformQuicLogic, UserIdentification};
    use wincode::{SchemaRead, SchemaWrite};

    use super::*;
    use crate::solana::anchor_client::SoccerAnchorClient;

    #[derive(Clone, Debug, SchemaRead, SchemaWrite)]
    pub enum NoCustomMessage {
        Never,
    }

    #[derive(Clone, Debug, SchemaRead, SchemaWrite)]
    pub struct NoAuth;

    #[derive(Clone, Debug)]
    pub struct SoccerQuicLogic;

    impl DeformQuicLogic for SoccerQuicLogic {
        type CustomReliableMessage = NoCustomMessage;
        type Auth = NoAuth;
        type UserLogic = SoccerGame;
        type ProgramClient = SoccerAnchorClient;

        fn authorize_connection(
            _identification: &UserIdentification<Self>,
        ) -> Result<(), SoccerError> {
            Ok(())
        }
    }

    #[cfg(feature = "foc")]
    #[derive(Clone, Debug)]
    pub struct SoccerFocLogic;

    #[cfg(feature = "foc")]
    impl deform_foc::DeformFocLogic for SoccerFocLogic {
        type UserLogic = SoccerGame;
        type ProgramClient = SoccerAnchorClient;
    }
}

// ─── Bot AI ─────────────────────────────────────────────────────
use std::cell::RefCell;

#[cfg(feature = "foc")]
pub use server_logic::SoccerFocLogic;
#[cfg(feature = "bin")]
pub use server_logic::{NoAuth, SoccerQuicLogic};

const BOT_DT: f32 = 1.0 / FPS;
const ACTION_HOLD_MIN: u32 = 2;
const ACTION_HOLD_MAX: u32 = 5;

struct SmartBotState {
    rng: fastrand::Rng,
    cached_inputs: SoccerInputs,
    hold_remaining: u32,
    target_offset: f32,
}

impl SmartBotState {
    fn new() -> Self {
        Self {
            rng: fastrand::Rng::new(),
            cached_inputs: SoccerInputs::default(),
            hold_remaining: 0,
            target_offset: 0.0,
        }
    }
}

thread_local! {
    static BOT_STATE: RefCell<SmartBotState> = RefCell::new(SmartBotState::new());
}

pub fn soccer_bot(
    state: &SoccerGameState,
    bot: &Pubkey,
    _prev_inputs: &SoccerInputs,
) -> SoccerInputs {
    BOT_STATE.with(|cell| {
        let mut s = cell.borrow_mut();

        if s.hold_remaining > 0 {
            s.hold_remaining -= 1;
            return s.cached_inputs.clone();
        }

        let inputs = compute_bot_inputs(&mut s, state, bot);
        s.cached_inputs = inputs.clone();
        s.hold_remaining = s.rng.u32(ACTION_HOLD_MIN..=ACTION_HOLD_MAX);
        inputs
    })
}

fn compute_bot_inputs(
    s: &mut SmartBotState,
    state: &SoccerGameState,
    bot: &Pubkey,
) -> SoccerInputs {
    let Some(me) = state.players.get(bot) else {
        return SoccerInputs::default();
    };

    let is_left_player = *bot == state.creator;
    let own_goal_x = if is_left_player {
        LEFT_WALL
    } else {
        RIGHT_WALL
    };
    let attack_dir: f32 = if is_left_player { 1.0 } else { -1.0 };

    let ball_moving_toward_own_goal = (state.ball_vel.x * attack_dir) < -50.0;
    let ball_close_to_own_goal = (state.ball_pos.x - own_goal_x).abs() < 350.0;
    let defending = ball_moving_toward_own_goal && ball_close_to_own_goal;

    // predict ball position ~12 ticks ahead
    let look_ahead = 12.0 * BOT_DT;
    let pred_ball = Vec2::new(
        (state.ball_pos.x + state.ball_vel.x * look_ahead)
            .clamp(LEFT_WALL + BALL_RADIUS, RIGHT_WALL - BALL_RADIUS),
        (state.ball_pos.y
            + state.ball_vel.y * look_ahead
            + 0.5 * GRAVITY * look_ahead * look_ahead)
            .max(BALL_RADIUS),
    );

    // refresh random offset occasionally for variation
    if s.rng.u32(0..20) == 0 {
        s.target_offset = (s.rng.f32() - 0.5) * 50.0;
    }

    let target_x;
    let should_jump;

    if defending {
        // position between ball and own goal, slightly toward ball
        let midpoint = (pred_ball.x + own_goal_x) * 0.5;
        let bias_toward_ball = (pred_ball.x - midpoint) * 0.3;
        target_x = midpoint + bias_toward_ball + s.target_offset * 0.5;

        let dx = (me.pos.x - pred_ball.x).abs();
        let dy = pred_ball.y - me.pos.y;
        should_jump = me.grounded && dx < PLAYER_RADIUS * 3.5 && dy > PLAYER_RADIUS * 0.6;
    } else {
        // attack: approach the ball from behind so we push it toward the opponent's goal
        let approach_offset = -attack_dir * (BALL_RADIUS + PLAYER_RADIUS * 0.6);
        target_x = pred_ball.x + approach_offset + s.target_offset;

        let dx = (me.pos.x - pred_ball.x).abs();
        let dy = pred_ball.y - me.pos.y;

        should_jump = me.grounded
            && dx < PLAYER_RADIUS * 2.5
            && dy > PLAYER_RADIUS * 0.7
            && dy < PLAYER_RADIUS * 6.0;
    }

    let diff_x = target_x - me.pos.x;
    let horizontal = if diff_x > 8.0 {
        100
    } else if diff_x < -8.0 {
        -100
    } else {
        0
    };

    SoccerInputs {
        horizontal,
        jump: should_jump,
    }
}
