# The on-chain side

**DeFORM ships no deployed program, and the program is not a cargo dependency.** The
`deform_*` crates you `cargo add`; the program you **copy**. `anchor_program/` in the repo
(<https://github.com/IVSOP/DeFORM>) is a working *template* for the simplest case: take that
directory into your own repo, retarget it at your game crate, build, deploy, and own it —
including its upgrade authority, account layout, and any extra instructions you need.

Getting a copy, in rough order of convenience:

```sh
# whole repo, then lift the directory out
git clone https://github.com/IVSOP/DeFORM /tmp/deform && cp -r /tmp/deform/anchor_program my-game-program

# or just that directory
git clone --filter=blob:none --sparse https://github.com/IVSOP/DeFORM /tmp/deform \
  && git -C /tmp/deform sparse-checkout set anchor_program
```

There is no update path once copied — it is a starting point, not a vendored library. Track
upstream changes by hand if you care about them.

This applies to both paths. Even a web2 (QUIC) game creates its lobbies on-chain and settles
results there; only the realtime simulation differs.

## The seam: `GameProgramClient`

Because the program is yours, DeFORM cannot build its instructions. Instead you implement
this trait, and every transaction DeFORM sends goes through it.

```rust
pub trait GameProgramClient<T: DeformUserLogic>: Clone + Send + Sync {
    fn game_program(&self) -> Pubkey;

    fn create_lobby_ix(&self, user, lobby, lobby_id, network: Network) -> Result<Instruction, T::Error>;
    fn join_lobby_ix (&self, user, lobby, lobby_id)                    -> Result<Instruction, T::Error>;
    fn ready_ix      (&self, args: ReadyArgs)                          -> Result<Instruction, T::Error>;
    fn start_ix      (&self, user, lobby_pubkey, &LobbyMetadata, &LobbyNotStarted, game) -> …;
    fn set_inputs_ix (&self, user, inputs_account, lobby_account, lobby_id,
                      inputs: &HashMap<u64, T::Inputs>)                -> …;
    fn tick_ix       (&self, lobby_account, lobby_id, inputs_accounts: &[Pubkey]) -> …;
    fn init_crank_ix (&self, payer, lobby_account, lobby_id, inputs_accounts,
                      execution_interval_millis: i64, iterations: i64) -> …;
    fn write_and_close_ix(&self, admin, lobby_pubkey, creator, lobby: &Lobby<T>) -> …;
}

pub enum ReadyArgs {
    Web2        { user, lobby, id },
    FullyOnchain{ user, lobby, id, inputs },   // FoC additionally creates the inputs PDA
}
```

It is an instruction *builder*, not a client — it never sends anything. That is deliberate:
you keep control of RPC endpoints, fee payers, priority fees, retries, and simulation.

The reference impl is `crates/examples/pong/src/solana/anchor_client.rs` (`PongAnchorClient`),
which is a thin wrapper over the codama-generated builders. Swapping Anchor for Pinocchio or
a hand-written program means rewriting only this file.

## Lobby lifecycle

```
create_lobby ── join_lobby ──▶ ready ──▶ start ──▶ init_crank ──▶ … play … ──▶ write_and_close
   (creator)     (others)      (each)   (FoC only)  (FoC only)                    (admin/server)
     │                                     │
     └── LobbyState::NotStarted ───────────┴──▶ Ongoing ───────────────────────▶ Finished
```

| Instruction | Signer | What it does |
| --- | --- | --- |
| `create_lobby(id, network)` | creator | creates the lobby PDA at `MAX_LOBBY_ACCOUNT_BYTES`, `NotStarted`, creator `NotReady` |
| `join_lobby(id)` | joiner | adds the player as `NotReady` |
| `ready(id)` | each player | marks `Ready`; on `FullyOnChain` also creates that player's inputs PDA |
| `start(id)` | a player in the lobby | verifies everyone is `Ready`, builds `user_logic` + initial `GameState`, transitions to `Ongoing`, then **delegates** the lobby and every inputs account to the ephemeral rollup |
| `init_crank` | payer | schedules the recurring `tick` on the ER (**send to the ER RPC, not the base layer**) |
| `set_inputs(id, bytes)` | player | writes a wincode-encoded `HashMap<u64, Inputs>` batch into that player's inputs account |
| `tick(id)` | **none** | signerless; the ER task executor drives it. Advances the simulation |
| `write_and_close(id, scores)` | admin | settles the result and closes the lobby |
| `undelegate` / `process_undelegation` | — | returns accounts to the base layer; `process_undelegation` is a delegation-program callback — **do not rename it**, its name determines the discriminator |

Web2 games use `create_lobby` / `join_lobby` / `ready(Web2)` / `write_and_close` only. They
never delegate, never crank, and never call `set_inputs` — the QUIC server is the authority
and calls `write_and_close` from `on_match_end`.

### Delegation (`start`)

`start` is the interesting one. It delegates the lobby PDA and every player's inputs PDA to
the MagicBlock ephemeral rollup, pinned to a single validator
(`ValidatorNetwork::address()`) so a `tick` transaction can touch them all together.

Constraints that shape the code:

- Delegation **zeroes the account data and reassigns ownership**, so the lobby must be fully
  written *before* the delegate CPI. In the template, delegating the lobby is the last thing
  the handler does.
- ERs are unreliable at resizing, so accounts are created at max size up front:
  `MAX_LOBBY_ACCOUNT_BYTES` for the lobby PDA in `create_lobby`, and
  `MAX_INPUTS_ACCOUNT_BYTES` for each player's inputs PDA in `ready`.
- Remaining accounts are grouped per player, in lobby order:
  `[inputs, inputs_buffer, inputs_delegation_record, inputs_delegation_metadata]`.
- `commit_frequency_ms: u32::MAX` — state is not periodically committed back to L1; the
  result is settled explicitly at the end.

### The crank

Nothing on Solana runs on its own, so `tick` is driven by a **scheduled task** on the ER
(MagicBlock's magic-program `ScheduleTask`).

- `init_crank_ix` builds the schedule and embeds the `tick_ix` instruction inside it.
- It is scheduled by a **direct client transaction** to the ER RPC — not via CPI from your
  program.
- `tick` is **signerless** for exactly this reason: the task executor drives it unattended.
- Interval: `TICK_RATE_MICROS / 1000` ms, `iterations: i64::MAX` (runs until the lobby is
  undelegated). Pong's menu does exactly this.
- `tick` tolerates irregular execution: it computes elapsed slots and runs
  `max(1, slot_delta * micros_per_slot / TICK_RATE_MICROS)` game ticks to catch up.

## Retargeting the copied template

The program is generic over the game via a single indirection —
`programs/anchor_program/src/state.rs`:

```rust
#[cfg(feature = "pong")]
pub use pong::pong_logic::{
    PongGame as UserLogic, PongGameState as GameState, PongInputs as Inputs,
};
```

Everything else in the program refers to `UserLogic` / `GameState` / `Inputs`. So retargeting
is mostly a Cargo.toml exercise.

**First, fix the dependencies the copy just broke.** As shipped,
`programs/anchor_program/Cargo.toml` reaches back into the DeFORM workspace by relative path:

```toml
deform_core = { path = "../../../crates/deform_core", default-features = false, features = ["anchor"] }
pong        = { path = "../../../crates/examples/pong", default-features = false, features = ["anchor"], optional = true }
```

Both paths are dangling once the directory lives in your repo. Replace them:

```toml
deform_core = { git = "https://github.com/IVSOP/DeFORM", rev = "<commit>", default-features = false, features = ["anchor"] }
my_game     = { path = "../../../my_game", default-features = false, features = ["anchor"] }
```

Use the **same `rev`** your game crate uses. The program and the client serialize the same
account layouts with the same `wincode` schemas; a mismatched `deform_core` between them is a
silent wire-format break, not a compile error.

Then:

1. Re-export your three types as `UserLogic` / `GameState` / `Inputs` in `state.rs`. You can
   drop the `#[cfg(feature = "…")]` gating entirely — it exists so the template can ship
   pong optionally, and a program that serves exactly one game does not need it. If you drop
   it, also drop the `pong` feature block and `default = ["pong"]` from the manifest and
   build with a plain `anchor build`.
2. Make sure your game crate has an `anchor` feature that enables `deform_core/anchor` and
   `solana-address/borsh`, and that `Inputs` derives `AnchorSerialize`/`AnchorDeserialize`
   under it (inputs cross the instruction boundary as borsh, not wincode).
3. `declare_id!` a fresh program ID in `src/lib.rs` and update `Anchor.toml`
   (`[programs.localnet]` and the program name).
4. Rename the crate/directory from `anchor_program` if you like — nothing in `deform_core`
   depends on the name, only your `GameProgramClient` impl does.

Two things that travel with the copied directory and that you should **keep**:

- `anchor_program/Cargo.toml`'s `[patch.crates-io]` pinning `wincode` to the anza git rev.
  The program is a separate cargo workspace from your game, so it needs its own copy of the
  patch — same requirement as your game's manifest, for the same reason.
- `rust-toolchain.toml` (1.89.0). Solana's toolchain lags; this is what the program is known
  to build under.

`deform_core` must be `default-features = false, features = ["anchor"]` here. Its default
`client` feature pulls in tokio, which will not build for SBF.

Add your own instructions freely (custom lobby metadata, wagers, NFTs, matchmaking). DeFORM
only requires the ones on `GameProgramClient`.

## Build and client generation

```sh
cd my-game-program                 # wherever you copied the template
anchor build                       # add `-- --no-default-features --features <game>` if you kept the gating
yarn install                       # once: codama and its renderers
yarn generate ../path/to/my_game   # codama → <that crate>/src/generated/
```

`generate-client.mjs` reads `target/idl/anchor_program.json` and writes the Rust client into
whatever directory you pass (defaulting to pong's). Point it at **your game crate** — the
generated module lands in its `src/generated/`, which is what your `GameProgramClient` impl
builds on. `build_pong.sh` is the two-line example of both steps.

Run both after **any** change to the program's instructions or arg types.

Codama emits instruction builders, borsh arg structs, discriminators, error enums, and the
program ID constant into `src/generated/` (**do not edit — regenerated in place**).

What codama does **not** generate: account decoders and PDA derivation for `Lobby` and
`InputsAccount`. Those are wincode-encoded and not annotated with `#[account]`, so they never
appear in the IDL. Use `DeformAccount::from_bytes` and the `find_program_address` helpers on
`Lobby<T>` / `InputsAccount<T>` instead. `examples/pong/src/main.rs` (`fetch_lobbies`) shows
scanning all lobbies with a `Memcmp` filter on the wincode-serialized discriminator.

## Deploying

- Localhost: the pong example runs surfpool (base layer) + a MagicBlock ephemeral validator
  in docker-compose. **surfpool must fork devnet** (`start --no-tui --network devnet`) —
  the ER's fee-vault startup checks pass there because MagicBlock's validator identity is a
  clean funded account on devnet, but not on mainnet-beta. If you copy
  `examples/pong/docker/localhost/` as your own dev stack, note every volume mount in it is
  relative to that directory's position in the DeFORM repo (`../../../../../surfpool/`,
  `../../../../../magicblock/config.toml`, the admin keypair) — all of them need repointing,
  and the `magicblock/` and `surfpool/` directories are themselves part of the repo.
- Devnet/mainnet: deploy normally with `anchor deploy`, then pick the matching
  `ValidatorNetwork` variant so `er_endpoints()` and `address()` resolve to that region's
  ephemeral rollup.
- `ValidatorNetwork::er_endpoints()` returns sane defaults per region. The router's
  `getDelegationStatus` is the real source of truth for which validator currently holds a
  delegated account.

## Reading lobbies from the chain

```rust
let data = rpc.get_account_data(&lobby_pda)?;
match DeformAccount::<MyGame>::from_bytes(&data)? {
    DeformAccount::Lobby(lobby) => { /* feed into a backend constructor */ }
    DeformAccount::Inputs(_)    => { /* wrong account */ }
}
```

This `Lobby<T>` is exactly what `new_quic_client` / `new_foc_client` want. `Lobby<T>` and
`LobbyState<T>` are `serde::Serialize`, so dumping one as JSON for debugging is one call.
