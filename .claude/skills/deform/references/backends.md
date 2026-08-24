# Backends

A backend owns the authoritative-state relationship: it runs the local prediction loop,
talks to whatever the authority is, applies rollbacks, and publishes a smoothed state into
`DeformSharedBackendState`. The game only ever sees `DeformClient<T>`.

## Start with offline. Always.

`deform_offline` is not just a demo mode — it is the recommended **first** integration step
and the fastest debugging loop you have.

1. It exercises the entire trait surface (`new_from_lobby`, `new_game_from_lobby`,
   `advance_frame`, `has_ended`, the smoother) with zero infrastructure: no keypair, no
   validator, no server, no Docker, no deployed program.
2. It runs the same fixed-tick + visual-tick loop as the networked backends, so tick-rate
   bugs, smoothing configuration, and rendering wiring all surface here.
3. It is deterministic and single-process, so a desync you see here is a bug in *your*
   `advance_frame`, not in the network.

Get the game fully playable against a bot offline. Only then add a backend that involves a
network. When you swap, no game logic or rendering code changes — just the constructor call.

```rust
let mut player_status = BTreeMap::new();
player_status.insert(me, PlayerStatus::Ready);
player_status.insert(bot, PlayerStatus::Ready);

let lobby = Lobby {
    metadata: LobbyMetadata { id: 0, creator: me, network: Network::Web2, bump: 0 },
    state: LobbyState::NotStarted(LobbyNotStarted { player_status }),
};

let token = CancellationToken::new();
let client = deform_offline::new_offline_client::<MiniPong>(
    me,
    lobby,
    my_bot_fn,          // fn(&T::GameState, &Pubkey, &T::Inputs) -> T::Inputs
    visual_tick_micros, // your render rate
    token.clone(),
)?;
```

The lobby is fabricated in memory — nothing touches the chain, and `Network::Web2` here just
means "no delegation". Every non-local player is driven by `bot_fn`, which receives the
current state, that player's pubkey, and their previous inputs (so a bot can have hysteresis
— see `pong_bot` in `pong_logic.rs`).

Offline never rolls back (there is nothing to disagree with), so it does not exercise
rollback paths. It still runs the smoother, so interpolation is testable.

## Choosing a networked backend

| | `deform_quic` | `deform_foc` |
| --- | --- | --- |
| Authority | your QUIC server | the ephemeral rollup running your program |
| Latency | good — UDP datagrams, ~1 RTT | worse — bounded by slot time (~50 ms) + tx inclusion |
| Trust | you (the server operator) | the validator / chain |
| Infra you run | a server binary | none beyond deploying the program |
| Transport | QUIC: reliable stream for control, datagrams for state/inputs | WS `accountSubscribe` in, `set_inputs` txs out |
| Tick rate | free choice | **must equal the ER slot time** |

Fully on-chain is the interesting one but currently lags too much for twitch gameplay with
today's technology; the QUIC path exists so the same game is shippable now. Both settle
results on-chain, so lobbies and outcomes are on-chain either way.

## `deform_quic` — client

```rust
let client = deform_quic::new_quic_client::<MyQuicLogic>(
    server_addr,        // "127.0.0.1:4433"
    server_name,        // TLS SNI name
    lobby,              // fetched from chain
    me,
    skip_cert_verify,   // true for self-signed dev certs
    visual_tick_micros,
    auth,               // Q::Auth value
    cancellation_token,
)?;
```

Note the type parameter is the *server* logic type `Q: DeformQuicLogic`, not the user logic —
`Q::UserLogic` is reached through it. This is deliberate: one server logic is bound to
exactly one game, so you never write `<Q, U>` anywhere.

Protocol shape (`deform_quic/src/lib.rs`), if you need to interop:
- ALPN `b"deform/1"`.
- One bidirectional **reliable** control stream opened at connect, carrying
  `ReliableMessage<Q>`: `Identification` → `Authorized` | `Error`, then `Finish(lobby)` at
  match end, plus your `Custom(Q::CustomReliableMessage)` in either direction. Length-prefixed
  (`u32` LE), capped at 4096 bytes.
- **Datagrams** (unreliable) for the hot path: client → server
  `UnreliableServerInstruction::BatchSetInputs(HashMap<u64, Inputs>)`, server → client
  `UnreliableServerResponse { lobby_state }`. Inputs are re-sent in batches keyed by tick, so
  a dropped datagram self-heals on the next one.

## `deform_quic::server` — the web2 server

This is where the second trait lives.

```rust
pub trait DeformQuicLogic: Clone + Sized + Debug + Send + Sync + 'static {
    type CustomReliableMessage: SchemaRead + SchemaWrite + Clone + Debug + Send + Sync;
    type Auth:                  SchemaRead + SchemaWrite + Clone + Debug + Send + Sync;
    type UserLogic:  DeformUserLogic;
    type ProgramClient: GameProgramClient<Self::UserLogic>;

    fn authorize_connection(
        identification: &UserIdentification<Self>,
    ) -> Result<(), <Self::UserLogic as DeformUserLogic>::Error>;

    // defaulted
    fn on_match_end(&self, lobby, rpc_client, admin: &Keypair, program_client)
        -> impl Future<Output = UserFacingResult<Self::UserLogic>> + Send;
}
```

`DeformQuicLogic` binds a game (`UserLogic`) to its on-chain instruction builder
(`ProgramClient`) and adds the two server-only concerns:

- **`Auth`** — arbitrary payload the client sends in `UserIdentification { user, lobby_id, auth }`.
  `authorize_connection` decides: `Ok(())` sends `ReliableMessage::Authorized`; `Err(e)` sends
  the error and closes. Use it for signature challenges, session tokens, ticketing, ban lists.
  Use a unit struct (`NoAuth` in pong) to accept everyone.
- **`CustomReliableMessage`** — your own out-of-band messages on the reliable stream (chat,
  emotes, spectator control). DeFORM just carries them.
- **`on_match_end`** — runs after `has_ended()` flips, before the match is dropped. The
  **default implementation already settles on-chain**: it builds
  `GameProgramClient::write_and_close_ix` and retries `send_and_confirm` up to 10 times with
  400 ms backoff. Override it to add leaderboards, payouts, webhooks, or a different
  settlement policy. Returning `Err` broadcasts to clients; the match is removed either way.

The minimal server, end to end (`examples/pong/src/server.rs`):

```rust
let mut server = DeformQuicServer::<PongQuicLogic>::new_with_defaults(
    &AuthConfig::DebugConfig,   // self-signed cert; ProdConfig { certs_pem_file, key_pem_file }
    rpc_client,                 // Arc<nonblocking RpcClient> — base layer
    admin_keypair,              // Arc<Keypair> — signs write_and_close
    PongQuicLogic,
    PongAnchorClient,
)?;
server.addr = format!("0.0.0.0:{port}").parse()?;

tokio::runtime::Builder::new_multi_thread().enable_all().build()?
    .block_on(server.init_server())?;
```

Tunable fields on `DeformQuicServer` (all public, set them after `new_with_defaults`):

| Field | Default | Purpose |
| --- | --- | --- |
| `addr` | `0.0.0.0:443` | UDP bind address |
| `max_conn_per_ip` | `5` | per-IP connection cap, enforced in the accept loop |
| `match_config` | `WaitForTimeout(10s)` | or `WaitForAllPlayers` — when a match starts |
| `quinn_config` | defaults + `max_incoming(1024)`, 20 s keepalive, 16 KiB datagram recv buffer | raw `quinn::ServerConfig`; `apply_custom_quinn_defaults()` re-applies DeFORM's |

`init_server()` registers SIGINT/SIGTERM handlers and drains: it stops accepting, waits for
in-flight matches to finish, then returns.

## `deform_foc` — fully on-chain client

```rust
let endpoints = validator_network.er_endpoints();
let slot_time_micros = <MyGame as DeformUserLogic>::get_micros_per_slot(validator_network);

let client = deform_foc::new_foc_client::<MyFocLogic>(
    endpoints.rpc.to_string(),
    endpoints.ws.to_string(),
    Arc::new(keypair),
    MyProgramClient,
    lobby,                 // must be a FullyOnChain lobby, already started + delegated
    visual_tick_micros,
    slot_time_micros,
    cancellation_token,
)?;
```

`DeformFocLogic` is the FoC analogue of `DeformQuicLogic`, minus the server concepts:

```rust
pub trait DeformFocLogic: Clone + Sized + Debug + Send + Sync + 'static {
    type UserLogic: DeformUserLogic;
    type ProgramClient: GameProgramClient<Self::UserLogic>;
}
```

How it works:
- A raw WebSocket to the ER carries `accountSubscribe` on the lobby PDA; each notification is
  base64-decoded and wincode-deserialized into `LobbyState<T>` — that is the authoritative
  feed, replacing the QUIC server's datagrams.
- Local inputs are batched and sent as `set_inputs` transactions built by your
  `GameProgramClient`. A dedicated commit task owns the RPC send path behind a bounded
  channel (64), so slow sends backpressure the sim loop instead of piling up.
- The chain advances the game via the **crank**: a recurring `tick` task scheduled on the ER
  (see `references/onchain.md`). The FoC client does not drive the authority; it only reads
  it and pushes inputs.

**Hard constraint**: `TICK_RATE_MICROS` must equal `slot_time_micros`, or `new_foc_client`
returns `DeformError::TickRateMissmatch`. On-chain the game runs at the validator's clock,
so there is no freedom here — pick a tick rate that divides the slot time (20 Hz / 50 000 µs
is the working default).

### Pacing signal

Two `accountSubscribe` sockets: one on the lobby PDA for authoritative states, one on the
player's own inputs PDA. The `tick` instruction removes each input as it consumes it, so
the inputs account's length is how much the chain still has queued for ticks it has not
run — the same signal the QUIC server reports explicitly, and what drives time dilation
(`references/netcode.md`).

Latency falls out of the same subscription: a commit is timed from when it was sent to
when it appears in the account, which feeds the match-start burst and `stats.ping_ms`.

## Writing another backend

There is no `Backend` trait to implement — a backend is anything that returns a
`DeformClient<T>`. The contract is:

1. Take a `Lobby<T>`, a `CancellationToken`, and `visual_tick_micros`.
2. Promote a `NotStarted` lobby to a fresh `Ongoing` at tick 0 (call `new_from_lobby` +
   `new_game_from_lobby`) so there is something to predict from.
3. Spawn a thread with its own tokio runtime; signal setup success/failure back over a
   `oneshot` so the constructor can return synchronously.
4. Run two tickers: the simulation at `TICK_RATE_MICROS`, and a visual ticker at
   `visual_tick_micros` that writes the smoothed state into the shared mutex.
5. Honour the cancellation token in every select arm.
6. On a fatal error, write it to `backend_state.internal_error`.

`deform_offline/src/client.rs` (~270 lines) is the readable template; `deform_foc/src/client.rs`
is the same shape with reconciliation added.
