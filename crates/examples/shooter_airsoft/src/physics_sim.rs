//! A headless bevy `World` running avian3d + tnua, driven one fixed
//! [`TICK_RATE_MICROS`] step at a time from `ShooterGame::advance_frame`.
//!
//! How this coexists with rollback netcode: the world is **not** the source of
//! truth — `ShooterGameState` is. Every step starts by overwriting the world's
//! bodies (positions, velocities, spawns, despawns) from the authoritative state,
//! then steps physics once, then reads the results back out into a fresh state.
//! When DeFORM rolls back and replays ticks, `advance_frame` is simply called with
//! older states and the same overwrite makes the world follow along. Solver caches
//! (warm starting, contact history) survive across calls, which can make replayed
//! ticks differ from the original by a hair — that only shows up as an extra
//! rollback correction absorbed by the smoother, never as a desync, because the
//! authority's result always wins.
//!
//! Why this is a separate module behind the `physics` feature: `advance_frame`
//! also compiles into the on-chain program for fully-on-chain games, and bevy,
//! avian and tnua cannot build for SBF. This example therefore supports the
//! offline and web2 (QUIC) backends only — the anchor program still manages
//! lobbies and settles scores, but a `FullyOnChain` lobby would have no way to
//! tick. Making a physics game fully on-chain means writing a deterministic,
//! `no_std` simulation by hand instead of this module.

use std::{collections::BTreeMap, time::Duration};

use avian3d::prelude::*;
use bevy::{prelude::*, time::TimeUpdateStrategy};
use bevy_tnua::{
    builtins::{TnuaBuiltinJump, TnuaBuiltinJumpConfig, TnuaBuiltinWalkConfig},
    prelude::*,
};
use bevy_tnua_avian3d::{TnuaAvian3dPlugin, TnuaAvian3dSensorShape};
use deform_core::Pubkey;
use glam::{Vec2, Vec3};

use crate::shooter_logic::*;

/// Walking is the basis; jumping is the one action, fed while the jump input is
/// held (see [`ShooterInputs::jump`]).
#[derive(TnuaScheme)]
#[scheme(basis = TnuaBuiltinWalk)]
pub enum ShooterScheme {
    Jump(TnuaBuiltinJump),
}

/// Debug markers; the sim's own bookkeeping goes through the entity maps.
#[derive(Component)]
#[allow(dead_code)]
struct PlayerBody(Pubkey);

/// The lazily-built world. `Default` (and `Clone`, and deserialization — the field
/// is `#[wincode(skip)]`/`#[serde(skip)]` in [`ShooterGame`]) all produce an empty
/// sim; the world is rebuilt from the next authoritative state that comes through
/// `step`, so a clone is never stale.
#[derive(Default)]
pub struct PhysicsSim {
    world: Option<SimWorld>,
}

impl Clone for PhysicsSim {
    fn clone(&self) -> Self {
        Self::default()
    }
}

impl std::fmt::Debug for PhysicsSim {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PhysicsSim")
            .field("built", &self.world.is_some())
            .finish()
    }
}

// `DeformUserLogic` requires Send + Sync; fail at compile time (not deep inside a
// backend's trait bounds) if bevy's App ever stops satisfying that.
const _: fn() = || {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<PhysicsSim>();
};

impl PhysicsSim {
    /// Advance the simulation by exactly one tick ([`TICK_RATE_MICROS`]).
    pub fn step(
        &mut self,
        state: &ShooterGameState,
        inputs: &BTreeMap<Pubkey, ShooterInputs>,
    ) -> ShooterGameState {
        if state.players.values().any(|p| p.score >= WIN_SCORE) {
            return state.clone();
        }
        let sim = self.world.get_or_insert_with(SimWorld::new);
        let mut next = state.clone();

        next.shots.retain(|_, shot| {
            shot.ttl = shot.ttl.saturating_sub(1);
            shot.ttl > 0
        });
        for ps in next.players.values_mut() {
            ps.cooldown = ps.cooldown.saturating_sub(1);
        }
        for (pk, input) in inputs {
            if let Some(ps) = next.players.get_mut(pk) {
                let look = input.look_dir();
                let horizontal = Vec2::new(look.x, look.z);
                if horizontal.length_squared() > 1e-6 {
                    ps.look_xz = horizontal.normalize();
                }
                ps.pitch = input.pitch();
            }
        }
        // Rays use the input snapshot directly, independent of physics solver caches.
        // All players shoot from the same snapshot, so simultaneous hits are fair.
        let snapshot = next.clone();
        for (pk, input) in inputs {
            let Some(ps) = snapshot.players.get(pk) else {
                continue;
            };
            if !input.fire || ps.cooldown != 0 {
                continue;
            }
            next.players.get_mut(pk).unwrap().cooldown = FIRE_COOLDOWN_TICKS;
            let origin = ps.pos + Vec3::Y * PLAYER_EYE_HEIGHT;
            let shot = sim.trace_shot(&snapshot, *pk, origin, input.look_dir());
            if shot.hit_player {
                next.players.get_mut(pk).unwrap().score += 1;
            }
            let id = next.next_shot_id;
            next.next_shot_id = id.wrapping_add(1);
            if next.shots.len() >= MAX_SHOT_EVENTS {
                // Lowest TTL is the oldest even when the u32 sequence wraps.
                if let Some(oldest) = next
                    .shots
                    .iter()
                    .min_by_key(|(id, s)| (s.ttl, **id))
                    .map(|(id, _)| *id)
                {
                    next.shots.remove(&oldest);
                }
            }
            next.shots.insert(id, shot);
        }
        sim.sync_from_state(&next);
        sim.feed_controllers(&next, inputs);
        sim.update();
        sim.extract_players(&mut next);

        next
    }
}

struct SimWorld {
    /// The `World` is taken *out* of the `App` after plugin setup: `App` itself is
    /// neither `Send` nor `Sync` (its runner closure isn't), but a plain `World`
    /// is both — and `DeformUserLogic` implementors must be.
    world: World,
    tnua_config: Handle<ShooterSchemeConfig>,
    players: BTreeMap<Pubkey, Entity>,
    solids: Vec<(Collider, Vec3, Quat)>,
}

impl SimWorld {
    fn new() -> Self {
        let mut app = App::new();
        app.add_plugins((
            MinimalPlugins,
            bevy::asset::AssetPlugin::default(),
            bevy::transform::TransformPlugin,
            PhysicsPlugins::default(),
            TnuaControllerPlugin::<ShooterScheme>::new(PhysicsSchedule),
            TnuaAvian3dPlugin::new(PhysicsSchedule),
        ));

        // One update == one simulation tick: the clock advances by exactly one tick
        // per update, and the fixed timestep matches, so FixedMain (and avian in
        // FixedPostUpdate) runs exactly once.
        app.insert_resource(Time::<Fixed>::from_hz(
            1_000_000.0 / TICK_RATE_MICROS as f64,
        ));
        app.insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_micros(
            TICK_RATE_MICROS,
        )));

        // Run any deferred plugin setup, then take ownership of the world; from
        // here on the sim drives the `Main` schedule directly.
        app.finish();
        app.cleanup();
        let mut world = std::mem::take(app.world_mut());

        let tnua_config =
            world
                .resource_mut::<Assets<ShooterSchemeConfig>>()
                .add(ShooterSchemeConfig {
                    basis: TnuaBuiltinWalkConfig {
                        speed: PLAYER_SPEED,
                        float_height: PLAYER_FLOAT_HEIGHT,
                        // The default spring (400) is fine at 60 Hz, but beware if you
                        // lower the tick rate: at 20 Hz it sits exactly on the
                        // stability limit (k · dt² = 1) and characters bounce ever
                        // higher instead of settling. Scale it with dt² if you retune.
                        ..Default::default()
                    },
                    jump: TnuaBuiltinJumpConfig {
                        // the default height is 0.0 — no jump at all
                        height: PLAYER_JUMP_HEIGHT,
                        ..Default::default()
                    },
                });

        let solids: Vec<_> = crate::arena_data::SOLIDS
            .iter()
            .map(|(center, size, rotation)| {
                let collider = Collider::cuboid(size[0], size[1], size[2]);
                let pos = Vec3::from_array(*center);
                let rot = Quat::from_array(*rotation).normalize();
                world.spawn((
                    RigidBody::Static,
                    collider.clone(),
                    Position(pos),
                    Rotation(rot),
                    Transform::from_translation(pos).with_rotation(rot),
                ));
                (collider, pos, rot)
            })
            .collect();

        let mut sim = SimWorld {
            world,
            tnua_config,
            players: BTreeMap::new(),
            solids,
        };
        // Warmup update: bevy's very first update has a zero delta, which would
        // otherwise swallow the first tick's physics step.
        sim.update();
        sim
    }

    /// What `App::update` does, minus the `App`: run the `Main` schedule once and
    /// clear change trackers.
    fn update(&mut self) {
        self.world.run_schedule(Main);
        self.world.clear_trackers();
    }

    /// Make the world's bodies match `state` exactly: spawn what's missing (in
    /// deterministic key order), despawn what's gone, teleport the rest.
    fn sync_from_state(&mut self, state: &ShooterGameState) {
        let world = &mut self.world;

        // players (never added mid-match, but removal-safety costs nothing)
        self.players.retain(|pk, entity| {
            if state.players.contains_key(pk) {
                true
            } else {
                world.despawn(*entity);
                false
            }
        });
        let mut player_keys: Vec<&Pubkey> = state.players.keys().collect();
        player_keys.sort();
        for pk in player_keys {
            let ps = &state.players[pk];
            let yaw = (-ps.look_xz.x).atan2(-ps.look_xz.y);
            let entity = *self.players.entry(*pk).or_insert_with(|| {
                world
                    .spawn((
                        PlayerBody(*pk),
                        RigidBody::Dynamic,
                        SleepingDisabled,
                        Collider::capsule(PLAYER_RADIUS, PLAYER_CAPSULE_LENGTH),
                        TnuaController::<ShooterScheme>::default(),
                        TnuaConfig::<ShooterScheme>(self.tnua_config.clone()),
                        // slightly slimmer than the capsule so wall contact doesn't
                        // read as ground
                        TnuaAvian3dSensorShape(Collider::cylinder(PLAYER_RADIUS * 0.95, 0.0)),
                        // tnua keeps the character upright and yaws it toward
                        // desired_forward; only roll/pitch are locked
                        LockedAxes::new().lock_rotation_x().lock_rotation_z(),
                    ))
                    .id()
            });
            // Write Transform alongside Position: a freshly spawned entity has a
            // default Transform at the origin, and avian's Transform->Position sync
            // would otherwise overwrite the teleport with (0,0,0) on its first tick.
            let rot = Quat::from_rotation_y(yaw);
            let mut e = world.entity_mut(entity);
            e.insert((
                Position::from(ps.pos),
                Rotation::from(rot),
                LinearVelocity(ps.vel),
                AngularVelocity(Vec3::ZERO),
                Transform::from_translation(ps.pos).with_rotation(rot),
            ));
        }
    }

    /// Tnua controllers must be fed every tick they should keep moving.
    fn feed_controllers(
        &mut self,
        state: &ShooterGameState,
        inputs: &BTreeMap<Pubkey, ShooterInputs>,
    ) {
        let world = &mut self.world;
        for (pk, entity) in self.players.iter() {
            let Some(mut controller) = world.get_mut::<TnuaController<ShooterScheme>>(*entity)
            else {
                continue;
            };
            let (desired_motion, desired_forward, jump) = match inputs.get(pk) {
                Some(input) => {
                    let look = input.look_dir();
                    (
                        input.move_dir(),
                        Dir3::new(Vec3::new(look.x, 0.0, look.z)).ok(),
                        input.jump,
                    )
                }
                None => {
                    let ps = &state.players[pk];
                    (
                        Vec3::ZERO,
                        Dir3::new(Vec3::new(ps.look_xz.x, 0.0, ps.look_xz.y)).ok(),
                        false,
                    )
                }
            };
            controller.basis = TnuaBuiltinWalk {
                desired_motion,
                desired_forward,
            };
            // Actions are pull-fed: call this every tick, then feed the jump for as
            // long as the input holds it. Tnua turns that into hold-to-jump-higher
            // and refuses mid-air starts (no double jumps).
            controller.initiate_action_feeding();
            if jump {
                controller.action(ShooterScheme::Jump(TnuaBuiltinJump::default()));
            }
        }
    }

    fn extract_players(&mut self, next: &mut ShooterGameState) {
        let world = &mut self.world;
        for (pk, entity) in self.players.iter() {
            let Some(ps) = next.players.get_mut(pk) else {
                continue;
            };
            if let Some(position) = world.get::<Position>(*entity) {
                ps.pos = position.0;
            }
            if let Some(velocity) = world.get::<LinearVelocity>(*entity) {
                ps.vel = velocity.0;
            }
        }
    }

    fn trace_shot(
        &self,
        state: &ShooterGameState,
        owner: Pubkey,
        origin: Vec3,
        direction: Vec3,
    ) -> Shot {
        let mut distance = SHOT_RANGE;
        let mut normal = Vec3::ZERO;
        let mut hit_geometry = false;
        let mut hit_player = false;
        for (collider, pos, rot) in &self.solids {
            if let Some((d, n)) = collider.cast_ray(*pos, *rot, origin, direction, distance, true)
                && d < distance
            {
                distance = d;
                normal = n;
                hit_geometry = true;
            }
        }
        let capsule = Collider::capsule(PLAYER_RADIUS, PLAYER_CAPSULE_LENGTH);
        let mut players: Vec<_> = state.players.iter().collect();
        players.sort_by_key(|(pk, _)| **pk);
        for (pk, ps) in players {
            if *pk == owner {
                continue;
            }
            if let Some((d, n)) =
                capsule.cast_ray(ps.pos, Quat::IDENTITY, origin, direction, distance, true)
            {
                // Geometry wins a tie; shots never penetrate a touching wall.
                if d < distance {
                    distance = d;
                    normal = n;
                    hit_geometry = false;
                    hit_player = true;
                }
            }
        }
        Shot {
            origin,
            impact: origin + direction * distance,
            normal,
            owner,
            hit_geometry,
            hit_player,
            ttl: SHOT_EVENT_TTL,
        }
    }
}

#[cfg(test)]
mod tests {
    use deform_core::{
        DeformUserLogic,
        accounts::lobby::{
            LobbyMetadata, Network, PlayerStatus, Web2Server, not_started::LobbyNotStarted,
        },
    };

    use super::*;

    fn two_player_setup() -> (ShooterGame, ShooterGameState, Pubkey, Pubkey) {
        let a = Pubkey::new_from_array([1; 32]);
        let b = Pubkey::new_from_array([2; 32]);
        let mut player_status = BTreeMap::new();
        player_status.insert(a, PlayerStatus::Ready);
        player_status.insert(b, PlayerStatus::Ready);
        let metadata = LobbyMetadata {
            id: 0,
            creator: a,
            network: Network::Web2(Web2Server::Localhost),
            bump: 0,
        };
        let not_started = LobbyNotStarted { player_status };
        let game = ShooterGame::new_from_lobby(&metadata, &not_started).unwrap();
        let state = ShooterGame::new_game_from_lobby(&metadata, &not_started).unwrap();
        (game, state, a, b)
    }

    fn idle_inputs(a: Pubkey, b: Pubkey) -> BTreeMap<Pubkey, ShooterInputs> {
        let mut inputs = BTreeMap::new();
        inputs.insert(a, ShooterInputs::default());
        inputs.insert(b, ShooterInputs::default());
        inputs
    }

    /// Players must settle at the tnua float height and stay inside the arena.
    #[test]
    fn players_float_and_stay_put() {
        let (mut game, mut state, a, b) = two_player_setup();
        let inputs = idle_inputs(a, b);
        for _ in 0..120 {
            state = game.advance_frame(&state, &inputs).unwrap();
        }
        for ps in state.players.values() {
            assert!(
                (ps.pos.y - PLAYER_FLOAT_HEIGHT).abs() < 0.3,
                "player should float near {PLAYER_FLOAT_HEIGHT}, got {}",
                ps.pos.y
            );
            assert!(ps.pos.x.abs() < ARENA_HALF_X && ps.pos.z.abs() < ARENA_HALF_Z);
        }
    }

    /// WASD input actually moves the capsule in the looked-at direction.
    #[test]
    fn movement_follows_look_direction() {
        let (mut game, mut state, a, b) = two_player_setup();
        state.players.get_mut(&a).unwrap().pos.x = 10.0;
        // let them settle on the floor first
        let idle = idle_inputs(a, b);
        for _ in 0..60 {
            state = game.advance_frame(&state, &idle).unwrap();
        }
        let start = state.players[&a].pos;

        let mut inputs = idle_inputs(a, b);
        let fwd = inputs.get_mut(&a).unwrap();
        fwd.move_z = 100;
        fwd.set_look(0.0, 0.0); // looking -Z
        for _ in 0..60 {
            state = game.advance_frame(&state, &inputs).unwrap();
        }
        let moved = state.players[&a].pos - start;
        assert!(
            moved.z < -3.5,
            "1s of forward input should move several meters along -Z, moved {moved:?}"
        );
        assert!(
            moved.x.abs() < 0.5,
            "no sideways drift expected, moved {moved:?}"
        );
    }

    /// Holding jump: one full-height jump, landing back on the float height, and
    /// no re-jump while the button stays held (tnua's ground-only jump action).
    #[test]
    fn jump_rises_lands_and_does_not_repeat() {
        let (mut game, mut state, a, b) = two_player_setup();
        let idle = idle_inputs(a, b);
        for _ in 0..60 {
            state = game.advance_frame(&state, &idle).unwrap();
        }

        let mut jumping = idle_inputs(a, b);
        jumping.get_mut(&a).unwrap().jump = true;

        // Rise phase: within half a second the capsule should be well above the
        // float height.
        let mut peak = f32::MIN;
        for _ in 0..30 {
            state = game.advance_frame(&state, &jumping).unwrap();
            peak = peak.max(state.players[&a].pos.y);
        }
        assert!(
            peak > PLAYER_FLOAT_HEIGHT + PLAYER_JUMP_HEIGHT * 0.5,
            "holding jump should lift the capsule, peaked at {peak}"
        );

        // Keep holding for 3 more seconds: the character must land back at the
        // float height and stay there — a held button must not chain jumps.
        for _ in 0..120 {
            state = game.advance_frame(&state, &jumping).unwrap();
        }
        let mut still_peak = f32::MIN;
        for _ in 0..60 {
            state = game.advance_frame(&state, &jumping).unwrap();
            still_peak = still_peak.max(state.players[&a].pos.y);
        }
        assert!(
            (still_peak - PLAYER_FLOAT_HEIGHT).abs() < 0.3,
            "held jump must not re-trigger after landing, but y reached {still_peak}"
        );
    }

    #[test]
    fn hitscan_scores_immediately_and_obeys_cooldown() {
        let (mut game, mut state, a, b) = two_player_setup();
        state.players.get_mut(&a).unwrap().pos = Vec3::new(0.0, PLAYER_FLOAT_HEIGHT, 14.4);
        state.players.get_mut(&b).unwrap().pos = Vec3::new(3.0, PLAYER_FLOAT_HEIGHT, 14.4);
        let mut inputs = idle_inputs(a, b);
        inputs.get_mut(&a).unwrap().fire = true;
        inputs
            .get_mut(&a)
            .unwrap()
            .set_look(-std::f32::consts::FRAC_PI_2, 0.0);
        let snapshot = state.clone();
        state = game.advance_frame(&state, &inputs).unwrap();
        assert_eq!(state.players[&a].score, 1, "same-tick hit");
        assert_eq!(state.shots.len(), 1);
        assert!(state.shots[&0].hit_player);
        let replay = game.advance_frame(&snapshot, &inputs).unwrap();
        assert_eq!(replay.players[&a].score, 1);
        assert_eq!(replay.shots[&0].impact, state.shots[&0].impact);
        for _ in 0..FIRE_COOLDOWN_TICKS - 1 {
            state = game.advance_frame(&state, &inputs).unwrap();
        }
        assert_eq!(state.players[&a].score, 1);
        state = game.advance_frame(&state, &inputs).unwrap();
        assert_eq!(state.players[&a].score, 2);
    }

    #[test]
    fn plywood_blocks_shots_and_preserves_surface_normal() {
        let sim = SimWorld::new();
        let (_, mut state, a, b) = two_player_setup();
        state.players.get_mut(&a).unwrap().pos = Vec3::new(-0.9, PLAYER_FLOAT_HEIGHT, 14.4);
        state.players.get_mut(&b).unwrap().pos = Vec3::new(-0.9, PLAYER_FLOAT_HEIGHT, 10.5);
        let origin = state.players[&a].pos + Vec3::Y * PLAYER_EYE_HEIGHT;
        let shot = sim.trace_shot(&state, a, origin, Vec3::NEG_Z);
        assert!(shot.hit_geometry && !shot.hit_player);
        assert!((11.85..11.94).contains(&shot.impact.z), "{:?}", shot.impact);
        assert!(shot.normal.dot(Vec3::Z) > 0.99);
        let miss = sim.trace_shot(&state, a, Vec3::new(30.0, 20.0, 0.0), Vec3::Y);
        assert!(!miss.hit_geometry && !miss.hit_player);
        assert_eq!(miss.origin.distance(miss.impact), SHOT_RANGE);
    }

    #[test]
    fn shots_expire_and_do_not_move() {
        let (mut game, mut state, a, b) = two_player_setup();
        let mut inputs = idle_inputs(a, b);
        inputs.get_mut(&a).unwrap().fire = true;
        state = game.advance_frame(&state, &inputs).unwrap();
        let impact = state.shots[&0].impact;
        inputs.get_mut(&a).unwrap().fire = false;
        for _ in 1..SHOT_EVENT_TTL {
            state = game.advance_frame(&state, &inputs).unwrap();
            assert_eq!(state.shots[&0].impact, impact);
        }
        state = game.advance_frame(&state, &inputs).unwrap();
        assert!(state.shots.is_empty());
    }

    #[test]
    fn player_can_climb_to_the_raised_deck() {
        let (mut game, mut state, a, b) = two_player_setup();
        state.players.get_mut(&a).unwrap().pos = Vec3::new(-4.8, PLAYER_FLOAT_HEIGHT, 7.8);
        let mut inputs = idle_inputs(a, b);
        inputs.get_mut(&a).unwrap().move_z = 65;
        for _ in 0..360 {
            state = game.advance_frame(&state, &inputs).unwrap();
            let pos = state.players[&a].pos;
            if pos.z < 2.6 && pos.y > 3.7 {
                return;
            }
        }
        let pos = state.players[&a].pos;
        assert!(
            pos.z < 2.6 && pos.y > 3.7,
            "must reach the elevated platform, got {pos:?}"
        );
    }

    /// A fresh sim (as after a clone or deserialize) picks up mid-match state.
    #[test]
    fn rebuilt_sim_continues_from_state() {
        let (mut game, mut state, a, b) = two_player_setup();
        let idle = idle_inputs(a, b);
        for _ in 0..60 {
            state = game.advance_frame(&state, &idle).unwrap();
        }
        state.players.get_mut(&a).unwrap().score = 3;

        // clone = empty sim, same rules
        let mut rebuilt = game.clone();
        let continued = rebuilt.advance_frame(&state, &idle).unwrap();
        assert_eq!(continued.players[&a].score, 3);
        assert!((continued.players[&a].pos.y - PLAYER_FLOAT_HEIGHT).abs() < 0.5);
    }

    /// Not a benchmark, just a guard against the sim being pathologically slow:
    /// a rollback replays several ticks inside one render frame.
    #[test]
    fn step_is_fast_enough_for_rollback_bursts() {
        let (mut game, mut state, a, b) = two_player_setup();
        let mut inputs = idle_inputs(a, b);
        inputs.get_mut(&a).unwrap().fire = true;
        inputs.get_mut(&a).unwrap().move_z = 100;

        // warm up (includes world build)
        for _ in 0..10 {
            state = game.advance_frame(&state, &inputs).unwrap();
        }
        let start = std::time::Instant::now();
        for _ in 0..100 {
            state = game.advance_frame(&state, &inputs).unwrap();
        }
        let per_tick = start.elapsed() / 100;
        // Loose bound: this is a debug build and the other tests' sims run in
        // parallel on the same compute pool. Solo, a tick is well under 1 ms.
        assert!(
            per_tick < std::time::Duration::from_millis(5),
            "one tick took {per_tick:?}; a rollback burst replays dozens of ticks in one frame"
        );
    }
}
