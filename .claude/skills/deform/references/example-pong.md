# The pong example, end to end

`crates/examples/pong/` is the reference integration: one game, all three backends, a QUIC
server, and an Anchor program. Read it in this order.

## File map

| File | ~LoC | What to learn from it |
| --- | --- | --- |
| `src/pong_logic.rs` | 340 | **Start here.** The whole `DeformUserLogic` implementation: inputs, game state, `Smooth` derive, error enum, `advance_frame`, the bot, and the `DeformQuicLogic` / `DeformFocLogic` bindings |
| `src/client.rs` | 510 | Bevy integration: `start_offline` / `start_online` / `start_online_foc` all produce the same resource; `send_inputs` and `update_state` systems; shutdown on `AppExit` |
| `src/menu.rs` | 660 | The lobby lifecycle driven by hand: create → join → ready → start → init crank, each building an ix via `GameProgramClient` and sending it |
| `src/solana/anchor_client.rs` | 320 | `PongAnchorClient`: the `GameProgramClient` impl over codama builders |
| `src/server.rs` | 40 | The entire QUIC server binary |
| `src/main.rs` | 160 | CLI (`run` / `serve` / `fetch-lobbies`), plus reading lobbies off-chain with a `Memcmp` filter |
| `src/generated/` | — | codama output — do not edit |
| `docker/localhost/` | — | surfpool + ephemeral validator + pong server compose stack |
| `run.sh` | — | launches two clients with separate wallets plus the docker stack |

The whole game is ~1500 lines including a full bevy/egui UI. The netcode-facing part —
`pong_logic.rs` — is 340.

## The three types, in one place

From `pong_logic.rs`:

```rust
PongInputs   { direction: i8 }                       // impl DeformInputs (default predict)
PlayerState  { #[smooth] paddle_y: f32, score: u32 } // derives Smooth
PongGameState{ #[smooth] ball_pos: Vec2, ball_vel: Vec2,
               creator: Pubkey, #[smooth(map)] players: HashMap<Pubkey, PlayerState> }
PongGame;                                            // unit struct — pong needs no persistent data
PongError    { Never, LobbyNotStarted, SerializeInputs(String), ScheduleCrank(String) }
```

`PongGame` being a unit struct is worth noting: pong has nothing that must survive a
rollback, so the logic type carries no data. `advance_frame(&mut self, …)` still gets its
`&mut self` — that's where you'd add a hit counter or an RNG seed.

`has_ended()` is `players.values().any(|ps| ps.score >= 10)`.

Note `ball_vel` is deliberately *not* `#[smooth]`ed — smoothing a velocity fights the
integrator. `PongGameState` pins its own `#[smooth(...)]` params, which propagate to the
per-entry `PlayerState` smoothers created by `#[smooth(map)]`.

Pong is the hard case for smoothing and worth reading as a worked example: 20 Hz ticks,
binary input (`direction` is ±100 or 0, so every misprediction is worth full paddle speed),
and a tight 2D field where an error of 70 units out of 1000 is obvious. Its params are
commented in `pong_logic.rs` with the arithmetic behind each one. `PongInputs` keeps the
default `predict()` on purpose, and the comment there records why damping it backfires.

## Backend swap

Three functions in `client.rs`, identical tails:

```rust
new_offline_client::<PongGame>(me, lobby, pong_bot, visual_tick_micros, token)?
new_quic_client::<PongQuicLogic>(addr, name, lobby, me, skip_cert, visual, NoAuth, token)?
new_foc_client::<PongFocLogic>(rpc, ws, keypair, PongAnchorClient, lobby, visual, slot, token)?
// → commands.insert_resource(MultiplayerClient(client));
```

Everything downstream — `send_inputs`, `update_state`, rendering — is backend-agnostic and
untouched. That is the whole design goal.

`update_state` is worth copying verbatim as an integration pattern: it takes the lock once,
copies `stats.ping_ms` out *before* any early return (so the HUD keeps updating when the
lobby isn't `Ongoing`), clones the lobby, and drops the guard.

## Cargo features

```
default = ["client", "server", "anchor", "foc", "20hz"]
bin     — CLI + tokio + solana client deps
client  — bevy + egui (pulls `bin`)
server  — the QUIC server (pulls `bin`)
anchor  — borsh/Anchor derives on the shared types; needed by the on-chain program
foc     — the fully-on-chain backend (pulls `client`)
20hz / 60hz — sets TICK_RATE_MICROS (50_000 / 16_667)
metrics — deform_metrics spans/plots/events across all crates (Tracy + on-disk run dir)
```

`20hz` is the default because the ephemeral validators run at 20 Hz and FoC requires
`TICK_RATE_MICROS == slot time`.

## Running it locally

```sh
cd crates/examples/pong
./run.sh          # two clients (separate wallets) + docker stack, Ctrl+C tears down all
```

The stack (`docker/localhost/docker-compose.yml`):

| Service | Port | Notes |
| --- | --- | --- |
| `surfpool` | 8899 / 8900 | base layer; **must** fork devnet (`start --no-tui --network devnet`) or the ER's fee-vault startup checks fail |
| `ephemeral-validator` | 7799 (RPC) / 7800 (WS) | MagicBlock ER, ~50 ms slots, config from `magicblock/config.toml` |
| `pong_server` | 4433/udp | the QUIC server, built from `docker/localhost/Dockerfile` |

Wallets are `CLi1*.json` / `CLi2*.json` at the workspace root; `run.sh` resolves them by
prefix and runs the clients with cwd there so the in-app keypair dropdown finds them too.

Manual variants:

```sh
# offline only — no docker, no chain
cargo run -p pong -- run --wallet ../../CLi1*.json

# server on its own
cargo run -p pong --no-default-features --features "server,anchor,20hz" -- serve --port 4433

# dump every lobby on chain as JSON
cargo run -p pong -- fetch-lobbies --rpc-url http://127.0.0.1:8899
```

## Playing a fully-on-chain match

The menu drives this manually, which is the clearest illustration of the lifecycle:

1. Pick a wallet and a network preset (Localhost / Devnet / Mainnet).
2. **Create Lobby** with an id, `Network::FullyOnChain(ValidatorNetwork::…)`.
3. Second client: **Join Lobby** with the same id.
4. Both: **Ready** (creates each player's inputs PDA).
5. Creator: **Start** — delegates the lobby and all inputs accounts to the ER.
6. Creator: **Init Crank** — schedules the recurring `tick`. **This transaction goes to the
   ER RPC (7799), not to surfpool (8899).** Sending it to the base layer is the most common
   mistake.
7. Both: **Play Online** — spawns the FoC backend against `er_endpoints()`.

The web2 path skips steps 5–6 and connects to the QUIC server address instead.

## Rebuilding the program

```sh
cd anchor_program
./build_pong.sh   # anchor build --no-default-features --features pong && yarn generate ../crates/examples/pong
```

Required after any change to the program's instructions or arg types — the generated client
under `examples/pong/src/generated/` is what `PongAnchorClient` builds on.
