# `deform_core` API reference

Everything here is exported from the `deform_core` crate (source: `crates/deform_core/` in
the DeFORM repo). It compiles for both the host and SBF (`target_arch = "bpf"`), which is why
several bounds sit behind `Maybe*` marker traits.

## `DeformUserLogic`

The single trait a game must implement. It names the game's types and holds the rules.

```rust
pub trait DeformUserLogic:
    Debug + Clone + Send + Sync + 'static
    + for<'de> SchemaRead<'de, DefaultConfig, Dst = Self>
    + SchemaWrite<DefaultConfig, Src = Self>
    + MaybeSerdeSerialize
```

### Associated types

| Type | Bound | Notes |
| --- | --- | --- |
| `Inputs` | `DeformInputs` | one player's input for one tick |
| `GameState` | `DeformGameState` | recreated every tick, discarded on rollback |
| `Smoother` | `Smooth<Self::GameState>` | `#[derive(Smooth)]` output, or `NoopSmoother` |
| `Error` | `std::error::Error + Send + Sync + 'static + Clone + Display + serde::Serialize + wincode` | broadcast to clients; ends the match |

### Associated constants

| Const | Default | Sizes / caps |
| --- | --- | --- |
| `TICK_RATE_MICROS` | `16667` (60 Hz) | microseconds per simulation tick |
| `MAX_LOBBY_ACCOUNT_BYTES` | `1024` | the lobby PDA, created in `create_lobby` |
| `MAX_INPUTS_ACCOUNT_BYTES` | `1024` | each player's inputs PDA, created in `ready` |
| `MAX_INPUTS` | `32` | buffered per-player input entries |

The byte constants exist because MagicBlock ephemeral rollups are unreliable at resizing
accounts, so PDAs are created at their maximum size up front. They count the
*wincode-serialized* form, not fields — be generous, and re-check them whenever `GameState`
or `Inputs` grows. Undersizing surfaces as serialization failures inside the program
(`SerializeLobby` / `SerializeInputsAccount`), never as a compile error.

The two byte constants size different accounts and should be tuned independently: the lobby
holds the full `GameState` + `user_logic` + one `Inputs` per player, while an inputs account
holds up to `MAX_INPUTS` entries of `HashMap<u64, Inputs>` for a single player.

`MAX_INPUTS` is enforced in `set_inputs`: once a player's inputs account holds that many
entries, further inputs in the batch are silently dropped until the next `tick` prunes them.

### Required methods

```rust
fn new_from_lobby(&LobbyMetadata, &LobbyNotStarted) -> Result<Self, Self::Error>;
fn new_game_from_lobby(&LobbyMetadata, &LobbyNotStarted) -> Result<Self::GameState, Self::Error>;
fn advance_frame(
    &mut self,
    state: &Self::GameState,
    inputs: &BTreeMap<Pubkey, Self::Inputs>,
) -> Result<Self::GameState, Self::Error>;
```

`advance_frame` is the whole simulation. It receives the *complete* input set for the tick
(every player, always present — missing inputs are predicted upstream, not omitted) and
returns the next state. Returning `Err` broadcasts the error to all clients and ends the
match.

`&mut self` is the escape hatch for data that must survive rollback (see the "why it is not
the game state" section in SKILL.md).

### Optional callbacks

All default to `Ok(())`. They fire on the client backends only; they let you emit events,
log, or resync presentation-layer things.

| Callback | Fires when |
| --- | --- |
| `on_rollback(old: TickInfo<Self>, new: &TickInfo<Self>)` | a prediction was wrong; `old` is owned because that timeline is gone |
| `on_gap(old: &TickInfo<Self>, new: &TickInfo<Self>)` | authoritative ticks skipped (e.g. `… 3 _ 5`); a rollback is always emitted too, so ignoring this is safe |
| `on_fast_forward(old: &TickInfo<Self>, new: &TickInfo<Self>)` | the authority is ahead of local simulation; local state is replaced wholesale, intermediate ticks are not recomputed |

### `get_micros_per_slot`

```rust
fn get_micros_per_slot(network: &ValidatorNetwork) -> u64 // default 50_000 for all networks
```

Slot duration is not observable on-chain, and a client cannot be trusted to report it, so
this is a hardcoded per-network answer you may override. The on-chain `tick` handler uses it
to convert elapsed slots into a number of game ticks.

## `DeformInputs`

```rust
pub trait DeformInputs:
    Default + Debug + Eq + Clone + Send + Sync + 'static
    + serde::Serialize + SchemaRead + SchemaWrite
    + MaybeAnchor + MaybeSerdeSerialize + MaybeSerdeDeserialize
{
    fn predict(&self) -> Self { self.clone() }
}
```

- **`Eq` is load-bearing** — the netcode compares received inputs against predicted ones to
  decide whether to roll back.
- **`Default`** is what a player who has never sent anything gets.
- **`predict()`** is called to fabricate an input for a tick that hasn't arrived. The default
  repeats the last input, which is right for held directions. Override it to zero out
  one-shot actions (a jump or a "buy item" toggle) so they don't fire repeatedly during
  prediction.
- `MaybeAnchor` requires `AnchorSerialize + AnchorDeserialize` when the `anchor` feature is
  on — inputs cross the instruction boundary, so they need borsh there too.

## `DeformGameState`

```rust
pub trait DeformGameState:
    Clone + Debug + Send + Sync + serde::Serialize
    + SchemaRead + SchemaWrite + MaybeSerdeSerialize
{
    fn has_ended(&self) -> bool;
}
```

`has_ended()` is polled after each tick. When it returns true the lobby transitions to
`LobbyState::Finished`, the server runs `on_match_end` (settling on-chain), and the FoC
crank stops advancing it.

Note it needs serde `Serialize` but *not* `Deserialize` — the reverse direction always goes
through wincode.

## `TickInfo<T>`

```rust
pub struct TickInfo<T: DeformUserLogic> {
    pub game_state: T::GameState,
    pub inputs: BTreeMap<Pubkey, T::Inputs>,
}
```

One entry of the netcode's history: the state at tick N, plus the inputs that (applied to
N-1) produced it. `BTreeMap` rather than `HashMap` so iteration order is deterministic
everywhere — this matters because `advance_frame` runs on multiple machines including inside
an SBF program.

## Account types (`deform_core::accounts`)

These are the wire *and* on-chain formats. They are wincode-serialized, **not** borsh, and
therefore invisible to Anchor's IDL — which is why the program uses `UncheckedAccount` and
manual PDA checks, and why codama can't generate account decoders for them.

```rust
enum DeformAccount<T> {          // #[repr(u64)] — occupies Anchor's 8-byte discriminator slot
    Lobby(Lobby<T>)  = 0,
    Inputs(InputsAccount<T>) = 1,
}

struct Lobby<T> { metadata: LobbyMetadata, state: LobbyState<T> }

struct LobbyMetadata { id: u64, creator: Pubkey, network: Network, bump: u8 }

enum LobbyState<T> {
    NotStarted(LobbyNotStarted),          // { player_status: BTreeMap<Pubkey, PlayerStatus> }
    Ongoing(LobbyOngoing<T>),             // { slot: Option<u64>, tick: u64, tick_info, user_logic }
    Finished(LobbyFinished<T>),           // newtype around LobbyOngoing
}

struct InputsAccount<T> { bump: u8, lobby_id: u64, player: Pubkey, inputs: HashMap<u64, T::Inputs> }
```

PDA seeds (derive with the provided helpers, don't hand-roll):

- `Lobby::<T>::find_program_address(id, &program)` → `["lobby", id.to_le_bytes()]`
- `InputsAccount::<T>::find_program_address(id, &player, &program)` →
  `["inputs", id.to_le_bytes(), player]`

`DeformAccount::from_bytes(&[u8])` / `.write_into(&mut [u8])` handle the discriminator.

### Networks

```rust
enum Network { Web2, FullyOnChain(ValidatorNetwork) }
enum ValidatorNetwork { Mainnet(MainnetRegion), Devnet(DevnetRegion), Localhost(LocalRegion) }
```

`ValidatorNetwork::address()` gives the MagicBlock validator identity to pin delegated
accounts to. `ValidatorNetwork::er_endpoints()` gives `ErEndpoints { rpc, ws }` for that
region — the ephemeral rollup's HTTP and PubSub URLs, which is exactly what
`new_foc_client` wants. Localhost is `http://127.0.0.1:7799` / `ws://127.0.0.1:7800`
(the docker-compose ER), unlike hosted clusters which share one host for both.

The hosted endpoints are sane defaults, not gospel — the router's `getDelegationStatus` is
the real source of truth for which validator currently holds a delegated account.

## `DeformClient<T>`

```rust
pub struct DeformClient<T: DeformUserLogic> {
    pub set_inputs_sender: mpsc::UnboundedSender<T::Inputs>,
    pub backend_state: Arc<Mutex<DeformSharedBackendState<T>>>,
    pub cancellation_token: CancellationToken,
}

impl<T> DeformClient<T> {
    fn read_state(&self) -> DeformResult<MutexGuard<'_, DeformSharedBackendState<T>>>;
    fn set_inputs(&self, inputs: T::Inputs) -> DeformResult;
    fn shutdown(&self);
}

pub struct DeformSharedBackendState<T> {
    pub lobby: Lobby<T>,
    pub stats: Stats,                       // { ping_ms: f64 }
    pub internal_error: UserFacingResult<T, ()>,
}
```

`DeformClient` is `Clone` (cheap — channel sender + `Arc` + token), so stash it in an ECS
resource and clone freely.

Contract notes:

- `read_state` is a thin mutex lock, deliberately not a clone. Hold it for as short as
  possible; the backend thread contends on it every visual tick.
- The `lobby.state`'s `game_state` is the **visual** state: interpolated toward the current
  tick with rollback offsets applied. The raw simulation state is backend-internal.
- Check `internal_error` — a backend that dies mid-match reports it there, not through the
  return value of `set_inputs`.
- The backend does **not** exit when the match ends. Call `shutdown()` (or cancel the token
  you passed in) yourself; this avoids a race where you send inputs into a just-closed
  channel.

## Errors

```rust
enum DeformError { Serialize, Deserialize, Connection, Protocol, InvalidState, LockPoisoned,
                   ChannelClosed, Rpc, Io, SerializeLobby, DeserializeLobby, BackendPanicked,
                   Auth, SerializeInputsAccount, DeserializeInputsAccount, CommitInputsError,
                   TickRateMissmatch }   // all String-carrying, #[non_exhaustive]

enum UserFacingError<D: DeformUserLogic> { Deform(DeformError), User(D::Error) }

type DeformResult<T = ()>          = Result<T, DeformError>;
type UserFacingResult<D, T = ()>   = Result<T, UserFacingError<D>>;
```

`DeformError` also converts into `solana_program_error::ProgramError::Custom(n)` so the same
enum works inside the program.

## Serialization requirements (wincode)

DeFORM serializes with [wincode](https://github.com/anza-xyz/wincode) — chosen because it is
fast enough to run many times per second and works inside SBF. Practical rules:

- Derive `SchemaRead, SchemaWrite` on every type reachable from `Inputs`, `GameState`,
  `Error`, or the logic type.
- Foreign POD types (e.g. `glam::Vec2`, `solana_signature::Signature`) need a wrapper:

  ```rust
  wincode::pod_wrapper! { unsafe struct PodVec2(Vec2); }

  struct State {
      #[wincode(with = "PodVec2")]
      pub ball_pos: Vec2,
  }
  ```

- A single-variant enum can make the generated reader warn about unreachable code; keep a
  dummy first variant (the pong example uses `PongError::Never`).
- Keep these types small and flat. They are serialized on every tick, every commit, and
  every account write.
