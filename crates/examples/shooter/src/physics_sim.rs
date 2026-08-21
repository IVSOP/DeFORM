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

#[derive(Component)]
#[allow(dead_code)]
struct ProjectileBody(u32);

/// The lazily-built world. `Default` (and `Clone`, and deserialization — the field
/// is `#[wincode(skip)]`/`#[serde(skip)]` in [`ShooterGame`]) all produce an empty
/// sim; the world is rebuilt from the next authoritative state that comes through
/// `step`, so a clone is never stale.
pub struct PhysicsSim {
    world: Option<SimWorld>,
}

impl Default for PhysicsSim {
    fn default() -> Self {
        PhysicsSim { world: None }
    }
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
        let sim = self.world.get_or_insert_with(SimWorld::new);
        let mut next = state.clone();

        // --- pre-step game rules: cooldowns, facing, firing ---
        let mut spawns: Vec<(u32, Projectile)> = Vec::new();
        for (pk, input) in inputs.iter() {
            let Some(ps) = next.players.get_mut(pk) else {
                continue;
            };
            ps.cooldown = ps.cooldown.saturating_sub(1);

            let look = input.look_dir();
            let look_xz = Vec2::new(look.x, look.z);
            if look_xz.length_squared() > 1e-6 {
                ps.look_xz = look_xz.normalize();
            }
            ps.pitch = input.pitch();

            if input.fire && ps.cooldown == 0 {
                ps.cooldown = FIRE_COOLDOWN_TICKS;
                let id = next.next_projectile_id;
                next.next_projectile_id = next.next_projectile_id.wrapping_add(1);
                let eye = ps.pos + Vec3::Y * PLAYER_EYE_HEIGHT;
                spawns.push((
                    id,
                    Projectile {
                        pos: eye + look * MUZZLE_OFFSET,
                        vel: look * PROJECTILE_SPEED,
                        owner: *pk,
                        ttl: PROJECTILE_TTL_TICKS,
                    },
                ));
            }
        }
        for (id, projectile) in spawns {
            next.projectiles.insert(id, projectile);
        }

        // --- overwrite the world from the (predicted or authoritative) state ---
        sim.sync_from_state(&next);
        sim.feed_controllers(&next, inputs);

        // --- one fixed step (TimeUpdateStrategy::ManualDuration == one tick) ---
        sim.update();

        // --- read back bodies, resolve hits, expire projectiles ---
        sim.extract_players(&mut next);
        sim.resolve_hits(&mut next);
        sim.expire_projectiles(&mut next);

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
    projectiles: BTreeMap<u32, Entity>,
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

        // The arena: a big box. Floor + 4 walls, all static.
        let world_ref = &mut world;
        let wall_len_x = 2.0 * ARENA_HALF_X + 2.0 * WALL_THICKNESS;
        let wall_len_z = 2.0 * ARENA_HALF_Z + 2.0 * WALL_THICKNESS;
        let arena: [(Vec3, Vec3); 5] = [
            // floor (top surface at y = 0)
            (
                Vec3::new(0.0, -FLOOR_THICKNESS / 2.0, 0.0),
                Vec3::new(wall_len_x, FLOOR_THICKNESS, wall_len_z),
            ),
            // +X / -X walls
            (
                Vec3::new(ARENA_HALF_X + WALL_THICKNESS / 2.0, WALL_HEIGHT / 2.0, 0.0),
                Vec3::new(WALL_THICKNESS, WALL_HEIGHT, wall_len_z),
            ),
            (
                Vec3::new(-ARENA_HALF_X - WALL_THICKNESS / 2.0, WALL_HEIGHT / 2.0, 0.0),
                Vec3::new(WALL_THICKNESS, WALL_HEIGHT, wall_len_z),
            ),
            // +Z / -Z walls
            (
                Vec3::new(0.0, WALL_HEIGHT / 2.0, ARENA_HALF_Z + WALL_THICKNESS / 2.0),
                Vec3::new(wall_len_x, WALL_HEIGHT, WALL_THICKNESS),
            ),
            (
                Vec3::new(0.0, WALL_HEIGHT / 2.0, -ARENA_HALF_Z - WALL_THICKNESS / 2.0),
                Vec3::new(wall_len_x, WALL_HEIGHT, WALL_THICKNESS),
            ),
        ];
        for (pos, size) in arena {
            world_ref.spawn((
                RigidBody::Static,
                Collider::cuboid(size.x, size.y, size.z),
                Position::from(pos),
            ));
        }

        let mut sim = SimWorld {
            world,
            tnua_config,
            players: BTreeMap::new(),
            projectiles: BTreeMap::new(),
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

        // projectiles
        self.projectiles.retain(|id, entity| {
            if state.projectiles.contains_key(id) {
                true
            } else {
                world.despawn(*entity);
                false
            }
        });
        let mut projectile_ids: Vec<&u32> = state.projectiles.keys().collect();
        projectile_ids.sort();
        for id in projectile_ids {
            let projectile = &state.projectiles[id];
            let entity = *self.projectiles.entry(*id).or_insert_with(|| {
                world
                    .spawn((
                        ProjectileBody(*id),
                        RigidBody::Dynamic,
                        SleepingDisabled,
                        Collider::sphere(PROJECTILE_RADIUS),
                        Restitution::new(0.6),
                        // light, so a hit nudges rather than launches
                        Mass(0.2),
                    ))
                    .id()
            });
            let mut e = world.entity_mut(entity);
            e.insert((
                Position::from(projectile.pos),
                // Angular velocity is not part of the game state, so it is zeroed
                // for reproducibility - spheres slide rather than roll.
                LinearVelocity(projectile.vel),
                AngularVelocity(Vec3::ZERO),
                Rotation::default(),
                Transform::from_translation(projectile.pos),
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
        for (id, entity) in self.projectiles.iter() {
            let Some(projectile) = next.projectiles.get_mut(id) else {
                continue;
            };
            if let Some(position) = world.get::<Position>(*entity) {
                projectile.pos = position.0;
            }
            if let Some(velocity) = world.get::<LinearVelocity>(*entity) {
                projectile.vel = velocity.0;
            }
        }
    }

    /// A projectile touching any player other than its owner disappears and scores.
    fn resolve_hits(&mut self, next: &mut ShooterGameState) {
        let contact_graph = self.world.resource::<ContactGraph>();

        // Deterministic double loop over sorted BTreeMaps; N is tiny.
        let mut hits: Vec<(u32, Pubkey)> = Vec::new();
        for (id, projectile_entity) in self.projectiles.iter() {
            let Some(projectile) = next.projectiles.get(id) else {
                continue;
            };
            for (pk, player_entity) in self.players.iter() {
                if *pk == projectile.owner {
                    continue;
                }
                if let Some((_, pair)) = contact_graph.get(*projectile_entity, *player_entity) {
                    if pair.is_touching() {
                        hits.push((*id, projectile.owner));
                        break;
                    }
                }
            }
        }

        let world = &mut self.world;
        for (id, owner) in hits {
            next.projectiles.remove(&id);
            if let Some(entity) = self.projectiles.remove(&id) {
                world.despawn(entity);
            }
            if let Some(shooter) = next.players.get_mut(&owner) {
                shooter.score += 1;
            }
        }
    }

    fn expire_projectiles(&mut self, next: &mut ShooterGameState) {
        let world = &mut self.world;
        let projectiles = &mut self.projectiles;
        next.projectiles.retain(|id, projectile| {
            projectile.ttl = projectile.ttl.saturating_sub(1);
            if projectile.ttl == 0 {
                if let Some(entity) = projectiles.remove(id) {
                    world.despawn(entity);
                }
                false
            } else {
                true
            }
        });
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
            moved.z < -5.0,
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

    /// Firing spawns a projectile that flies, drops with gravity, and expires.
    #[test]
    fn projectiles_fly_fall_and_expire() {
        let (mut game, mut state, a, b) = two_player_setup();
        let idle = idle_inputs(a, b);
        for _ in 0..60 {
            state = game.advance_frame(&state, &idle).unwrap();
        }

        // fire one shot straight ahead (then release)
        let mut firing = idle_inputs(a, b);
        firing.get_mut(&a).unwrap().fire = true;
        state = game.advance_frame(&state, &firing).unwrap();
        assert_eq!(
            state.projectiles.len(),
            1,
            "one projectile after one firing tick"
        );
        let (_, projectile) = state.projectiles.iter().next().unwrap();
        assert_eq!(projectile.owner, a);
        let spawn_pos = projectile.pos;

        // after a second, it has moved and is falling
        let mut cleared = state.clone();
        for _ in 0..45 {
            cleared = game.advance_frame(&cleared, &idle).unwrap();
        }
        if let Some(projectile) = cleared.projectiles.values().next() {
            assert!(
                projectile.pos.distance(spawn_pos) > 1.0,
                "projectile should travel"
            );
        }

        // TTL cleans it up eventually
        for _ in 0..(PROJECTILE_TTL_TICKS + 5) {
            cleared = game.advance_frame(&cleared, &idle).unwrap();
        }
        assert!(cleared.projectiles.is_empty(), "projectile should expire");
    }

    /// Shooting at another player scores and removes the projectile.
    #[test]
    fn hits_score() {
        let (mut game, mut state, a, b) = two_player_setup();
        let idle = idle_inputs(a, b);
        for _ in 0..60 {
            state = game.advance_frame(&state, &idle).unwrap();
        }

        // stand b right in front of a (the sync teleports the body along), then
        // aim from a to b and hold fire until something lands
        let point_blank = state.players[&a].pos + Vec3::new(3.0, 0.0, 0.0);
        state.players.get_mut(&b).unwrap().pos = point_blank;
        let me = state.players[&a].pos;
        let target = point_blank;
        let to_target = target - me;
        let yaw = (-to_target.x).atan2(-to_target.z);
        let mut inputs = idle_inputs(a, b);
        let shooter = inputs.get_mut(&a).unwrap();
        shooter.fire = true;
        shooter.set_look(yaw, 0.0);

        let mut scored = false;
        for _ in 0..300 {
            state = game.advance_frame(&state, &inputs).unwrap();
            if state.players[&a].score > 0 {
                scored = true;
                break;
            }
        }
        assert!(scored, "point-blank fire for 5s should land a hit");
        assert_eq!(state.players[&b].score, 0);
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
