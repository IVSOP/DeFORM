# Setup and integration guide

[Back to the README](../README.md)

## Try an example

The Rust workspace lives in [`crates/`](../crates), rather than the repository root.
It selects nightly Rust through its `rust-toolchain.toml`. The graphical examples
use Bevy and need a desktop graphics environment and its platform dependencies.

**Pong is the smallest complete example to read and adapt.** From the repository root:

```sh
cd crates
cargo run --locked -p pong -- run --offline
```

Use **W/S** to move your paddle against the bot. All four graphical examples can
start offline without a wallet, funded account, RPC connection, or running server:

```sh
cargo run --locked -p soccer -- run --offline
cargo run --locked -p shooter -- run --offline
cargo run --locked -p shooter_airsoft -- run --offline
```

Omit `--offline` to open the menu instead. Its **Play Offline** button also works
without loading a keypair; offline play uses a local player ID. A wallet is only
needed for the online flow, and can still be preloaded with
`--wallet /absolute/path/to/test-keypair.json`.

In the shooter examples, use WASD to move, the mouse to aim, and left click to
fire. Airsoft's [README](../crates/examples/shooter_airsoft/README.md) covers its
controls and assets.

| Example | Start here for | Backends |
| --- | --- | --- |
| [Pong](../crates/examples/pong) | A small simulation and complete client integration | Offline, QUIC, FOC |
| [Soccer](../crates/examples/soccer) | A larger game with match phases | Offline, QUIC, FOC |
| [Shooter](../crates/examples/shooter) | A 3D physics simulation | Offline, QUIC |
| [Airsoft](../crates/examples/shooter_airsoft) | A playable 3D example with a shipped level | Offline, QUIC |
| [Pong FFI](../crates/examples/pong_ffi) | Rust exports, a C header, and a C caller | See its wrapper and feature flags |

The shooter examples' physics simulation is not available in the onchain build.

## Use the library

### Define the simulation

Start with [`pong_logic.rs`](../crates/examples/pong/src/pong_logic.rs). It keeps the
simulation separate from the Bevy application and implements these core traits:

| Trait | What your game supplies |
| --- | --- |
| `DeformInputs` | One player's input for a tick, equality comparison, and optional prediction/merging rules |
| `DeformGameState` | The simulation state and `has_ended()` |
| `DeformUserLogic` | The associated input/state/error/smoother types, lobby initialization, tick period, and `advance_frame()` |

`advance_frame()` receives the current state and a `BTreeMap<Pubkey, Inputs>`
containing every player's applied input, and returns the next state. Keep all
state needed to reproduce a tick in the simulation state. The same starting state
and inputs must produce the same result during normal execution and replay:
wall-clock reads, unseeded randomness, or gameplay decisions based on unordered
iteration can break that requirement. Keep rendering, audio, and other external
side effects out of this function, since rollback can execute it again.

The traits require serialization and thread-safety bounds; copy the derives and
feature setup from Pong rather than implementing only the three methods above.
[`deform_core`](../crates/deform_core/src/lib.rs) contains the complete contracts.
`DeformInputs::predict()` repeats the previous input by default; override it to
clear one-shot actions that should not repeat. `merge()` combines multiple samples
from the same player within one simulation tick; its default keeps the latest.

For development inside this repository, add your game under `crates/examples/`
(already included by the workspace's `examples/*` member pattern), and start with:

```toml
[dependencies]
deform_core = { workspace = true }
deform_offline = { workspace = true }
tokio-util = { workspace = true }
serde = { workspace = true }
wincode = { workspace = true }
```

Add the selected network backend when needed. For a separate application, use
path dependencies to the crates in this checkout and carry over the workspace's
[`wincode` patch](../crates/Cargo.toml); a dependency's workspace patch does not
configure your application's root manifest.

### Choose a backend

Each constructor returns a `DeformClient<YourGame>`, so input submission and
rendering use the same interface across backends.

| Backend | Constructor | What runs remotely / what you provide |
| --- | --- | --- |
| [`deform_offline`](../crates/deform_offline/src/lib.rs) | `new_offline_client` | No remote. Provide a local lobby, player ID, and a bot callback for the other players. |
| [`deform_quic`](../crates/deform_quic/src/lib.rs) | `new_quic_client` | A Rust server runs the authoritative simulation. Implement `DeformQuicLogic` for authentication, custom messages, and the game/program types. |
| [`deform_foc`](../crates/deform_foc/src/lib.rs) | `new_foc_client` | An onchain program runs the authoritative simulation. Implement `DeformFocLogic` and `GameProgramClient`; provide RPC/WebSocket endpoints, a signing keypair, and the lobby. |

The current QUIC server also uses Solana for lobby discovery and match settlement;
it is not a chain-independent deployment. FOC additionally needs the game logic
compiled into the [Anchor program](../anchor_program/programs/anchor_program),
delegated lobby/input accounts, and a recurring `tick` task on the ephemeral
rollup. [`PongAnchorClient`](../crates/examples/pong/src/solana/anchor_client.rs)
shows how to build the instructions.

[`client.rs`](../crates/examples/pong/src/client.rs) contains all three working
construction paths: `start_offline`, `start_online`, and `start_online_foc`.
The offline path constructs the lobby entirely in memory. Networked paths use
the lobby read from the program.

### Connect your game loop

Once a backend has returned a client, submit input samples from your application
loop and read the latest published state. This helper is independent of the
chosen backend and engine:

```rust
use deform_core::{DeformClient, DeformUserLogic};
use deform_core::accounts::lobby::LobbyState;
use deform_core::error::UserFacingResult;

fn update<T: DeformUserLogic>(
    client: &DeformClient<T>,
    inputs: T::Inputs,
) -> UserFacingResult<T, Option<T::GameState>> {
    client.set_inputs(inputs)?;
    let snapshot = {
        let shared = client.read_state()?;
        shared.internal_error.clone()?;
        match &shared.lobby.state {
            LobbyState::Ongoing(game) => Some(game.tick_info.game_state.clone()),
            LobbyState::Finished(game) => Some(game.0.tick_info.game_state.clone()),
            LobbyState::NotStarted(_) => None,
        }
    }; // Release the lock before rendering.
    Ok(snapshot)
}
```

The backend owns simulation timing; call `set_inputs()` as your application
collects input rather than calling `advance_frame()` yourself in the render loop.
Handle lobby completion in your UI, and call `client.shutdown()` when leaving a
match or closing the application.

Choose `NoopSmoother` initially, or use `#[derive(Smooth)]` and annotate visual
fields with `#[smooth]`, as Pong does. The smoother interpolates between local
simulation ticks and softens rollback corrections in the state published for
rendering. It does not change the simulation history used for gameplay.
`visual_tick_micros` controls the visual publication cadence;
`DeformUserLogic::TICK_RATE_MICROS` defines the simulation's nominal tick period.

### Run a networked example

Pong's [`server.rs`](../crates/examples/pong/src/server.rs) is the minimal QUIC
server integration. With the matching lobby program already deployed and an
admin keypair configured, run from `crates/`:

```sh
cargo run --locked -p pong --no-default-features --features server,anchor,20hz -- \
  serve --rpc-url http://127.0.0.1:8899 --keypair /absolute/path/to/admin.json
```

The default QUIC port is 4433. Run a client for each player with a different
keypair, connect to the same RPC cluster, select `Web2` and the matching server,
create/join the same lobby, mark both players ready, read the lobby, and choose
**Play Online (web2)**. The example server uses development TLS configuration;
its local clients expose **Skip TLS verification (dev)** for that setup.

For FOC, use a `FullyOnChain` lobby instead. After both players are ready, use
**Start** to delegate the accounts, then **Init Crank** to schedule simulation
ticks. Read the lobby and use **Play Online (FoC)**. Starting the lobby and
scheduling the crank are separate operations. The configured slot duration must
match the validator used by the game.

The repository includes [`build_pong.sh`](../anchor_program/build_pong.sh) for the
program/client build, a [local Docker stack](../crates/examples/pong/docker/localhost),
and a [two-client launcher](../crates/examples/pong/run.sh). These are development
setups: the Compose file expects built program artifacts, an admin keypair, and
the custom `surfpool/surfpool:programsubscribe-local` image. Prepare those before
using the launcher. Rebuild the program, server, and clients together when game
types or tick-rate features change; Pong defaults to `20hz`, and its `20hz` and
`60hz` features must not be enabled together.

