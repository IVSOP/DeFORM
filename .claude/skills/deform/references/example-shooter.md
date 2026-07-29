# Example: 3D shooter (`crates/examples/shooter`)

A first-person arena shooter — bevy + avian3d physics + tnua character controller —
running on DeFORM's rollback netcode at a fixed 60 Hz simulation rate (free to,
because it has no FoC backend — only FoC ties the tick rate to the ER's 20 Hz slot
time). Where pong shows the minimal path, the shooter answers the harder question:
**how do you use a real physics engine inside `advance_frame`?**

## File map

Same shape as pong, plus the sim:

| File | Contents |
| --- | --- |
| `src/shooter_logic.rs` | `ShooterInputs` / `ShooterGameState` / `ShooterGame` + constants + bot |
| `src/physics_sim.rs` | the headless bevy `World` with avian + tnua, stepped once per tick (`physics` feature) |
| `src/client.rs` | 3D scene, FPS camera, input capture, backend wiring |
| `src/menu.rs` | egui lobby menu (pong's, minus the FoC buttons) + scoreboard HUD |
| `src/server.rs`, `src/main.rs`, `src/solana/` | as in pong |
| `run.sh`, `docker/localhost/` | surfpool + QUIC server stack (no ephemeral validator) |

## The pattern: a physics world inside the logic type

`advance_frame` must be a function `(state, inputs) -> state`, but a physics engine
is a big stateful object. The example reconciles them by putting a headless bevy
`World` (avian + tnua plugins, no rendering) inside `ShooterGame` — the
`DeformUserLogic` type — because that is the one object that **survives rollback**.
Every tick:

1. overwrite the world's bodies wholesale from the incoming state (spawn/despawn to
   match, teleport positions, set velocities);
2. run game rules (cooldowns, spawning projectiles from `look_dir()`);
3. step physics exactly once (`TimeUpdateStrategy::ManualDuration` + matching
   `Time<Fixed>` — one `Main`-schedule run == one simulation tick);
4. read bodies back into a fresh `ShooterGameState`; resolve hits from avian's
   `ContactGraph`; expire TTLs.

Because the state overwrite is total, the world's internal caches are never
load-bearing: rollback replays just drive the world through the same overwrite.
Solver warm-starting can make a replayed tick differ by a hair, which surfaces as
an extra smoother-absorbed correction — never a desync, because the authority's
result always wins. This deliberately relaxes the "bit-perfect determinism"
gotcha, and its cost is the FoC ceiling below.

Serialization: the sim field is `#[wincode(skip)]` + `#[serde(skip)]`, and `Clone`
hands back an *empty* sim — a cloned/deserialized `ShooterGame` lazily rebuilds
its world from the next authoritative state, so it is never stale.

Send/Sync trap: bevy's `App` is neither, but `DeformUserLogic` must be both. Build
the `App`, `app.finish(); app.cleanup();`, then `std::mem::take(app.world_mut())`
and own the `World` — run `world.run_schedule(Main)` + `world.clear_trackers()`
yourself.

## The camera/look-input split

No amount of smoothing makes a tick-rate, state-driven first-person camera bearable.
So the camera's orientation lives **only on the client** (`CameraOrientation`,
updated from raw mouse motion every render frame; rollbacks can't touch it), and
each frame it is quantized into the inputs as "I am looking this way":
`yaw_q: u16`, `pitch_q: i16` — integers, because `DeformInputs` requires `Eq`. The
simulation uses the look direction to aim projectiles and orient capsules; the
camera *position* still follows the player's smoothed body.

## Fixed-tick physics-engine gotchas (all hit for real while building this)

- **Teleporting bodies:** write `Transform` *together with* avian's `Position` — a
  fresh entity's default `Transform` (origin) counts as changed and the
  Transform→Position sync silently overwrites your teleport.
- **Stiff springs blow up at low tick rates:** tnua's default
  `spring_strength: 400` is fine at 60 Hz but sits exactly at the stability limit
  for dt = 50 ms (k·dt² = 1) — at 20 Hz, characters bounce ever higher instead of
  settling. Scale spring constants with dt² when retuning the tick rate.
- **Sleeping vs. overwriting:** a sleeping body ignores teleports — put
  `SleepingDisabled` on everything dynamic.
- **Trim avian's default features** (`default-features = false`, `["3d", "f32",
  "parry-f32", "parallel"]`): the mesh/scene/picking integrations panic in a
  headless world ("Message not initialized" for `AssetEvent<Mesh>`).
- Keyed smoothing maps: projectiles live in `HashMap<u32, Projectile>` with a
  monotonically increasing id so `#[smooth(map)]` tracks each sphere across
  spawns/despawns (a `Vec` would re-index and smear them together).
- Meter-scale smoothing params: `#[smooth(decay = 0.8, max_offset = 10.0,
  min_offset_sq = 0.0004)]` — pong's pixel-scale defaults would zero out every
  offset smaller than 2 units.
- Tick-count constants (`PROJECTILE_TTL_TICKS`, `FIRE_COOLDOWN_TICKS`) and any
  test tick loops are denominated in ticks, not seconds — they all change when
  `TICK_RATE_MICROS` does.

## Why there is no FoC backend here

`advance_frame` compiles into the on-chain program, and bevy/avian/tnua cannot
build for SBF. The crate's `physics` feature isolates the sim: the program depends
on `shooter` with `default-features = false, features = ["anchor"]`, where
`advance_frame` returns `ShooterError::PhysicsUnavailable`. Lobbies and settlement
still work on-chain (the anchor program gained a `shooter` feature next to `pong`,
with a `compile_error!` if both are enabled — `state.rs` is still the only
indirection), but only `Network::Web2` lobbies are playable. A fully-on-chain
physics game needs a hand-written, `no_std`-able deterministic sim instead.

## Running it

```sh
anchor_program/build_shooter.sh        # program with --features shooter + codama client
crates/examples/shooter/run.sh         # docker (surfpool + QUIC server) + two clients
```

In each client: Connect (Localhost) → Create/Join Lobby → Ready → Read Lobby →
Play Online (web2). Or skip all of it with "Play Offline (vs bot)" — no
infrastructure at all, and the aim-bot exercises the full sim. WASD moves, mouse
looks, M1 fires, Esc frees the cursor.

The sim has real tests (`cargo test -p shooter --no-default-features --features
physics`): floating, movement, projectile flight/expiry, scoring hits, rebuilding
a sim mid-match, wire-format round-trips, and a per-tick time budget guard for
rollback bursts.
