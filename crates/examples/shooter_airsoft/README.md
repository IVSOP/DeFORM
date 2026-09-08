# Bomb House / Airsoft

A copy of `../shooter`, with its Avian/Tnua movement and DeFORM offline/QUIC backends. The original 46 files were copied byte-for-byte before changes; the original gameplay is unchanged. The examples’ launch scripts now share failure handling.

From the `crates` workspace:

```sh
cargo run -p shooter_airsoft --locked -- run --offline
```

No wallet, Blender, or server is needed for offline play. Omit `--offline` for the inherited lobby menu. WASD moves, mouse looks, Space jumps, left mouse fires, Escape releases the cursor, and F12 saves `airsoft-screenshot.png`. First to ten points wins. Each spawn begins with three seconds of look-only time. The first hit scores one point and starts a five-second round-end phase: the victim becomes an uncontrolled physics capsule retaining its linear velocity, while the survivor can move and shoot without scoring again. Both players then return to their assigned spawns for another three-second countdown. The victim's camera stays at the hit position and tracks the shooter, whose red silhouette remains visible through walls. The camera weapon and crosshair are hidden while dead. The winning tenth point ends the match immediately. Other players have capsule bodies with a held AEG, glove grips, and a team headband: blue for spawn A, orange for spawn B. Headband colors follow the same deterministic player order as spawn assignment. Their weapons follow replicated yaw and pitch, making their aim direction visible.

The map is a 24 × 36 m plywood training house inside a warehouse: rooms and doorways, firing windows, staggered central cover, two flank routes, crates, a raised deck with stairs and rails, roof trusses, skylights, and fluorescent fixtures. Players alternate between the two Blender spawn markers in sorted player-key order; additional players spread along each spawn line. The offline bot follows a ground navigation grid derived from the exported collision geometry.

Shots are immediate 100 m rays from the eye/crosshair. The nearest solid or other player's capsule stops the ray, with a 0.2 s firing cooldown. Shots are evaluated from the same input snapshot before physics advances; replay does not depend on physics contact caches. A bounded history of discrete impact events drives local and spatial remote firing audio, gun recoil, hit feedback, and surface-aligned chipped-plywood decals. There are at most 32 retained shot records and 256 decals. Clients suppress duplicate event IDs across rollback; an effect already presented during prediction is not undone if authority later corrects the shot.

## Blender workflow

The editable source is `blender/bomb_house.blend`; the shipped model is `assets/levels/bomb_house.glb`. This adapts the source/export separation described in `~/Desktop/rust/bevy_blender_test/docs/BLENDER_BEVY_WORKFLOW.md` to a rollback game with a renderless server.

Unlike the Lazarus prototype, this example uses explicit Blender object custom properties and a generated collision manifest instead of Skein runtime components. That lets the headless simulation use exactly the exported static geometry without loading Bevy rendering or a GLB asynchronously. No Skein addon or running registry is needed.

1. Open the `.blend` and edit the **BOMB HOUSE** scene. Save GUI edits before using the background exporter.
2. `airsoft_solid = true` marks collision cuboids. Translation, rotation, and scale are supported. Keep these as unmodified cuboids; use multiple boxes for doorways, stairs, and complex shapes. Decorative meshes have `airsoft_solid = false`.
3. Keep exactly two Empty objects with `airsoft_spawn = 0` and `1`, positioned at floor height. `yaw` is the Bevy Y-axis angle in radians: zero faces −Z, π faces +Z.
4. Export saved edits from this example directory:

```sh
blender --background -noaudio --python-exit-code 1 blender/bomb_house.blend --python blender/export_level.py
python3 scripts/check_level.py
cargo fmt -p shooter_airsoft
```

The exporter saves the `.blend`, exports Y-up GLB with packed PBR textures and punctual lights, and writes both `assets/levels/collision.json` and `src/arena_data.rs`. Rebuild/restart **both clients and server** after geometry or spawn edits: collision is compiled in. Ship the matching `assets` directory with clients. Development asset lookup uses the example's manifest directory; packaged builds can set `AIRSOFT_ASSET_DIR` to an absolute asset directory.

To reconstruct the initial layout (replaces the scene; do not use for exporting hand edits):

```sh
blender --background -noaudio --python-exit-code 1 --python blender/build_level.py --python blender/export_level.py
```

`blender/apply_materials.py` reapplies the packed PBR materials without rebuilding geometry. `scripts/make_effect_assets.py` recreates the original firing WAV and impact PNG (requires Pillow). The asset validator requires NumPy and verifies every exported visual collision bound against the server manifest and generated Rust data, plus spawns, embedded textures, and lights.

## Lighting

Direct lighting uses real-time shadow maps: angled daylight through skylight gaps plus eight imported ceiling spotlights aimed downward. Each ceiling light renders one shadow view instead of the six faces required by a point light. Ambient light, SSAO, TAA, and a small bloom contribution provide the current warehouse look. Imported lamps explicitly enable shadows, so their direct light is blocked by plywood. There is **no Cycles bake or ray-traced lighting** in this version; editing the scene needs only an export. This is a visually tuned prototype, not a calibrated Blender/Bevy lighting match. Static lightmaps can be added using the reference project's indirect-only bake and UV1 workflow.

## Validation

```sh
cargo test -p shooter_airsoft --locked -- --test-threads=1
cargo check -p shooter_airsoft --locked --no-default-features --features server,anchor
cargo clippy -p shooter_airsoft --locked --all-targets --no-deps -- -D warnings
cargo fmt -p shooter_airsoft --check
python3 examples/shooter_airsoft/scripts/check_level.py
cargo run -p shooter_airsoft --locked -- run --smoke-test
```

These commands assume the workspace directory. The rendered smoke test needs a display/GPU, starts offline with an idle opponent, checks GLB import, grounding, shot events and decals, saves `/tmp/airsoft-smoke.png`, and exits. It submits gameplay inputs directly rather than emulating OS mouse clicks. Unit/integration tests cover immediate hits, cooldowns, occlusion, impact normals, event expiry, replay, serialization, spawn-route connectivity, grounding, movement, jumping, and offline visual interpolation.

## Multiplayer

Use this example's `serve` command and copied Docker/run/package scripts with **airsoft clients on both ends**. Its round/death and shot-event wire format differs from earlier Airsoft builds and the original shooter; rebuild both clients and server together, and do not mix clients and servers. Build the lobby program with `anchor_program/build_airsoft_shooter.sh`. It selects the `shooter_airsoft` game feature without physics, then regenerates this example’s Rust client. Web2 lobbies remain `NotStarted` on chain and the admin settlement instruction passes scores separately. The server image has been built and its binary startup checked. Local Surfpool and ephemeral-validator startup and RPC health have also been verified; no live multiplayer match was performed. The default local server port remains 4433; run it separately from the original shooter's server.

The headless server uses the compiled collision data and does not require graphical assets, audio, or Blender. Fully-on-chain physics remains unsupported, as in the original example.

Texture credits and source checksums are in [assets/textures/README.md](assets/textures/README.md). Geometry, gun silhouette, firing audio, and impact texture are original to this example.

The Docker build pins `nightly-2026-04-03` through `RUSTUP_TOOLCHAIN` in all
cargo-chef stages and uses `--locked` for dependency cooking and the final build.
This matches the local compiler used to verify Bevy 0.19.0; the workspace's
floating `nightly` alone does not reproduce a local rustup override in Docker.
The Tnua/Avian adapter enables debug-rendering dependencies transitively, so a
headless server build still compiles `bevy_render`. No display is needed to run it.

Rebuild/start the local stack from the workspace directory:

```sh
./examples/shooter_airsoft/docker/localhost/up.sh
```

The launcher stops immediately if image creation fails; it will not attempt to
pull a nonexistent server image after a compiler error.

The `run.sh` launchers for Pong, Soccer, Shooter, and Airsoft build the server
image before starting either client. Compose also builds its configured services,
with terminal menus/progress disabled so background startup is not suspended by
the terminal. The launchers monitor both clients and Compose;
when any process exits, the launcher preserves its status and terminates the
remaining process groups, including the game processes started by Cargo.
The `up.sh` and `build-image.sh` wrappers work from any current directory and
propagate build failures. The existing devnet launchers already stop on errors.

The round flow is authoritative (`SpawnFreeze` → `Playing` → `RoundOver`) with tick timers, following Soccer's phase/reset pattern. Simultaneous lethal shots resolve in deterministic player-key order and award only one point for the round. Corpse orientation, angular velocity, and the fixed death-camera origin are serialized for rollback. Bodies are single physics capsules with attached weapons and headbands.

Run the death/respawn rendering fixture with:

```sh
AIRSOFT_ROUND_SMOKE=1 cargo run -p shooter_airsoft --locked -- run --smoke-test
```

It captures the death camera, a killer occluded by verified solid geometry, the fallen body, and the respawn countdown under `/tmp/airsoft-*.png`.

The egui menu and scoreboard display smoothed FPS. To compare the ceiling-light rendering cost with the original point-light setup:

```sh
AIRSOFT_PERF_PROBE=1 cargo run -p shooter_airsoft --release --locked -- run --offline
```

This optional probe runs a stationary, bot-disabled scene, temporarily disables VSync, and compares the current spotlights with the original point lights. Each stage warms up for four seconds, measures six seconds, and logs FPS, frame times, physical framebuffer resolution and GPU pass timings before exiting. Normal play does not enable these diagnostics. `run.sh` launches two clients, which share the GPU; its performance is not directly comparable with a single-client probe.
