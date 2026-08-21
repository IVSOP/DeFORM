---
name: deform
description: Build realtime multiplayer games with DeFORM (Deterministic Fully-Onchain Realtime Multiplayer) — rollback netcode in Rust with pluggable backends (offline, QUIC web2 server, fully-on-chain via MagicBlock ephemeral rollups) and a Solana/Anchor program template. Use when defining game state/inputs/logic types, wiring a DeformClient, writing a QUIC server, configuring interpolation, or deploying the on-chain game program.
---

# DeFORM

**De**centralized **F**ully-**O**nchain **R**ealtime **M**ultiplayer. A Rust library for
lockstep-with-prediction (rollback) multiplayer where the authoritative simulation can live
either on a normal server or entirely on Solana, via MagicBlock ephemeral rollups.

Upstream: <https://github.com/IVSOP/DeFORM>. Repo-relative paths in this skill
(`crates/…`, `anchor_program/…`) refer to that repository, not to the user's project — you
consume DeFORM as cargo dependencies plus a copied program template, never by working inside
its workspace.

## Mental model

You write the game **rules** once, as a plain deterministic function. DeFORM runs that same
function in three places — your client (predicting ahead), the authority (server or chain),
and any other client — and reconciles them with rollback.

```
   your game engine (bevy, macroquad, raw winit, …)
        │  set_inputs()          read_state()  ▲
        ▼                                      │
   DeformClient<T>          ← identical API for every backend
        │
   ┌────┴─────────────┬──────────────────────┐
   │                  │                      │
deform_offline   deform_quic (client)   deform_foc
 local + bots     ↔ deform_quic::server  ↔ ephemeral rollup running
                    (your web2 server)     YOUR deployed Solana program
```

Everything below `DeformClient` is abstracted: **swapping backends does not change a single
line of game logic or rendering code.** A client only ever talks to one backend at a time,
picked at runtime (see `crates/examples/pong/src/client.rs` — `start_offline`,
`start_online`, `start_online_foc` all end with `commands.insert_resource(MultiplayerClient(client))`).

## Crate map

| Crate | Role | Depend on it when |
| --- | --- | --- |
| `deform_core` | Traits + types + `DeformClient`. Builds for both host and SBF (`target_arch = "bpf"`). | Always. |
| `deform_derive` | `#[derive(Smooth)]` — generates the interpolation/rollback-absorption smoother. Re-exported as `deform_core::Smooth`. | Always (comes with core). |
| `deform_offline` | Backend: single-process simulation with a bot function. No network, no chain. | Prototyping, singleplayer, tests. |
| `deform_quic` | Backend: QUIC client **and** the authoritative web2 server (`deform_quic::server`). | You want playable latency today. |
| `deform_foc` | Backend: fully-on-chain client. `accountSubscribe` over WS for state, `set_inputs` txs for input. | You want the chain to be the authority. |
| `anchor_program/` | **Template you copy, not a dependency.** The Solana program you deploy yourself. | Any on-chain lobby (both web2 and FoC use it for lobbies/settlement). |

`deform_core` features: `client` (default; enables `DeformClient`, pulls tokio), `anchor`
(adds Anchor ser/de to the account types — use with `default-features = false` in the
on-chain program), `egui-probe`, `metrics`.

## Adding DeFORM to your project

The `deform_*` crates are ordinary cargo dependencies — you do **not** fork the repo or work
inside its workspace. Your game lives in your own repo.

**Not on crates.io (yet).** Depend on them by git — cargo finds each package by name
anywhere in the repo, so no `path` is needed despite them living under `crates/`. Pin a `rev`:
these are pre-1.0 with no semver guarantees, and `branch = "master"` will move under you.

```toml
[dependencies]
deform_core    = { git = "https://github.com/IVSOP/DeFORM", rev = "<commit>" }
deform_offline = { git = "https://github.com/IVSOP/DeFORM", rev = "<commit>" }  # start here
# deform_quic  = { git = "https://github.com/IVSOP/DeFORM", rev = "<commit>" }  # web2 server
# deform_foc   = { git = "https://github.com/IVSOP/DeFORM", rev = "<commit>" }  # chain as authority

# your game's types must satisfy DeFORM's bounds, so you need these directly:
wincode   = { version = "0.5.5", default-features = false, features = ["alloc", "std", "derive"] }
serde     = { version = "1", features = ["derive"] }
thiserror = "2"
glam      = "0.32"            # only if you use Vec2/Vec3 in your game state
tokio-util = "0.7"            # CancellationToken, passed to every backend

# REQUIRED: DeFORM builds against a git revision of wincode, not the crates.io release.
[patch.crates-io]
wincode = { git = "https://github.com/anza-xyz/wincode", rev = "ce424cbcd1c3cca34ad16b8d462feb444fcbf172" }
```

**The `[patch.crates-io]` entry is not optional and is the single most likely thing to break
your first build.** Without it — or if any dependency drags in a `wincode` 0.6.x — you get a
wall of errors like `the trait bound Address: SchemaWrite<...> is not satisfied` with a note
saying *"there are multiple different versions of crate `wincode` in the dependency graph"*.
That message means exactly what it says: two incompatible `wincode`s, so `deform_core`'s
`SchemaRead`/`SchemaWrite` impls aren't the ones your derives generated.

If you depend on `solana-address` yourself, pin it so its `wincode` unifies with the patched
one (`solana-address = "=2.6.1"` works; 2.7 pulls wincode 0.6). The same class of conflict
can come from any Solana crate, so when in doubt run `cargo tree -i wincode` and confirm
there is exactly one.

## Path A — up and running fastest (always implement `deform_offline` first)

**Whatever you are ultimately building, wire up `deform_offline` before anything else.** It
is the fastest way to get a playable, testable game and the only backend that needs no
infrastructure at all: no keypair, no validator, no server, no Docker, no deployed program.
It exercises the entire trait surface and the same fixed-tick + visual-tick loop the
networked backends use, so tick-rate, smoothing, and rendering bugs all surface here — where
they are trivially reproducible and unambiguously *your* code rather than the network.

1. Define `Inputs`, `GameState`, `Error`, and the logic type implementing `DeformUserLogic`.
2. Build a `Lobby` in memory with `LobbyState::NotStarted` and `Network::Web2`.
3. `deform_offline::new_offline_client::<MyGame>(...)`, passing a bot function for the
   other players.
4. Poll `client.read_state()` each frame; `client.set_inputs()` each frame.

Get the game fully playable against the bot. *Then* point the same types at
`deform_quic::new_quic_client` (add a server) or `deform_foc::new_foc_client` (add a deployed
program) — only the constructor call changes. Keep the offline path working afterwards; it
stays your regression harness for `advance_frame`, since a desync there is provably a
determinism bug and not a netcode one.

(Offline never rolls back — there is nothing to disagree with — so rollback paths still need
a networked backend to exercise.)

## Path B — control and extensibility

- **Web2 server**: implement `DeformQuicLogic` (a *second* trait — see
  `references/backends.md`) to add connection auth, custom reliable messages, and an
  `on_match_end` hook that settles the result on-chain. Configure `DeformQuicServer`
  fields directly (`match_config`, `max_conn_per_ip`, raw `quinn::ServerConfig`).
- **On-chain**: `GameProgramClient` is the instruction-builder seam. Every instruction
  DeFORM sends is built by *your* impl, so the on-chain program's account layout, extra
  instructions, fees, and even framework (Anchor → Pinocchio) are yours to change.
- **Interpolation**: tune `#[smooth(...)]` per struct, or write your own `Smooth<G>` impl.
- **Latency estimation** (FoC): pick one of `rtt-getslot` / `rtt-ping` / `rtt-inputs` at
  compile time.

Feature surface is still small. When something isn't configurable, the escape hatch is
almost always "implement the trait yourself" rather than a config knob.

## The three types you define

```rust
use std::collections::{BTreeMap, HashMap};

use deform_core::{
    DeformGameState, DeformInputs, DeformUserLogic, Pubkey, Smooth,
    accounts::lobby::{not_started::LobbyNotStarted, LobbyMetadata},
};
use wincode::{SchemaRead, SchemaWrite};

// ---------- 1. inputs: what a player sends, per tick ----------
#[derive(Default, Clone, Debug, Eq, PartialEq,
         serde::Serialize, serde::Deserialize, SchemaRead, SchemaWrite)]
pub struct MiniInputs {
    pub dir: i8, // -100 .. 100
}
impl DeformInputs for MiniInputs {}
// `Eq` is mandatory: it is how the netcode detects that a prediction was wrong.

// ---------- 2. game state: fully replaced every tick ----------
#[derive(Default, Clone, Debug, serde::Serialize, SchemaRead, SchemaWrite, Smooth)]
pub struct MiniPaddle {
    #[smooth]
    pub y: f32,
    pub score: u32,
}

#[derive(Default, Clone, Debug, serde::Serialize, SchemaRead, SchemaWrite, Smooth)]
// decay/max_correction are per SIMULATION tick; see references/smoothing.md before
// picking numbers — a value tuned at 60Hz means something very different at 20Hz.
#[smooth(decay = 0.5, max_offset = 200.0, min_offset_sq = 9.0,
         max_correction = 40.0, motion_ratio = 2.0)]
pub struct MiniState {
    #[smooth]
    pub ball_y: f32,
    pub ball_v: f32,
    #[smooth(map)] // per-entry smoothing; MiniPaddle must also derive Smooth
    pub paddles: HashMap<Pubkey, MiniPaddle>,
}

impl DeformGameState for MiniState {
    fn has_ended(&self) -> bool {
        self.paddles.values().any(|p| p.score >= 5)
    }
}

// ---------- 3. the logic type: survives rollback ----------
#[derive(Debug, Clone, thiserror::Error, serde::Serialize, SchemaRead, SchemaWrite)]
pub enum MiniError {
    #[error("unreachable")]
    Never, // wincode wants >1 variant; keep a dummy if you only have one real error
    #[error("lobby has no players")]
    NoPlayers,
}

#[derive(Debug, Clone, serde::Serialize, SchemaRead, SchemaWrite)]
pub struct MiniPong {
    /// Not game state: a rollback does NOT undo this.
    pub rallies: u32,
}

impl DeformUserLogic for MiniPong {
    type Inputs = MiniInputs;
    type GameState = MiniState;
    type Smoother = MiniStateSmoother; // generated by #[derive(Smooth)]
    type Error = MiniError;

    const TICK_RATE_MICROS: u64 = 50_000; // 20 Hz — must equal ER slot time for FoC

    fn new_from_lobby(_m: &LobbyMetadata, _n: &LobbyNotStarted) -> Result<Self, MiniError> {
        Ok(MiniPong { rallies: 0 })
    }

    fn new_game_from_lobby(
        _m: &LobbyMetadata,
        not_started: &LobbyNotStarted,
    ) -> Result<MiniState, MiniError> {
        if not_started.player_status.is_empty() {
            return Err(MiniError::NoPlayers);
        }
        Ok(MiniState {
            ball_v: 300.0,
            paddles: not_started
                .player_status
                .keys()
                .map(|pk| (*pk, MiniPaddle::default()))
                .collect(),
            ..Default::default()
        })
    }

    /// Pure and deterministic. Runs on every client, the server, and inside the
    /// Solana program — same bytes in, same bytes out, or clients desync.
    fn advance_frame(
        &mut self,
        state: &MiniState,
        inputs: &BTreeMap<Pubkey, MiniInputs>,
    ) -> Result<MiniState, MiniError> {
        let dt = Self::TICK_RATE_MICROS as f32 / 1_000_000.0;
        let mut next = state.clone();

        for (pk, input) in inputs {
            if let Some(p) = next.paddles.get_mut(pk) {
                p.y = (p.y + input.dir as f32 * dt).clamp(-100.0, 100.0);
            }
        }

        next.ball_y += next.ball_v * dt;
        if next.ball_y.abs() >= 100.0 {
            next.ball_y = next.ball_y.clamp(-100.0, 100.0);
            next.ball_v = -next.ball_v;
            self.rallies += 1; // persists across rollbacks
        }

        Ok(next)
    }
}
```

### Why `DeformUserLogic` is not the game state

`GameState` is *recreated every tick* and thrown away on rollback — the netcode keeps a
history of `TickInfo { game_state, inputs }` and rewinds it freely. The type implementing
`DeformUserLogic` is the **one long-lived object per match**: it is created once from the
lobby and reused until the match ends, and `advance_frame` takes `&mut self`. So it is where
per-player metadata, config, counters, RNG seeds, or handles you don't want reset belong.
It is also the type stored server-side and on-chain alongside the lobby (`LobbyOngoing`
carries both `tick_info` and `user_logic`), so it must serialize fast.

Trade-off: anything you keep in `self` will *not* be rewound, so it must be either
idempotent or genuinely rollback-independent (a stat counter is fine; a physics
accumulator is not — that belongs in `GameState`).

The `on_rollback` / `on_gap` / `on_fast_forward` callbacks (all defaulted to `Ok(())`) hang
off the same trait — use them to emit events, log, or resync audio/VFX.

## Wiring a backend

All three return the same `DeformClient<T>`.

```rust
// offline — no network
let client = deform_offline::new_offline_client::<MiniPong>(
    me, lobby, bot_fn, visual_tick_micros, cancellation_token)?;

// web2 — QUIC server is the authority
let client = deform_quic::new_quic_client::<MyQuicLogic>(
    server_addr, server_name, lobby, me, skip_cert_verify,
    visual_tick_micros, auth, cancellation_token)?;

// fully on-chain — the ephemeral rollup is the authority
let endpoints = validator_network.er_endpoints();
let client = deform_foc::new_foc_client::<MyFocLogic>(
    endpoints.rpc.into(), endpoints.ws.into(), Arc::new(keypair),
    MyProgramClient, lobby, visual_tick_micros, slot_time_micros,
    cancellation_token)?;
```

Each spawns its own OS thread with a dedicated tokio runtime and hands back the client
synchronously, so a non-async game loop can use it directly. Every backend honours the
single `CancellationToken` you pass in — **call `client.shutdown()` (or cancel the token) on
exit; backends do not self-terminate when a match ends.**

`visual_tick_micros` is your *render* rate; `TICK_RATE_MICROS` is the *simulation* rate.
They are deliberately independent — the smoother rescales its per-tick rates by the ratio, so
you author `decay`/`max_correction` per simulation tick and never compensate for refresh rate.

## Using the client from a game loop

```rust
client.set_inputs(MiniInputs { dir })?;            // cheap, channel send

let lobby = {
    let state = client.read_state()?;              // MutexGuard — drop it fast
    ping_ms = state.stats.ping_ms;
    state.lobby.clone()
};
if let LobbyState::Ongoing(ongoing) = lobby.state {
    draw(&ongoing.tick_info.game_state);           // already interpolated
}
```

`read_state()` returns a `MutexGuard` over the shared backend state — clone what you need
and drop it; the backend thread blocks on the same mutex. The `game_state` you read is the
**visual** state (interpolated + rollback offsets applied), not the raw simulation state.

## Interpolation

`#[derive(Smooth)]` on `Foo` generates `FooSmoother` plus `impl Smooth<Foo>` and
`impl Smoothable for Foo`. Two jobs: lerp between the previous and current tick for smooth
motion at any render rate, and absorb rollback corrections as a decaying offset so a
mispredicted paddle *eases* into place instead of teleporting. Use `NoopSmoother` as
`type Smoother` to disable it. Details and all field attributes: `references/smoothing.md`.

Two things that catch people out, both covered there: `decay` is per **simulation** tick (so
`0.9` at 20 Hz is ~6× slower in wall-clock than `0.9` at 60 Hz), and smoothing cannot fix a
state that is persistently wrong — if a remote entity visibly rubber-bands, fix
`DeformInputs::predict()` first, then reach for `motion_ratio`.

## The on-chain program is yours

DeFORM does **not** ship a deployed program, and the program is not something you can pull
from cargo. Unlike the `deform_*` crates, `anchor_program/` is source you **copy into your
own repo** — grab it from <https://github.com/IVSOP/DeFORM> (clone, sparse-checkout, or just
download that directory), then point it at your game crate, `declare_id!` your own key,
`anchor build`, and deploy. From then on it is your program, diverging freely.

`deform_core::game_program_client::GameProgramClient` is the trait that turns "DeFORM needs
to create a lobby / ready up / start / commit inputs / crank / settle" into *your*
instructions. Full lifecycle, delegation, and the copy-and-retarget steps:
`references/onchain.md`.

Note this applies to the web2 path too: lobbies are still created on-chain and the QUIC
server settles the result via `write_and_close` when the match ends.

## References

| File | Contents |
| --- | --- |
| `references/core-api.md` | Every trait bound, `TickInfo`, `Lobby`/`LobbyState`, `DeformClient`, errors, wincode requirements |
| `references/backends.md` | Choosing a backend; offline/QUIC/FoC constructors; `DeformQuicLogic` + running a server |
| `references/netcode.md` | Prediction, rollback, gaps, fast-forward, time dilation, ticks-ahead targeting |
| `references/smoothing.md` | `#[derive(Smooth)]` attributes, `SmoothableField`, custom smoothers |
| `references/onchain.md` | Program template, lobby lifecycle, ER delegation, the crank, codama client generation, deployment |
| `references/example-pong.md` | Guided tour of `crates/examples/pong` + how to run it locally |
| `references/example-shooter.md` | 3D shooter with bevy + avian + tnua: embedding a real physics engine in `advance_frame`, the local-camera/look-input split, fixed-tick tuning |

## Gotchas checklist

- `advance_frame` must be **deterministic**: no `HashMap` iteration order dependence, no
  wall clock, no un-seeded RNG, no `f32` operations that differ across targets. It runs on
  clients, server, *and* inside an SBF program. (A game may deliberately relax this and
  give up the FoC backend — the shooter example does, to run a real physics engine; see
  `references/example-shooter.md`.)
- Inputs need `Eq` — misprediction detection is an equality check.
- FoC requires `TICK_RATE_MICROS == slot_time_micros`; `new_foc_client` returns
  `DeformError::TickRateMissmatch` otherwise. Default `get_micros_per_slot` is 50 000 (20 Hz).
- Ephemeral rollups can't resize accounts, so PDAs are created at max size up front:
  `MAX_LOBBY_ACCOUNT_BYTES` for the lobby, `MAX_INPUTS_ACCOUNT_BYTES` per player's inputs,
  `MAX_INPUTS` capping buffered input entries. Raise them when your state grows, or
  serialization runs out of room at runtime, inside the program.
- Serialization is wincode, not borsh, for anything DeFORM stores. Foreign POD types need
  `wincode::pod_wrapper!` + `#[wincode(with = "...")]` (see `PodVec2` in pong).
- `GameState` needs serde `Serialize`; `Inputs` needs `Serialize` **and** `DeserializeOwned`.
  Never add `Deserialize` to the `DeformUserLogic` bound set — it causes an E0283 ambiguity.
- After editing the on-chain program, run `anchor build` then `yarn generate` in
  `anchor_program/` to regenerate the codama client.
