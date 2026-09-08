use std::collections::{BTreeMap, HashMap};

use deform_core::{
    DeformGameState, DeformInputs, DeformUserLogic, Pubkey, Smooth,
    accounts::lobby::{LobbyMetadata, not_started::LobbyNotStarted},
};
use glam::{Vec2, Vec3};
use wincode::{SchemaRead, SchemaWrite};

wincode::pod_wrapper! {
    unsafe struct PodVec2(Vec2);
}
wincode::pod_wrapper! {
    unsafe struct PodVec3(Vec3);
}

// 60Hz. A fully-on-chain game must match the ephemeral validators' 20Hz slot
// time, but this example has no FoC backend (avian can't run in SBF), so the
// simulation is free to run at a proper client/server rate.
pub const TICK_RATE_MICROS: u64 = 16_667;

// Warehouse bounds, in metres; static geometry and spawns come from Blender.
pub const ARENA_HALF_X: f32 = 12.0;
pub const ARENA_HALF_Z: f32 = 18.0;

// --- players: simple capsules ---
pub const PLAYER_RADIUS: f32 = 0.4;
/// Length of the cylindrical part of the capsule (total height = length + 2 * radius).
pub const PLAYER_CAPSULE_LENGTH: f32 = 1.0;
/// Tnua float height: how high the capsule *center* hovers above the floor.
pub const PLAYER_FLOAT_HEIGHT: f32 = PLAYER_CAPSULE_LENGTH / 2.0 + PLAYER_RADIUS + 0.2;
/// Camera/muzzle height above the capsule center.
pub const PLAYER_EYE_HEIGHT: f32 = 0.6;
pub const PLAYER_SPEED: f32 = 5.5;
pub const PLAYER_JUMP_HEIGHT: f32 = 1.3;

// Hits resolve immediately; retained events let slower render frames see shots.
pub const SHOT_RANGE: f32 = 100.0;
pub const SHOT_EVENT_TTL: u16 = 60;
pub const FIRE_COOLDOWN_TICKS: u16 = 12;
pub const MAX_SHOT_EVENTS: usize = 32;

pub const WIN_SCORE: u32 = 10;
pub const SPAWN_FREEZE_TICKS: u16 = 180;
pub const ROUND_OVER_TICKS: u16 = 300;

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
pub enum RoundPhase {
    #[default]
    SpawnFreeze,
    Playing,
    RoundOver,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, SchemaRead, SchemaWrite)]
pub struct DeathState {
    pub killer: Pubkey,
    /// Fixed at the hit, independent of the falling body and input prediction.
    #[wincode(with = "PodVec3")]
    pub camera_position: Vec3,
}

/// One player's input for one tick.
///
/// Everything is quantized integers because `DeformInputs` requires `Eq` (the
/// netcode detects mispredictions by equality). The look direction is *part of the
/// inputs*, not of the camera: the local camera turns with the mouse every render
/// frame and is never touched by rollbacks, but each frame the client sends "I am
/// looking this way" so the 60 Hz simulation can aim shots and other clients
/// can orient this player's capsule.
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
pub struct ShooterInputs {
    /// Strafe: -100 (left) .. 100 (right), relative to where the player looks.
    pub move_x: i8,
    /// Forward: -100 (back) .. 100 (forward), relative to where the player looks.
    pub move_z: i8,
    /// M1 held. The per-player cooldown in the simulation turns this into shots.
    pub fire: bool,
    /// Space held. Fed to tnua's jump action while true: holding gives the full
    /// jump height, releasing early shortens it, and tnua only starts a jump from
    /// the ground (`allow_in_air: false`), so there is no double jumping.
    pub jump: bool,
    /// Yaw quantized to the full u16 range over [0, 2π). 0 looks toward -Z.
    pub yaw_q: u16,
    /// Pitch quantized over [-π/2, π/2]; positive looks up.
    pub pitch_q: i16,
}

impl ShooterInputs {
    pub fn set_look(&mut self, yaw: f32, pitch: f32) {
        let tau = std::f32::consts::TAU;
        self.yaw_q = ((yaw.rem_euclid(tau) / tau) * 65536.0) as u16;
        let half_pi = std::f32::consts::FRAC_PI_2;
        self.pitch_q = ((pitch.clamp(-half_pi, half_pi) / half_pi) * i16::MAX as f32) as i16;
    }

    pub fn yaw(&self) -> f32 {
        (self.yaw_q as f32 / 65536.0) * std::f32::consts::TAU
    }

    pub fn pitch(&self) -> f32 {
        (self.pitch_q as f32 / i16::MAX as f32) * std::f32::consts::FRAC_PI_2
    }

    /// Unit look direction. Yaw 0 / pitch 0 faces -Z (bevy's forward).
    pub fn look_dir(&self) -> Vec3 {
        let (yaw, pitch) = (self.yaw(), self.pitch());
        let (sy, cy) = yaw.sin_cos();
        let (sp, cp) = pitch.sin_cos();
        Vec3::new(-sy * cp, sp, -cy * cp)
    }

    /// Movement direction on the ground plane, rotated by yaw. Not normalized.
    pub fn move_dir(&self) -> Vec3 {
        let yaw = self.yaw();
        let (sy, cy) = yaw.sin_cos();
        let forward = Vec3::new(-sy, 0.0, -cy);
        let right = Vec3::new(cy, 0.0, -sy);
        (forward * (self.move_z as f32 / 100.0) + right * (self.move_x as f32 / 100.0))
            .clamp_length_max(1.0)
    }
}

impl DeformInputs for ShooterInputs {
    /// Several render frames land inside one tick, and only one value survives.
    /// Movement and look are *held* state, so the newest sample is the truthful
    /// one; `fire` and `jump` are actions, so they are OR-ed instead — a click that
    /// starts and ends between two ticks must still fire the shot.
    fn merge(&mut self, newer: &Self) {
        let fire = self.fire || newer.fire;
        let jump = self.jump || newer.jump;
        *self = newer.clone();
        self.fire = fire;
        self.jump = jump;
    }
}

#[derive(
    Default, Debug, Clone, serde::Serialize, serde::Deserialize, SchemaRead, SchemaWrite, Smooth,
)]
pub struct PlayerState {
    /// Capsule center.
    #[smooth]
    #[wincode(with = "PodVec3")]
    pub pos: Vec3,
    #[wincode(with = "PodVec3")]
    pub vel: Vec3,
    /// Unit facing on the XZ plane, for orienting other players' capsules. Smoothed
    /// as a vector (then re-normalized when rendering) so it never lerps the long
    /// way around like a raw angle would.
    #[smooth]
    #[wincode(with = "PodVec2")]
    pub look_xz: Vec2,
    pub pitch: f32,
    pub score: u32,
    /// Ticks until this player can fire again.
    pub cooldown: u16,
    pub death: Option<DeathState>,
    /// Full corpse orientation and angular velocity survive rollback/world rebuilds.
    pub body_rotation: [f32; 4],
    #[wincode(with = "PodVec3")]
    pub angular_velocity: Vec3,
}

#[derive(
    Default, Debug, Clone, serde::Serialize, serde::Deserialize, SchemaRead, SchemaWrite, Smooth,
)]
pub struct Shot {
    #[wincode(with = "PodVec3")]
    pub origin: Vec3,
    #[wincode(with = "PodVec3")]
    pub impact: Vec3,
    #[wincode(with = "PodVec3")]
    pub normal: Vec3,
    pub owner: Pubkey,
    pub hit_geometry: bool,
    pub hit_player: bool,
    pub ttl: u16,
}

/// Everything here is in meters, so the smoothing thresholds are much smaller than
/// pong's pixel-scale defaults: offsets under ~2 cm snap, offsets over 10 m are
/// treated as teleports.
#[derive(
    Default, Debug, Clone, serde::Serialize, serde::Deserialize, SchemaRead, SchemaWrite, Smooth,
)]
#[smooth(decay = 0.8, max_offset = 10.0, min_offset_sq = 0.0004)]
pub struct ShooterGameState {
    #[smooth(map)]
    #[serde(serialize_with = "deform_core::pubkey_map::serialize")]
    pub players: HashMap<Pubkey, PlayerState>,
    // Events are discrete: interpolating impact positions smears decals off walls.
    pub shots: BTreeMap<u32, Shot>,
    pub next_shot_id: u32,
    pub phase: RoundPhase,
    pub phase_ticks: u16,
    pub round: u32,
}

impl ShooterGameState {
    pub fn reset_round(&mut self) {
        let mut keys: Vec<_> = self.players.keys().copied().collect();
        keys.sort_unstable();
        for (slot, key) in keys.into_iter().enumerate() {
            let player = self.players.get_mut(&key).unwrap();
            let (pos, yaw) = spawn_for_slot(slot);
            *player = PlayerState {
                pos,
                look_xz: Vec2::new(-yaw.sin(), -yaw.cos()),
                score: player.score,
                body_rotation: glam::Quat::from_rotation_y(yaw).to_array(),
                ..Default::default()
            };
        }
        self.phase = RoundPhase::SpawnFreeze;
        self.phase_ticks = 0;
        self.round = self.round.wrapping_add(1);
        self.shots.clear();
        // Keep shot IDs monotonic so clients don't suppress the next round's effects.
    }
}

impl DeformGameState for ShooterGameState {
    fn has_ended(&self) -> bool {
        self.players.values().any(|ps| ps.score >= WIN_SCORE)
    }
}

#[derive(Debug, Clone, serde::Serialize, SchemaRead, SchemaWrite, thiserror::Error)]
pub enum ShooterError {
    // needed otherwise SchemaRead will warn about unreachable code
    #[error("unreachable")]
    Never,
    #[error("this build has no physics simulation (compiled without the `physics` feature)")]
    PhysicsUnavailable,
    #[error("Lobby should be started")]
    LobbyNotStarted,
    #[error("Error serializing inputs: {0}")]
    SerializeInputs(String),
    #[error("Error scheduling crank: {0}")]
    ScheduleCrank(String),
}

/// The one long-lived object per match. The interesting part is `sim`: a headless
/// bevy `World` running avian + tnua. It lives *here* — not in the game state —
/// precisely because rollbacks throw the game state away: the world survives, and
/// every `advance_frame` overwrites it wholesale from the authoritative
/// `ShooterGameState` before stepping, so its internal caches never become load-bearing.
#[derive(Debug, Clone, serde::Serialize, SchemaRead, SchemaWrite, Default)]
pub struct ShooterGame {
    /// Skipped by every serializer: peers and the chain only ever see plain state.
    /// A deserialized (or cloned) `ShooterGame` starts with an empty sim, which is
    /// rebuilt lazily on the next `advance_frame`.
    #[cfg(feature = "physics")]
    #[serde(skip)]
    #[wincode(skip)]
    pub sim: crate::physics_sim::PhysicsSim,
}

impl DeformUserLogic for ShooterGame {
    type Inputs = ShooterInputs;
    type GameState = ShooterGameState;
    type Smoother = ShooterGameStateSmoother;
    type Error = ShooterError;

    const TICK_RATE_MICROS: u64 = TICK_RATE_MICROS;
    // Bounded shot history leaves room for eight players and lobby metadata.
    const MAX_LOBBY_ACCOUNT_BYTES: u64 = 8192;

    fn new_from_lobby(
        _lobby_metadata: &LobbyMetadata,
        _not_started: &LobbyNotStarted,
    ) -> Result<Self, ShooterError> {
        Ok(ShooterGame::default())
    }

    fn new_game_from_lobby(
        _lobby_metadata: &LobbyMetadata,
        not_started: &LobbyNotStarted,
    ) -> Result<ShooterGameState, ShooterError> {
        let mut players = HashMap::new();
        for (i, player) in not_started.player_status.keys().enumerate() {
            let (pos, yaw) = spawn_for_slot(i);
            let look_xz = Vec2::new(-yaw.sin(), -yaw.cos());
            players.insert(
                *player,
                PlayerState {
                    pos,
                    vel: Vec3::ZERO,
                    look_xz,
                    pitch: 0.0,
                    score: 0,
                    body_rotation: glam::Quat::from_rotation_y(yaw).to_array(),
                    ..Default::default()
                },
            );
        }

        Ok(ShooterGameState {
            players,
            shots: BTreeMap::new(),
            next_shot_id: 0,
            ..Default::default()
        })
    }

    fn advance_frame(
        &mut self,
        state: &Self::GameState,
        inputs: &BTreeMap<Pubkey, Self::Inputs>,
    ) -> Result<Self::GameState, Self::Error> {
        #[cfg(feature = "physics")]
        {
            Ok(self.sim.step(state, inputs))
        }
        #[cfg(not(feature = "physics"))]
        {
            let _ = (state, inputs);
            Err(ShooterError::PhysicsUnavailable)
        }
    }
}

#[cfg(feature = "bin")]
mod server_logic {
    use deform_quic::{DeformQuicLogic, UserIdentification};
    use wincode::{SchemaRead, SchemaWrite};

    use super::*;
    use crate::solana::anchor_client::ShooterAnchorClient;

    #[derive(Clone, Debug, SchemaRead, SchemaWrite)]
    pub enum NoCustomMessage {
        // so compiler does not complain about wincode
        Never,
    }

    #[derive(Clone, Debug, SchemaRead, SchemaWrite)]
    pub struct NoAuth;

    #[derive(Clone, Debug)]
    pub struct ShooterQuicLogic;

    impl DeformQuicLogic for ShooterQuicLogic {
        type CustomReliableMessage = NoCustomMessage;
        type Auth = NoAuth;
        type UserLogic = ShooterGame;
        type ProgramClient = ShooterAnchorClient;

        fn authorize_connection(
            _identification: &UserIdentification<Self>,
        ) -> Result<(), ShooterError> {
            Ok(())
        }
    }
}

#[cfg(feature = "bin")]
pub use server_logic::{NoAuth, ShooterQuicLogic};

/// Offline bot: follows exported-map routes and fires only with line of sight.
/// Deterministic — everything is derived from the game state.
pub fn shooter_bot(
    state: &ShooterGameState,
    bot: &Pubkey,
    _prev_inputs: &ShooterInputs,
) -> ShooterInputs {
    let Some(me) = state.players.get(bot) else {
        return ShooterInputs::default();
    };

    if me.death.is_some() {
        return ShooterInputs::default();
    }

    // Nearest other player, by lowest key on ties (BTreeMap-style determinism).
    let mut target: Option<(&Pubkey, &PlayerState)> = None;
    for (pk, ps) in state.players.iter() {
        if pk == bot || ps.death.is_some() {
            continue;
        }
        let better = match target {
            None => true,
            Some((tpk, tps)) => {
                let d_new = ps.pos.distance_squared(me.pos);
                let d_old = tps.pos.distance_squared(me.pos);
                d_new < d_old || (d_new == d_old && pk < tpk)
            }
        };
        if better {
            target = Some((pk, ps));
        }
    }

    let mut inputs = ShooterInputs::default();
    let Some((_, target)) = target else {
        return inputs;
    };

    let eye = me.pos + Vec3::Y * PLAYER_EYE_HEIGHT;
    let aim = target.pos + Vec3::Y * 0.2;
    #[cfg(feature = "physics")]
    let visible = crate::navigation::visible(eye, aim);
    #[cfg(not(feature = "physics"))]
    let visible = true;
    #[cfg(feature = "physics")]
    let destination = if visible {
        target.pos
    } else {
        crate::navigation::waypoint(me.pos, target.pos)
    };
    #[cfg(not(feature = "physics"))]
    let destination = target.pos;
    let to_target = if visible {
        aim - eye
    } else {
        destination - me.pos
    };
    let yaw = (-to_target.x).atan2(-to_target.z);
    let horizontal = Vec2::new(to_target.x, to_target.z).length();
    let pitch = if visible {
        to_target.y.atan2(horizontal.max(0.001))
    } else {
        0.0
    };
    inputs.set_look(yaw, pitch);
    inputs.move_z = if !visible || horizontal > 6.0 { 75 } else { 0 };
    inputs.fire = visible;

    inputs
}

/// Alternate sides in deterministic key order; additional players spread along the back line.
pub fn spawn_for_slot(slot: usize) -> (Vec3, f32) {
    let (position, yaw) = crate::arena_data::SPAWNS[slot % 2];
    let rank = slot / 2;
    let offset = if rank == 0 {
        0.0
    } else {
        let distance = rank.div_ceil(2) as f32 * 1.2;
        if rank % 2 == 1 { distance } else { -distance }
    };
    (
        Vec3::from_array(position) + Vec3::new(offset, PLAYER_FLOAT_HEIGHT, 0.0),
        yaw,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The physics world is `#[wincode(skip)]`-ed out of `ShooterGame`, so the
    /// serialized form must be identical whether or not a sim has been built, and
    /// must round-trip cleanly (this is what crosses the wire and sits on-chain).
    #[test]
    fn game_and_state_roundtrip_wincode() {
        let game = ShooterGame::default();
        let bytes = wincode::serialize(&game).unwrap();
        let _restored: ShooterGame = wincode::deserialize(&bytes).unwrap();

        let mut inputs = ShooterInputs::default();
        inputs.set_look(1.234, -0.5);
        inputs.move_z = 100;
        inputs.fire = true;
        let bytes = wincode::serialize(&inputs).unwrap();
        let restored: ShooterInputs = wincode::deserialize(&bytes).unwrap();
        assert_eq!(inputs, restored);

        let mut state = ShooterGameState::default();
        for i in 0..8u8 {
            state.players.insert(
                Pubkey::new_from_array([i; 32]),
                PlayerState {
                    pos: Vec3::new(1.0, 2.0, 3.0),
                    vel: Vec3::ONE,
                    look_xz: Vec2::NEG_Y,
                    pitch: 0.1,
                    score: 3,
                    cooldown: 4,
                    ..Default::default()
                },
            );
        }
        for id in 0..MAX_SHOT_EVENTS as u32 {
            state.shots.insert(
                id,
                Shot {
                    origin: Vec3::ONE,
                    impact: Vec3::splat(5.0),
                    normal: Vec3::Y,
                    owner: Pubkey::new_from_array([1; 32]),
                    hit_geometry: true,
                    hit_player: false,
                    ttl: SHOT_EVENT_TTL,
                },
            );
        }
        let bytes = wincode::serialize(&state).unwrap();
        let restored: ShooterGameState = wincode::deserialize(&bytes).unwrap();
        assert_eq!(restored.players.len(), 8);
        assert_eq!(restored.shots.len(), MAX_SHOT_EVENTS);

        // worst-case-ish state must leave room for the lobby wrappers within
        // MAX_LOBBY_ACCOUNT_BYTES
        assert!(
            (bytes.len() as u64) < ShooterGame::MAX_LOBBY_ACCOUNT_BYTES / 2,
            "state is {} bytes; MAX_LOBBY_ACCOUNT_BYTES needs raising",
            bytes.len()
        );
    }

    /// Look-direction quantization: u16 yaw / i16 pitch must survive well within
    /// visual tolerance (this is the precision every aim command is limited to).
    #[test]
    fn look_quantization_is_tight() {
        for (yaw, pitch) in [
            (0.0_f32, 0.0_f32),
            (1.0, 0.3),
            (-2.5, -1.2),
            (std::f32::consts::TAU - 0.001, 1.5),
        ] {
            let mut inputs = ShooterInputs::default();
            inputs.set_look(yaw, pitch);
            let expected = yaw.rem_euclid(std::f32::consts::TAU);
            assert!((inputs.yaw() - expected).abs() < 1e-3, "yaw {yaw}");
            assert!((inputs.pitch() - pitch).abs() < 1e-3, "pitch {pitch}");
            let dir = inputs.look_dir();
            assert!((dir.length() - 1.0).abs() < 1e-4);
        }
    }
}
